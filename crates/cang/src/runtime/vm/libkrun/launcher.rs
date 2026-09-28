//! Direct libkrun launch sequencing from a prepared `LaunchConfig`.

use anyhow::{Context, Result, anyhow};
#[cfg(test)]
use std::cell::RefCell;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::runtime::launch::config::{LaunchConfig, NetworkMode};
use crate::runtime::publish::tsi_port_forwards;
use crate::runtime::seccomp::{self, SeccompMode};
use crate::runtime::session::supervisor::rlimits::host_nofile_hard_limit;
use crate::runtime::vm::gpu::GpuMode;

use cang_libkrun::{CANG_FS_ROOT_TAG, CANG_LIBKRUN_COMPAT_NET_FEATURES, Handle, LibkrunApi};

pub(in crate::runtime::vm::libkrun) const PROFILE_KERNEL_CMDLINE_APPEND: &str =
    "ignore_loglevel loglevel=7 printk.time=1 initcall_debug";
/// `KRUN_TSI_FLAGS_HIJACK_INET`: the guest's TCP/UDP traffic is hijacked to the
/// host when no virtio-net device is configured.
const CANG_TSI_FLAG_HIJACK_INET: u32 = 1;
const CANG_VSOCK_GUEST_CID: u64 = 3;
const CANG_NET_DEVICE_ID: &str = "net0";
const CANG_PASST_MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];
const VIRGLRENDERER_USE_EGL: u32 = 1 << 0;
const VIRGLRENDERER_THREAD_SYNC: u32 = 1 << 1;
const VIRGLRENDERER_VENUS: u32 = 1 << 6;
const VIRGLRENDERER_RENDER_SERVER: u32 = 1 << 9;
const VIRGLRENDERER_DRM: u32 = 1 << 10;
const VIRGLRENDERER_USE_VIDEO: u32 = 1 << 11;
// Venus Vulkan renderer via an external render server. Proven against the L1
// virtio-gpu DRM capset: the guest sees a hardware-backed RADV venus device.
// The venus renderer runs in the sandboxed render server (RENDER_SERVER is
// respected by venus but ignored by virgl), while vrend runs in-process for
// GL and VA-API video (USE_EGL + USE_VIDEO with a get_drm_fd callback).
// THREAD_SYNC enables a background vrend-sync thread that retires GL fences
// independently of the guest's command submission. This is required for video
// decode (which blocks synchronously on the decode fence) — without it, the
// fence is only checked once at the end of submit and never again.
const VIRGLRENDERER_VENUS_FLAGS: u32 = VIRGLRENDERER_USE_EGL
    | VIRGLRENDERER_THREAD_SYNC
    | VIRGLRENDERER_VENUS
    | VIRGLRENDERER_RENDER_SERVER
    | VIRGLRENDERER_DRM
    | VIRGLRENDERER_USE_VIDEO;
const GPU_SHM_SIZE_BYTES: u64 = 256 * 1024 * 1024;
/// Env var the supervisor sets on the VM worker carrying the parent end of the
/// SOCK_SEQPACKET socketpair connected to the render-server runner.
pub(crate) const RENDER_SERVER_FD_ENV: &str = "CANG_RENDER_SERVER_FD";

#[cfg(test)]
type AuditStartMarkerHook = Box<dyn FnMut() -> Result<()>>;

#[cfg(test)]
thread_local! {
    static AUDIT_START_MARKER_HOOK: RefCell<Option<AuditStartMarkerHook>> =
        RefCell::new(None);
}

#[cfg(test)]
pub(in crate::runtime) fn with_audit_start_marker_hook_for_test<T>(
    hook: impl FnMut() -> Result<()> + 'static,
    action: impl FnOnce() -> T,
) -> T {
    struct MarkerHookGuard;

    impl Drop for MarkerHookGuard {
        fn drop(&mut self) {
            AUDIT_START_MARKER_HOOK.with(|hook| {
                *hook.borrow_mut() = None;
            });
        }
    }

    AUDIT_START_MARKER_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "nested seccomp audit marker hooks are not supported"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let _guard = MarkerHookGuard;
    action()
}

#[derive(Debug)]
pub(in crate::runtime) struct DirectLibkrunLauncher<A> {
    api: A,
}

impl<A: LibkrunApi> DirectLibkrunLauncher<A> {
    pub(in crate::runtime) fn new(api: A) -> Self {
        Self { api }
    }

