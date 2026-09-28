use anyhow::Result;
use std::path::PathBuf;

mod codec;
mod components;
mod guest_env;
mod model;

pub(crate) use components::mounts::validate_mounts;
pub(crate) use components::resources::resolve_cpu_count;
pub(crate) use model::{
    AllocatorMode, BindMount, BindMountSourceKind, CANG_KRUN_CONFIG_PATH, CARGO_TAG, CARGO_TARGET,
    CODEX_TAG, CODEX_TARGET, CORTEXKIT_TAG, CORTEXKIT_TARGET, DEFAULT_PULSE_BRIDGE_PORT,
    DEFAULT_WAYPIPE_PORT, DIRGE_CONFIG_TAG, DIRGE_CONFIG_TARGET, DIRGE_DATA_TAG, DIRGE_DATA_TARGET,
    DIRGE_HOME_TAG, DIRGE_HOME_TARGET, DiskAttachment, ExecConfig, GuestInitOverrideMount,
    GuestPermissions, HostNixOverlay, LaunchConfig, LaunchSpec, MANAGED_GUEST_KERNEL_CONSOLE_LOG,
    ManagedSessionConfig, NIX_TARGET, NetworkMode, OMP_TAG, OMP_TARGET, PI_TAG, PI_TARGET,
    PulseBridgeConfig, PulseServer, SCCACHE_TAG, SCCACHE_TARGET, WORKSPACE_TAG, WORKSPACE_TARGET,
    WaypipeConfig, canonical_mount_target,
};

#[cfg(test)]
use self::codec::{decode_text_for_debug, push_field};
#[cfg(test)]
use self::components::resources::{default_ram_mib_from_meminfo, resolve_ram_mib};

impl LaunchConfig {
    /// Build the serialized helper/libkrun launch contract from explicit contributors.
    pub(crate) fn build_for_task(spec: LaunchSpec<'_>) -> Result<Self> {
        let ram_mib = components::resources::resolve_ram_mib(spec.mem_gib)?;
        let mounts = components::mounts::mounts_with_host_nix_overlay(
            spec.mounts,
            spec.host_nix_overlay.as_ref(),
        )?;
        if let Some(mount) = &spec.guest_init_override {
            components::guest_init::validate_guest_init_override_mount(
                mount,
                spec.guest_init_exec,
            )?;
        }

        let mut guest_config_env = guest_env::bootstrap_env(
            &spec.image_process_config.env,
            components::identity::required_env(spec.host_uid, spec.host_gid),
        )?;
        if spec.root {
            guest_env::insert_env(&mut guest_config_env, model::ENTER_AS_ROOT_ENV, "1");
        }
        if spec.profile {
            guest_env::insert_env(&mut guest_config_env, model::GUEST_PROFILE_ENV, "1");
        }
        if spec.log_level.enables_debug() {
            guest_env::insert_env(&mut guest_config_env, model::GUEST_DEBUG_ENV, "1");
        }
        guest_env::insert_env(
            &mut guest_config_env,
            model::NIX_ALLOCATOR_ENV,
            spec.allocator.as_env_value(),
        );
        components::network::contribute_guest_env(&mut guest_config_env, spec.network_mode);
        if let Some(pulse) = spec.pulse
            && let Some(value) = pulse.direct_env_value()
        {
            guest_env::insert_env(&mut guest_config_env, model::GUEST_PULSE_SERVER_ENV, &value);
        }
        if let Some(pulse_bridge) = &spec.pulse_bridge {
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_PULSE_BRIDGE_PORT_ENV,
                &pulse_bridge.guest_port.to_string(),
            );
        }
        if spec.gpu_mode == crate::runtime::vm::gpu::GpuMode::Drm {
            guest_env::insert_env(&mut guest_config_env, model::GUEST_GPU_DRM_ENV, "1");
        }
        if spec.wayland {
            guest_env::insert_env(&mut guest_config_env, model::GUEST_WAYLAND_ENV, "1");
        }
        if !spec.new_perms.is_empty() {
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_PERMISSIONS_ENV,
                &spec.new_perms.to_string(),
            );
        }
        if let Some(waypipe) = &spec.waypipe {
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_WAYPIPE_PORT_ENV,
                &waypipe.guest_port.to_string(),
            );
        }
        if let Some(exec) = &spec.exec {
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_EXEC_PORT_ENV,
                &exec.guest_port.to_string(),
            );
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_EXEC_PROTOCOL_VERSION_ENV,
                &exec.protocol_version.to_string(),
            );
        }
        if let Some(managed) = &spec.managed_session {
            guest_env::insert_env(&mut guest_config_env, model::GUEST_SESSION_MANAGED_ENV, "1");
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_ATTACH_PORT_ENV,
                &managed.guest_port.to_string(),
            );
            guest_env::insert_env(
                &mut guest_config_env,
                model::GUEST_ATTACH_PROTOCOL_VERSION_ENV,
                &managed.protocol_version.to_string(),
            );
        }
        for (key, value) in spec.extra_env {
            guest_config_env.insert(key, value);
        }

        Ok(Self {
            task_rootfs: spec.task_rootfs.to_path_buf(),
            hostname: spec.hostname.to_owned(),
            mounts,
            host_nix_overlay: spec.host_nix_overlay,
            guest_init_override: spec.guest_init_override,
            disks: spec.disks,
            ram_mib,
            vcpus: spec.vcpus,
            log_level: spec.log_level,
            network_mode: spec.network_mode,
            gpu_mode: spec.gpu_mode,
            zero_copy_shm: spec.zero_copy_shm,
            new_perms: spec.new_perms,
            publish: spec.publish.to_vec(),
            workdir: components::process::workdir_from_image(
                spec.image_process_config.working_dir.as_deref(),
            ),
            exec_path: spec.guest_init_exec.to_owned(),
            argv: components::process::guest_init_argv(
                spec.guest_command,
                &spec.image_process_config.cmd,
            ),
            env: vec![(
                model::KRUN_CONFIG_ENV.to_owned(),
                CANG_KRUN_CONFIG_PATH.to_owned(),
            )],
            guest_config_env: guest_config_env.into_iter().collect(),
            passt_fd: None,
            pulse_bridge: spec.pulse_bridge,
            waypipe: spec.waypipe,
            exec: spec.exec,
            managed_session: spec.managed_session,
            seccomp: Default::default(),
            landlock: Default::default(),
        })
    }

    pub(crate) fn with_root_export(&self, root_export: PathBuf) -> Self {
        let mut config = self.clone();
        config.task_rootfs = root_export;
        config
    }

    pub(crate) fn with_passt_fd(&self, passt_fd: i32) -> Self {
        let mut config = self.clone();
        config.passt_fd = Some(passt_fd);
        config
    }

    pub(crate) fn is_managed_session(&self) -> bool {
        self.managed_session.is_some()
    }

    #[cfg(test)]
    pub(crate) fn guest_config_env_contains(&self, name: &str, value: &str) -> bool {
        self.guest_config_env
            .iter()
            .any(|(key, actual)| key == name && actual == value)
    }
}

#[cfg(test)]
mod tests;