    #[cfg(test)]
    pub(in crate::runtime) fn start_enter(self, config: &LaunchConfig) -> Result<()> {
        self.start_enter_with_pre_enter_hook(config, || Ok(()))
    }

    #[cfg(test)]
    pub(in crate::runtime) fn start_enter_with_pre_enter_hook(
        self,
        config: &LaunchConfig,
        before_start_enter: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.start_enter_profiled_with_pre_enter_hook(config, None, before_start_enter)
    }

    pub(in crate::runtime) fn start_enter_profiled_with_pre_enter_hook(
        self,
        config: &LaunchConfig,
        profile_path: Option<&Path>,
        before_start_enter: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let host_nofile_hard_limit = host_nofile_hard_limit()?;
        self.start_enter_profiled_with_nofile_hard_limit(
            config,
            profile_path,
            before_start_enter,
            host_nofile_hard_limit,
        )
    }

    #[cfg(test)]
    pub(in crate::runtime) fn start_enter_with_host_nofile_hard_limit(
        self,
        config: &LaunchConfig,
        host_nofile_hard_limit: libc::rlim_t,
    ) -> Result<()> {
        self.start_enter_profiled_with_nofile_hard_limit(
            config,
            None,
            || Ok(()),
            host_nofile_hard_limit,
        )
    }

    fn start_enter_profiled_with_nofile_hard_limit(
        mut self,
        config: &LaunchConfig,
        profile_path: Option<&Path>,
        before_start_enter: impl FnOnce() -> Result<()>,
        host_nofile_hard_limit: libc::rlim_t,
    ) -> Result<()> {
        tracing::debug!(
            level = ?config.log_level,
            libkrun_level = config.log_level.libkrun_level(),
            "libkrun log init: begin"
        );
        setup(
            "krun_init_log",
            self.api.init_log(config.log_level.libkrun_level()),
        )?;
        tracing::debug!("libkrun log init: complete");
        tracing::debug!("krun_vmm_builder_new: begin");
        let builder = setup("krun_vmm_builder_new", self.api.vmm_builder_new())?;
        tracing::debug!(builder, "krun_vmm_builder_new: complete");
        let mut builder = builder;
        let console_log =
            match self.configure(&mut builder, config, profile_path, host_nofile_hard_limit) {
                Ok(console_log) => console_log,
                Err(err) => {
                    let _ = self.api.vmm_builder_destroy(builder);
                    return Err(err);
                }
            };
        // The managed console's output file must stay open until the VMM has
        // duplicated the fd, which happens in `vmm_builder_build`.
        let _console_log = console_log;
        if let Err(err) =
            before_start_enter().and_then(|()| emit_audit_start_marker_for_launch(config))
        {
            let _ = self.api.vmm_builder_destroy(builder);
            return Err(err);
        }
        let vmm = setup(
            "krun_vmm_builder_build",
            self.api.vmm_builder_build(builder),
        )?;
        tracing::debug!(vmm, "krun_vmm_builder_build: complete");
        tracing::debug!(vmm, "krun_vmm_run: begin");
        self.api
            .vmm_run(vmm)
            .with_context(|| "libkrun start failed: krun_vmm_run")
    }

    /// Configure the payload, the init config, the devices and the VMM builder.
    ///
    /// Returns the managed guest's kernel-console file, which the caller must
    /// keep open until the VMM has duplicated its fd.
    fn configure(
        &mut self,
        builder: &mut Handle,
        config: &LaunchConfig,
        profile_path: Option<&Path>,
        host_nofile_hard_limit: libc::rlim_t,
    ) -> Result<Option<File>> {
        tracing::debug!(
            builder = *builder,
            vcpus = config.vcpus,
            ram_mib = config.ram_mib,
            "krun_vmm_builder_vcpus/ram_mib: begin"
        );
        *builder = setup(
            "krun_vmm_builder_vcpus",
            self.api.vmm_builder_vcpus(*builder, config.vcpus),
        )?;
        *builder = setup(
            "krun_vmm_builder_ram_mib",
            self.api.vmm_builder_ram_mib(*builder, config.ram_mib),
        )?;

        let devices = setup(
            "krun_mmio_device_manager_new",
            self.api.mmio_device_manager_new(),
        )?;

        self.configure_gpu(devices, config.gpu_mode)?;
        self.configure_nested_virt(builder)?;
        let console_log = self.configure_console(devices, config)?;

        let overlay = setup("krun_fs_overlay_new", self.api.fs_overlay_new())?;
        tracing::debug!(builder = *builder, "krun_payload_load_krunfw: begin");
        let payload = setup("krun_payload_load_krunfw", self.api.payload_load_krunfw())?;
        tracing::debug!(payload, "krun_payload_load_krunfw: complete");
        self.configure_init_config(overlay, payload, config, host_nofile_hard_limit)?;

        tracing::debug!(
            rootfs = %config.task_rootfs.display(),
            tag = CANG_FS_ROOT_TAG,
            "krun_fs_device_new: begin"
        );
        let rootfs = setup(
            "krun_fs_device_new",
            self.api
                .fs_device_new(CANG_FS_ROOT_TAG, &config.task_rootfs, false),
        )?;
        tracing::debug!(rootfs, "krun_fs_device_new: complete");
        setup(
            "krun_fs_device_set_overlay",
            self.api.fs_device_set_overlay(rootfs, overlay),
        )?;
        add_device(&mut self.api, devices, rootfs)?;

        for disk in &config.disks {
            tracing::debug!(
                disk_id = %disk.id,
                disk_path = %disk.path.display(),
                read_only = disk.read_only,
                "krun_block_device_new: begin"
            );
            let device = setup(
                "krun_block_device_new",
                self.api
                    .block_device_new(&disk.id, &disk.path, disk.read_only),
            )?;
            add_device(&mut self.api, devices, device)?;
            tracing::debug!(disk_id = %disk.id, "krun_block_device_new: complete");
        }

        self.configure_vsock(devices, config)?;
        self.configure_network(devices, config)?;
        self.configure_balloon(devices)?;

        if let Some(profile_path) = profile_path {
            tracing::debug!(profile_path = %profile_path.display(), "krun_vmm_builder_set_profile_path: begin");
            *builder = setup(
                "krun_vmm_builder_set_profile_path",
                self.api.set_profile_path(*builder, profile_path),
            )?;
            tracing::debug!(
                payload,
                fragment = PROFILE_KERNEL_CMDLINE_APPEND,
                "krun_payload_append_cmdline: begin"
            );
            self.api
                .payload_append_cmdline(payload, PROFILE_KERNEL_CMDLINE_APPEND)?;
            tracing::debug!(payload, "krun_payload_append_cmdline: complete");
        }

        *builder = setup(
            "krun_vmm_builder_payload",
            self.api.vmm_builder_payload(*builder, payload),
        )?;
        *builder = setup(
            "krun_vmm_builder_devices",
            self.api.vmm_builder_devices(*builder, devices),
        )?;
        Ok(console_log)
    }

    fn configure_gpu(&mut self, devices: Handle, gpu_mode: GpuMode) -> Result<()> {
        match gpu_mode {
            GpuMode::Off => Ok(()),
            GpuMode::Drm => {
                let render_server_fd = std::env::var(RENDER_SERVER_FD_ENV)
                    .context("gpu mode drm requires a sandboxed render server runner")?
                    .parse::<i32>()
                    .context("render server fd env value is not a valid fd number")?;
                tracing::debug!(
                    virgl_flags = VIRGLRENDERER_VENUS_FLAGS,
                    shm_size = GPU_SHM_SIZE_BYTES,
                    render_server_fd,
                    "krun_gpu_device_new: begin"
                );
                let device = setup(
                    "krun_gpu_device_new",
                    self.api.gpu_device_new(
                        VIRGLRENDERER_VENUS_FLAGS,
                        GPU_SHM_SIZE_BYTES,
                        render_server_fd,
                    ),
                )?;
                add_device(&mut self.api, devices, device)?;
                tracing::debug!(device, "krun_gpu_device_new: complete");
                Ok(())
            }
        }
    }

    fn configure_nested_virt(&mut self, builder: &mut Handle) -> Result<()> {
        tracing::debug!("krun_check_nested_virt: begin");
        match self.api.check_nested_virt() {
            Ok(true) => {
                tracing::debug!("krun_check_nested_virt: host nested virtualization supported");
            }
            Ok(false) => {
                tracing::warn!(
                    "krun_check_nested_virt: host nested virtualization is not reported as supported; requesting nested virtualization anyway"
                );
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "krun_check_nested_virt: support check failed; requesting nested virtualization anyway"
                );
            }
        }
        tracing::debug!("krun_vmm_builder_nested_virt: begin");
        *builder = setup(
            "krun_vmm_builder_nested_virt",
            self.api.vmm_builder_nested_virt(*builder, true),
        )?;
        tracing::debug!("krun_vmm_builder_nested_virt: complete");
        Ok(())
    }

    /// The console is the guest's window onto its own kernel.
    ///
    /// A managed guest has no user attached, and the kernel reports OOM kills and
    /// panics only on its console, so a managed launch gets the extra leading
    /// console device whose output is a file the supervisor reads when the task
    /// ends (`console=hvc0` therefore lands there). The default console device,
    /// carrying the worker's stdio as `krun-stdin`/`krun-stdout`/`krun-stderr`
    /// ports, follows in both cases.
    fn configure_console(
        &mut self,
        devices: Handle,
        config: &LaunchConfig,
    ) -> Result<Option<File>> {
        let console_log = match &config.managed_session {
            Some(managed) => {
                let console_log =
                    File::create(&managed.guest_kernel_console_log).with_context(|| {
                        format!(
                            "failed to create the managed guest kernel console log '{}'",
                            managed.guest_kernel_console_log.display()
                        )
                    })?;
                tracing::debug!(
                    output = %managed.guest_kernel_console_log.display(),
                    "krun_console_device_builder: managed kernel console begin"
                );
                let builder = setup(
                    "krun_console_device_builder",
                    self.api.console_device_builder(),
                )?;
                setup(
                    "krun_console_builder_add_inout_port",
                    self.api.console_builder_add_inout_port(
                        builder,
                        "",
                        None,
                        Some(console_log.as_raw_fd()),
                    ),
                )?;
                let device = setup(
                    "krun_console_builder_build",
                    self.api.console_builder_build(builder),
                )?;
                add_device(&mut self.api, devices, device)?;
                tracing::debug!("krun_console_device_builder: managed kernel console complete");
                Some(console_log)
            }
            None => None,
        };
        tracing::debug!("krun_console_device_builder: default console begin");
        let builder = setup(
            "krun_console_device_builder",
            self.api.console_device_builder(),
        )?;
        setup(
            "krun_console_builder_add_default_console",
            self.api
                .console_builder_add_default_console(builder, 0, 1, 2),
        )?;
        let device = setup(
            "krun_console_builder_build",
            self.api.console_builder_build(builder),
        )?;
        add_device(&mut self.api, devices, device)?;
        tracing::debug!("krun_console_device_builder: default console complete");
        Ok(console_log)
    }

    /// Build the init config that the guest init blob turns into the guest's
    /// PID 1 and apply it to the overlay and the payload.
    ///
    /// ABI 2 has no `krun_set_exec`/`set_workdir`/`set_rlimits`: the init config
    /// owns the workload's argv, environment, workdir and resource limits, and
    /// DHCP is the init-side replacement for the old net device flag.
    fn configure_init_config(
        &mut self,
        overlay: Handle,
        payload: Handle,
        config: &LaunchConfig,
        host_nofile_hard_limit: libc::rlim_t,
    ) -> Result<()> {
        let mut init = setup("krun_init_config_builder", self.api.init_config_builder())?;
        let mut exec_args = Vec::with_capacity(config.argv.len() + 1);
        exec_args.push(config.exec_path.clone());
        exec_args.extend(config.argv.iter().cloned());
        let mut env = config.env.clone();
        env.extend(config.guest_config_env.iter().cloned());
        tracing::debug!(
            exec_path = %config.exec_path,
            argv_len = config.argv.len(),
            env_len = env.len(),
            workdir = %config.workdir,
            "krun_init_builder_args/env/workdir: begin"
        );
        init = setup(
            "krun_init_builder_args",
            self.api.init_builder_args(init, &exec_args),
        )?;
        init = setup(
            "krun_init_builder_env",
            self.api.init_builder_env(init, &env),
        )?;
        init = setup(
            "krun_init_builder_workdir",
            self.api.init_builder_workdir(init, &config.workdir),
        )?;
        let guest_nofile_rlimit = guest_nofile_rlimit_entry(host_nofile_hard_limit);
        tracing::debug!(
            host_nofile_hard_limit,
            rlimit = %guest_nofile_rlimit,
            "krun_init_builder_rlimits: begin"
        );
        init = setup(
            "krun_init_builder_rlimits",
            self.api.init_builder_rlimits(init, &[guest_nofile_rlimit]),
        )?;
        if config.network_mode == NetworkMode::Passt {
            // ABI 2's net device ignores the old DHCP flag; the init blob runs the
            // DHCP client when the config asks for it.
            tracing::debug!("krun_init_builder_dhcp: begin");
            init = setup(
                "krun_init_builder_dhcp",
                self.api.init_builder_dhcp(init, true),
            )?;
            tracing::debug!("krun_init_builder_dhcp: complete");
        }
        let init_config = setup("krun_init_builder_build", self.api.init_builder_build(init))?;
        tracing::debug!(init_config, "krun_init_config_apply_in: begin");
        setup(
            "krun_init_config_apply_in",
            self.api.init_config_apply_in(init_config, overlay, payload),
        )?;
        tracing::debug!(init_config, "krun_init_config_apply_in: complete");
        Ok(())
    }

    /// One vsock device carries cang's host channels: TSI port forwards in TSI
    /// mode and every unix-socket port (pulse, waypipe, exec, attach).
    fn configure_vsock(&mut self, devices: Handle, config: &LaunchConfig) -> Result<()> {
        let needs_vsock = config.network_mode == NetworkMode::Tsi
            || config.pulse_bridge.is_some()
            || config.waypipe.is_some()
            || config.exec.is_some()
            || config.managed_session.is_some();
        if !needs_vsock {
            return Ok(());
        }
        let tsi_features = match config.network_mode {
            NetworkMode::Tsi => CANG_TSI_FLAG_HIJACK_INET,
            NetworkMode::Passt => 0,
        };
        tracing::debug!(
            guest_cid = CANG_VSOCK_GUEST_CID,
            tsi_features,
            "krun_vsock_device_new: begin"
        );
        let vsock = setup(
            "krun_vsock_device_new",
            self.api
                .vsock_device_new(CANG_VSOCK_GUEST_CID, tsi_features),
        )?;
        tracing::debug!(vsock, "krun_vsock_device_new: complete");
        if let Some(pulse_bridge) = &config.pulse_bridge {
            tracing::debug!(
                guest_port = pulse_bridge.guest_port,
                socket = %pulse_bridge.socket.display(),
                "krun_vsock_device_add_unix_port: Pulse bridge begin"
            );
            setup(
                "krun_vsock_device_add_unix_port Pulse bridge",
                self.api.vsock_device_add_unix_port(
                    vsock,
                    pulse_bridge.guest_port,
                    &pulse_bridge.socket,
                    false,
                ),
            )?;
            tracing::debug!("krun_vsock_device_add_unix_port: Pulse bridge registered");
        }
        if let Some(waypipe) = &config.waypipe {
            tracing::debug!(
                guest_port = waypipe.guest_port,
                socket = %waypipe.socket.display(),
                "krun_vsock_device_add_unix_port: Waypipe begin"
            );
            setup(
                "krun_vsock_device_add_unix_port Waypipe",
                self.api.vsock_device_add_unix_port(
                    vsock,
                    waypipe.guest_port,
                    &waypipe.socket,
                    false,
                ),
            )?;
            tracing::debug!("krun_vsock_device_add_unix_port: Waypipe registered");
        }
        if let Some(exec) = &config.exec {
            tracing::debug!(
                guest_port = exec.guest_port,
                socket = %exec.socket.display(),
                "krun_vsock_device_add_unix_port: exec begin"
            );
            setup(
                "krun_vsock_device_add_unix_port exec",
                self.api
                    .vsock_device_add_unix_port(vsock, exec.guest_port, &exec.socket, true),
            )?;
            tracing::debug!("krun_vsock_device_add_unix_port: exec registered");
        }
        if let Some(managed) = &config.managed_session {
            tracing::debug!(
                guest_port = managed.guest_port,
                socket = %managed.attach_socket.display(),
                "krun_vsock_device_add_unix_port: managed attach begin"
            );
            setup(
                "krun_vsock_device_add_unix_port",
                self.api.vsock_device_add_unix_port(
                    vsock,
                    managed.guest_port,
                    &managed.attach_socket,
                    true,
                ),
            )?;
            tracing::debug!("krun_vsock_device_add_unix_port: managed attach registered");
        }
        if config.network_mode == NetworkMode::Tsi && !config.publish.is_empty() {
            let forwards =
                tsi_port_forwards(&config.publish).context("libkrun TSI publish setup failed")?;
            tracing::debug!(ports = ?forwards, "krun_vsock_device_add_port_forward: begin");
            for mapping in forwards {
                setup(
                    "krun_vsock_device_add_port_forward",
                    self.api.vsock_device_add_port_forward(vsock, &mapping),
                )?;
            }
            tracing::debug!("krun_vsock_device_add_port_forward: complete");
        }
        // ABI 2's device manager takes ownership of the device, so every port has
        // to be registered on it first.
        add_device(&mut self.api, devices, vsock)?;
        Ok(())
    }

    /// Attach the virtio-balloon so the host can reclaim guest memory the guest
    /// has finished with.
    ///
    /// This does not make `ram_mib` growable: libkrun implements only the
    /// balloon's free-page-reporting queue, and the inflate/deflate queues it
    /// would need are stubs, as is the guest kernel's virtio-mem. What it does
    /// buy is that the VMM `madvise`s the guest RAM mapping where the guest
    /// reported freed pages, instead of holding every page the guest has ever
    /// touched until the VM exits.
    ///
    /// It is attached last on purpose: the MMIO device manager numbers devices
    /// (and their IRQs) in registration order, and the guest's boot contract
    /// already depends on that order - `console=hvc0` and the tagged root
    /// filesystem. Appending the balloon leaves those indices where they are.
    fn configure_balloon(&mut self, devices: Handle) -> Result<()> {
        tracing::debug!("krun_balloon_device_new: begin");
        let device = setup("krun_balloon_device_new", self.api.balloon_device_new())?;
        add_device(&mut self.api, devices, device)?;
        tracing::debug!(device, "krun_balloon_device_new: complete");
        Ok(())
    }

    fn configure_network(&mut self, devices: Handle, config: &LaunchConfig) -> Result<()> {
        if config.network_mode != NetworkMode::Passt {
            return Ok(());
        }
        let passt_fd = config
            .passt_fd
            .ok_or_else(|| anyhow!("libkrun passt setup requires a prepared passt socket fd"))?;
        tracing::debug!(fd = passt_fd, "krun_net_device_new_unixstream_fd: begin");
        let device = setup(
            "krun_net_device_new_unixstream_fd",
            self.api.net_device_new_unixstream_fd(
                CANG_NET_DEVICE_ID,
                passt_fd,
                CANG_PASST_MAC,
                CANG_LIBKRUN_COMPAT_NET_FEATURES,
                0,
            ),
        )?;
        add_device(&mut self.api, devices, device)?;
        tracing::debug!(device, "krun_net_device_new_unixstream_fd: complete");
        Ok(())
    }
}

fn setup<T>(name: &str, result: Result<T>) -> Result<T> {
    result.with_context(|| format!("libkrun setup failed: {name}"))
}

fn add_device(api: &mut impl LibkrunApi, devices: Handle, device: Handle) -> Result<()> {
    setup(
        "krun_mmio_device_manager_add",
        api.mmio_device_manager_add(devices, device),
    )
}

fn emit_audit_start_marker_for_launch(config: &LaunchConfig) -> Result<()> {
    if !matches!(config.seccomp, SeccompMode::Audit(_)) {
        return Ok(());
    }

    #[cfg(test)]
    {
        let hook_result = AUDIT_START_MARKER_HOOK.with(|slot| {
            let mut hook = slot.borrow_mut();
            hook.as_mut().map(|hook| hook())
        });
        if let Some(result) = hook_result {
            return result;
        }
    }

    seccomp::emit_audit_start_marker()
}

pub(in crate::runtime::vm::libkrun) fn guest_nofile_rlimit_entry(
    host_nofile_hard_limit: libc::rlim_t,
) -> String {
    // libkrun expects Linux resource numeric IDs in RESOURCE=RLIM_CUR:RLIM_MAX form.
    format!(
        "{}={}:{}",
        libc::RLIMIT_NOFILE,
        host_nofile_hard_limit,
        host_nofile_hard_limit
    )
}
