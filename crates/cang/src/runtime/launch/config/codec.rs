//! Hex-line launch-config serialization contract.

use anyhow::{Context, Result, anyhow};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::logging::LogLevel;
use crate::runtime::landlock::LandlockMode;
use crate::runtime::seccomp::SeccompMode;
use crate::runtime::vm::gpu::GpuMode;

use super::components::{guest_init, mounts};
use super::guest_env::guest_config_json;
use super::model::{
    BindMount, BindMountSourceKind, CANG_KRUN_CONFIG_PATH, DiskAttachment, ExecConfig,
    GuestInitOverrideMount, HostNixOverlay, LaunchConfig, ManagedSessionConfig, NetworkMode,
    PulseBridgeConfig, WaypipeConfig,
};

impl LaunchConfig {
    pub(crate) fn write_guest_config_to_rootfs(&self) -> Result<PathBuf> {
        let relative_path = CANG_KRUN_CONFIG_PATH
            .strip_prefix('/')
            .ok_or_else(|| anyhow!("cang krun config path must be absolute"))?;
        let path = self.task_rootfs.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create cang krun config dir '{}'",
                    parent.display()
                )
            })?;
        }
        fs::write(&path, guest_config_json(&self.guest_config_env))
            .with_context(|| format!("failed to write cang krun config '{}'", path.display()))?;
        Ok(path)
    }

    pub(crate) fn write_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create cang launch config dir '{}'",
                    parent.display()
                )
            })?;
        }
        fs::write(path, self.serialize())
            .with_context(|| format!("failed to write cang launch config '{}'", path.display()))
    }

    pub(crate) fn read_from(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read cang launch config '{}'", path.display()))?;
        Self::parse(&text)
    }

    pub(crate) fn decode_file_for_debug(path: &Path) -> Result<String> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read cang launch config '{}'", path.display()))?;
        decode_text_for_debug(&text)
    }

    pub(crate) fn serialize(&self) -> String {
        let mut out = String::new();
        push_field(
            &mut out,
            "task_rootfs",
            &self.task_rootfs.display().to_string(),
        );
        push_field(&mut out, "hostname", &self.hostname);
        if let Ok(workspace) = mounts::workspace_mount(&self.mounts) {
            push_field(
                &mut out,
                "workspace_source",
                &workspace.source.display().to_string(),
            );
            push_field(&mut out, "workspace_tag", &workspace.tag);
            push_field(&mut out, "workspace_target", &workspace.target);
        }
        for (index, mount) in self.mounts.iter().enumerate() {
            push_field(
                &mut out,
                &format!("mount.{index}.source"),
                &mount.source.display().to_string(),
            );
            push_field(&mut out, &format!("mount.{index}.tag"), &mount.tag);
            push_field(&mut out, &format!("mount.{index}.target"), &mount.target);
            push_field(
                &mut out,
                &format!("mount.{index}.source_kind"),
                mount.source_kind.as_config_value(),
            );
            push_field(
                &mut out,
                &format!("mount.{index}.read_only"),
                if mount.read_only { "true" } else { "false" },
            );
        }
        if let Some(mount) = &self.guest_init_override {
            push_field(
                &mut out,
                "guest_init_override_source",
                &mount.source.display().to_string(),
            );
            push_field(&mut out, "guest_init_override_target", &mount.target);
            push_field(
                &mut out,
                "guest_init_override_read_only",
                if mount.read_only { "true" } else { "false" },
            );
        }
        if let Some(overlay) = &self.host_nix_overlay {
            push_field(
                &mut out,
                "host_nix_overlay.selected_reference",
                &overlay.selected_reference,
            );
            push_field(
                &mut out,
                "host_nix_overlay.image_digest",
                &overlay.image_digest,
            );
            push_field(&mut out, "host_nix_overlay.digest_key", &overlay.digest_key);
            push_field(
                &mut out,
                "host_nix_overlay.lowerdir",
                &overlay.lowerdir.display().to_string(),
            );
            push_field(
                &mut out,
                "host_nix_overlay.upperdir",
                &overlay.upperdir.display().to_string(),
            );
            push_field(
                &mut out,
                "host_nix_overlay.workdir",
                &overlay.workdir.display().to_string(),
            );
            push_field(
                &mut out,
                "host_nix_overlay.mergeddir",
                &overlay.mergeddir.display().to_string(),
            );
        }
        push_field(&mut out, "ram_mib", &self.ram_mib.to_string());
        push_field(&mut out, "vcpus", &self.vcpus.to_string());
        push_field(&mut out, "log_level", self.log_level.as_str());
        push_field(
            &mut out,
            "network_mode",
            self.network_mode.as_config_value(),
        );
        push_field(&mut out, "gpu_mode", self.gpu_mode.as_config_value());
        if self.zero_copy_shm {
            push_field(&mut out, "zero_copy_shm", "1");
        }
        if !self.new_perms.is_empty() {
            push_field(&mut out, "permissions", &self.new_perms.to_string());
        }
        for (index, spec) in self.publish.iter().enumerate() {
            push_field(&mut out, &format!("publish.{index}"), spec);
        }
        push_field(&mut out, "workdir", &self.workdir);
        push_field(&mut out, "exec_path", &self.exec_path);
        if let Some(pulse_bridge) = &self.pulse_bridge {
            push_field(
                &mut out,
                "pulse_bridge.socket",
                &pulse_bridge.socket.display().to_string(),
            );
            push_field(
                &mut out,
                "pulse_bridge.guest_port",
                &pulse_bridge.guest_port.to_string(),
            );
            push_field(
                &mut out,
                "pulse_bridge.host_port",
                &pulse_bridge.host_port.to_string(),
            );
        }
        if let Some(waypipe) = &self.waypipe {
            push_field(
                &mut out,
                "waypipe.socket",
                &waypipe.socket.display().to_string(),
            );
            push_field(
                &mut out,
                "waypipe.guest_port",
                &waypipe.guest_port.to_string(),
            );
        }
        if let Some(exec) = &self.exec {
            push_field(&mut out, "exec.socket", &exec.socket.display().to_string());
            push_field(&mut out, "exec.guest_port", &exec.guest_port.to_string());
            push_field(
                &mut out,
                "exec.protocol_version",
                &exec.protocol_version.to_string(),
            );
            push_field(&mut out, "exec.socket_uid", &exec.socket_uid.to_string());
            push_field(&mut out, "exec.socket_gid", &exec.socket_gid.to_string());
        }
        if let Some(managed) = &self.managed_session {
            push_field(
                &mut out,
                "managed_session.attach_socket",
                &managed.attach_socket.display().to_string(),
            );
            push_field(
                &mut out,
                "managed_session.guest_port",
                &managed.guest_port.to_string(),
            );
            push_field(
                &mut out,
                "managed_session.protocol_version",
                &managed.protocol_version.to_string(),
            );
            push_field(
                &mut out,
                "managed_session.attach_socket_uid",
                &managed.attach_socket_uid.to_string(),
            );
            push_field(
                &mut out,
                "managed_session.attach_socket_gid",
                &managed.attach_socket_gid.to_string(),
            );
            push_field(
                &mut out,
                "managed_session.cleanup_task_rootfs_on_exit",
                if managed.cleanup_task_rootfs_on_exit {
                    "true"
                } else {
                    "false"
                },
            );
            push_field(
                &mut out,
                "managed_session.guest_kernel_console_log",
                &managed.guest_kernel_console_log.display().to_string(),
            );
        }
        push_field(&mut out, "seccomp.mode", self.seccomp.as_config_value());
        push_field(&mut out, "landlock.mode", self.landlock.as_config_value());
        if let Some(path) = self.seccomp.audit_trace_path() {
            push_field(
                &mut out,
                "seccomp.audit_trace_path",
                &path.display().to_string(),
            );
        }
        if let Some(path) = self.seccomp.audit_baseline_policy_path() {
            push_field(
                &mut out,
                "seccomp.audit_baseline_policy_path",
                &path.display().to_string(),
            );
        }
        if let Some(path) = self.seccomp.enforce_policy_path() {
            push_field(
                &mut out,
                "seccomp.enforce_policy_path",
                &path.display().to_string(),
            );
        }
        for (index, disk) in self.disks.iter().enumerate() {
            push_field(&mut out, &format!("disk.{index}.id"), &disk.id);
            push_field(
                &mut out,
                &format!("disk.{index}.path"),
                &disk.path.display().to_string(),
            );
            push_field(
                &mut out,
                &format!("disk.{index}.read_only"),
                if disk.read_only { "true" } else { "false" },
            );
        }
        for (index, arg) in self.argv.iter().enumerate() {
            push_field(&mut out, &format!("argv.{index}"), arg);
        }
        for (index, (key, value)) in self.env.iter().enumerate() {
            push_field(&mut out, &format!("env.{index}"), &format!("{key}={value}"));
        }
        for (index, (key, value)) in self.guest_config_env.iter().enumerate() {
            push_field(
                &mut out,
                &format!("guest_env.{index}"),
                &format!("{key}={value}"),
            );
        }
        out
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut fields = BTreeMap::new();
        let mut argv = BTreeMap::new();
        let mut env = BTreeMap::new();
        let mut guest_config_env = BTreeMap::new();
        let mut publish = BTreeMap::new();
        let mut mounts: BTreeMap<usize, PartialBindMount> = BTreeMap::new();
        let mut disks: BTreeMap<usize, PartialDiskAttachment> = BTreeMap::new();

        for (line_index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let (key, encoded) = line.split_once('=').ok_or_else(|| {
                anyhow!("cang launch config line {} is missing '='", line_index + 1)
            })?;
            let value = decode_hex(encoded).with_context(|| {
                format!("cang launch config line {} has invalid hex", line_index + 1)
            })?;
            if let Some(rest) = key.strip_prefix("mount.") {
                let (index, field) = rest.split_once('.').ok_or_else(|| {
                    anyhow!("cang launch config mount entry {key} is missing field")
                })?;
                let mount = mounts.entry(parse_index(key, index)?).or_default();
                match field {
                    "source" => mount.source = Some(PathBuf::from(value)),
                    "tag" => mount.tag = Some(value),
                    "target" => mount.target = Some(value),
                    "source_kind" => {
                        mount.source_kind = Some(BindMountSourceKind::parse_config_value(&value)?);
                    }
                    "read_only" => {
                        mount.read_only = Some(parse_bool_field(key, &value)?);
                    }
                    _ => anyhow::bail!("cang launch config contains unknown key {key}"),
                }
            } else if let Some(rest) = key.strip_prefix("disk.") {
                let (index, field) = rest.split_once('.').ok_or_else(|| {
                    anyhow!("cang launch config disk entry {key} is missing field")
                })?;
                let disk = disks.entry(parse_index(key, index)?).or_default();
                match field {
                    "id" => disk.id = Some(value),
                    "path" => disk.path = Some(PathBuf::from(value)),
                    "read_only" => {
                        disk.read_only = Some(parse_bool_field(key, &value)?);
                    }
                    _ => anyhow::bail!("cang launch config contains unknown key {key}"),
                }
            } else if let Some(index) = key.strip_prefix("argv.") {
                argv.insert(parse_index(key, index)?, value);
            } else if let Some(index) = key.strip_prefix("publish.") {
                publish.insert(parse_index(key, index)?, value);
            } else if let Some(index) = key.strip_prefix("env.") {
                let (name, actual) = value
                    .split_once('=')
                    .ok_or_else(|| anyhow!("cang launch config env entry {key} is missing '='"))?;
                env.insert(
                    parse_index(key, index)?,
                    (name.to_owned(), actual.to_owned()),
                );
            } else if let Some(index) = key.strip_prefix("guest_env.") {
                let (name, actual) = value.split_once('=').ok_or_else(|| {
                    anyhow!("cang launch config guest env entry {key} is missing '='")
                })?;
                guest_config_env.insert(
                    parse_index(key, index)?,
                    (name.to_owned(), actual.to_owned()),
                );
            } else if matches!(
                key,
                "task_rootfs"
                    | "workspace_source"
                    | "workspace_tag"
                    | "workspace_target"
                    | "guest_init_override_source"
                    | "guest_init_override_target"
                    | "guest_init_override_read_only"
                    | "host_nix_overlay.selected_reference"
                    | "host_nix_overlay.image_digest"
                    | "host_nix_overlay.digest_key"
                    | "host_nix_overlay.lowerdir"
                    | "host_nix_overlay.upperdir"
                    | "host_nix_overlay.workdir"
                    | "host_nix_overlay.mergeddir"
                    | "hostname"
                    | "ram_mib"
                    | "vcpus"
                    | "log_level"
                    | "network_mode"
                    | "gpu_mode"
                    | "zero_copy_shm"
                    | "permissions"
                    | "workdir"
                    | "exec_path"
                    | "pulse_bridge.socket"
                    | "pulse_bridge.guest_port"
                    | "pulse_bridge.host_port"
                    | "waypipe.socket"
                    | "waypipe.guest_port"
                    | "exec.socket"
                    | "exec.guest_port"
                    | "exec.protocol_version"
                    | "exec.socket_uid"
                    | "exec.socket_gid"
                    | "managed_session.attach_socket"
                    | "managed_session.guest_port"
                    | "managed_session.protocol_version"
                    | "managed_session.attach_socket_uid"
                    | "managed_session.attach_socket_gid"
                    | "managed_session.cleanup_task_rootfs_on_exit"
                    | "managed_session.guest_kernel_console_log"
                    | "seccomp.mode"
                    | "landlock.mode"
                    | "seccomp.audit_trace_path"
                    | "seccomp.audit_baseline_policy_path"
                    | "seccomp.enforce_policy_path"
            ) {
                if fields.insert(key.to_owned(), value).is_some() {
                    anyhow::bail!("cang launch config repeats key {key}");
                }
            } else {
                anyhow::bail!("cang launch config contains unknown key {key}");
            }
        }

        let required = |key: &str| -> Result<String> {
            fields
                .get(key)
                .cloned()
                .ok_or_else(|| anyhow!("cang launch config missing required key {key}"))
        };
        let ram_mib = required("ram_mib")?
            .parse::<u32>()
            .context("cang launch config ram_mib is invalid")?;
        let vcpus = required("vcpus")?
            .parse::<u8>()
            .context("cang launch config vcpus is invalid")?;
        let log_level_text = required("log_level")?;
        let log_level = LogLevel::parse_name(&log_level_text)
            .ok_or_else(|| anyhow!("cang launch config log_level is invalid"))?;
        let network_mode = NetworkMode::parse_config_value(&required("network_mode")?)?;
        let gpu_mode = GpuMode::parse_config_value(
            fields
                .get("gpu_mode")
                .map(String::as_str)
                .unwrap_or(GpuMode::Off.as_config_value()),
        )?;
        let zero_copy_shm = fields.get("zero_copy_shm").map(String::as_str) == Some("1");
        let new_perms = fields
            .get("permissions")
            .map_or(
                Ok(crate::runtime::launch::config::GuestPermissions::default()),
                |value| value.parse(),
            )
            .map_err(anyhow::Error::msg)?;
        let mounts = parse_mounts(&fields, mounts)?;
        let guest_init_override =
            parse_guest_init_override_mount(&fields, required("exec_path")?.as_str())?;
        let host_nix_overlay = parse_host_nix_overlay(&fields)?;
        let pulse_bridge = parse_pulse_bridge(&fields)?;
        let waypipe = parse_waypipe(&fields)?;
        let exec = parse_exec(&fields)?;
        let managed_session = parse_managed_session(&fields)?;
        let seccomp = SeccompMode::parse_config_value(
            fields.get("seccomp.mode").map(String::as_str),
            fields.get("seccomp.audit_trace_path").map(String::as_str),
            fields
                .get("seccomp.audit_baseline_policy_path")
                .map(String::as_str),
            fields
                .get("seccomp.enforce_policy_path")
                .map(String::as_str),
        )?;
        let landlock =
            LandlockMode::parse_config_value(fields.get("landlock.mode").map(String::as_str))?;
        Ok(Self {
            task_rootfs: PathBuf::from(required("task_rootfs")?),
            hostname: required("hostname")?,
            mounts,
            host_nix_overlay,
            guest_init_override,
            disks: disks
                .into_iter()
                .map(|(index, disk)| disk.finish(index))
                .collect::<Result<Vec<_>>>()?,
            ram_mib,
            vcpus,
            log_level,
            network_mode,
            gpu_mode,
            zero_copy_shm,
            new_perms,
            publish: publish.into_values().collect(),
            workdir: required("workdir")?,
            exec_path: required("exec_path")?,
            argv: argv.into_values().collect(),
            env: env.into_values().collect(),
            guest_config_env: guest_config_env.into_values().collect(),
            passt_fd: None,
            pulse_bridge,
            waypipe,
            exec,
            managed_session,
            seccomp,
            landlock,
        })
    }
}

fn parse_pulse_bridge(fields: &BTreeMap<String, String>) -> Result<Option<PulseBridgeConfig>> {
    let keys = [
        "pulse_bridge.socket",
        "pulse_bridge.guest_port",
        "pulse_bridge.host_port",
    ];
    if !keys.iter().any(|key| fields.contains_key(*key)) {
        return Ok(None);
    }

    let host_port = required_field(fields, "pulse_bridge.host_port")?
        .parse::<u16>()
        .context("cang launch config pulse_bridge.host_port is invalid")?;
    if host_port == 0 {
        anyhow::bail!("cang launch config pulse_bridge.host_port must be nonzero");
    }

    Ok(Some(PulseBridgeConfig {
        socket: PathBuf::from(required_field(fields, "pulse_bridge.socket")?),
        guest_port: required_field(fields, "pulse_bridge.guest_port")?
            .parse::<u32>()
            .context("cang launch config pulse_bridge.guest_port is invalid")?,
        host_port,
    }))
}

fn parse_waypipe(fields: &BTreeMap<String, String>) -> Result<Option<WaypipeConfig>> {
    let keys_present = ["waypipe.socket", "waypipe.guest_port"]
        .iter()
        .any(|key| fields.contains_key(*key));
    if !keys_present {
        return Ok(None);
    }

    Ok(Some(WaypipeConfig {
        socket: PathBuf::from(required_field(fields, "waypipe.socket")?),
        guest_port: required_field(fields, "waypipe.guest_port")?
            .parse::<u32>()
            .context("cang launch config waypipe.guest_port is invalid")?,
    }))
}

fn parse_exec(fields: &BTreeMap<String, String>) -> Result<Option<ExecConfig>> {
    let keys = [
        "exec.socket",
        "exec.guest_port",
        "exec.protocol_version",
        "exec.socket_uid",
        "exec.socket_gid",
    ];
    if !keys.iter().any(|key| fields.contains_key(*key)) {
        return Ok(None);
    }

    Ok(Some(ExecConfig {
        socket: PathBuf::from(required_field(fields, "exec.socket")?),
        guest_port: required_field(fields, "exec.guest_port")?
            .parse::<u32>()
            .context("cang launch config exec.guest_port is invalid")?,
        protocol_version: required_field(fields, "exec.protocol_version")?
            .parse::<u16>()
            .context("cang launch config exec.protocol_version is invalid")?,
        socket_uid: required_field(fields, "exec.socket_uid")?
            .parse::<u32>()
            .context("cang launch config exec.socket_uid is invalid")?,
        socket_gid: required_field(fields, "exec.socket_gid")?
            .parse::<u32>()
            .context("cang launch config exec.socket_gid is invalid")?,
    }))
}

fn parse_managed_session(
    fields: &BTreeMap<String, String>,
) -> Result<Option<ManagedSessionConfig>> {
    let keys_present = [
        "managed_session.attach_socket",
        "managed_session.guest_port",
        "managed_session.protocol_version",
        "managed_session.attach_socket_uid",
        "managed_session.attach_socket_gid",
        "managed_session.cleanup_task_rootfs_on_exit",
        "managed_session.guest_kernel_console_log",
    ]
    .iter()
    .any(|key| fields.contains_key(*key));
    if !keys_present {
        return Ok(None);
    }
    Ok(Some(ManagedSessionConfig {
        attach_socket: PathBuf::from(required_field(fields, "managed_session.attach_socket")?),
        guest_port: required_field(fields, "managed_session.guest_port")?
            .parse::<u32>()
            .context("cang launch config managed_session.guest_port is invalid")?,
        protocol_version: required_field(fields, "managed_session.protocol_version")?
            .parse::<u16>()
            .context("cang launch config managed_session.protocol_version is invalid")?,
        attach_socket_uid: required_field(fields, "managed_session.attach_socket_uid")?
            .parse::<u32>()
            .context("cang launch config managed_session.attach_socket_uid is invalid")?,
        attach_socket_gid: required_field(fields, "managed_session.attach_socket_gid")?
            .parse::<u32>()
            .context("cang launch config managed_session.attach_socket_gid is invalid")?,
        cleanup_task_rootfs_on_exit: parse_bool_field(
            "managed_session.cleanup_task_rootfs_on_exit",
            &required_field(fields, "managed_session.cleanup_task_rootfs_on_exit")?,
        )?,
        guest_kernel_console_log: PathBuf::from(required_field(
            fields,
            "managed_session.guest_kernel_console_log",
        )?),
    }))
}

fn parse_host_nix_overlay(fields: &BTreeMap<String, String>) -> Result<Option<HostNixOverlay>> {
    let keys_present = [
        "host_nix_overlay.selected_reference",
        "host_nix_overlay.image_digest",
        "host_nix_overlay.digest_key",
        "host_nix_overlay.lowerdir",
        "host_nix_overlay.upperdir",
        "host_nix_overlay.workdir",
        "host_nix_overlay.mergeddir",
    ]
    .iter()
    .any(|key| fields.contains_key(*key));
    if !keys_present {
        return Ok(None);
    }
    Ok(Some(HostNixOverlay {
        selected_reference: required_field(fields, "host_nix_overlay.selected_reference")?,
        image_digest: required_field(fields, "host_nix_overlay.image_digest")?,
        digest_key: required_field(fields, "host_nix_overlay.digest_key")?,
        lowerdir: PathBuf::from(required_field(fields, "host_nix_overlay.lowerdir")?),
        upperdir: PathBuf::from(required_field(fields, "host_nix_overlay.upperdir")?),
        workdir: PathBuf::from(required_field(fields, "host_nix_overlay.workdir")?),
        mergeddir: PathBuf::from(required_field(fields, "host_nix_overlay.mergeddir")?),
    }))
}

#[derive(Default)]
struct PartialBindMount {
    source: Option<PathBuf>,
    tag: Option<String>,
    target: Option<String>,
    source_kind: Option<BindMountSourceKind>,
    read_only: Option<bool>,
}

impl PartialBindMount {
    fn finish(self, index: usize) -> Result<BindMount> {
        Ok(BindMount {
            source: self
                .source
                .ok_or_else(|| anyhow!("cang launch config mount.{index} missing source"))?,
            tag: self
                .tag
                .ok_or_else(|| anyhow!("cang launch config mount.{index} missing tag"))?,
            target: self
                .target
                .ok_or_else(|| anyhow!("cang launch config mount.{index} missing target"))?,
            source_kind: self.source_kind.unwrap_or(BindMountSourceKind::Directory),
            read_only: self.read_only.unwrap_or(false),
        })
    }
}

#[derive(Default)]
struct PartialDiskAttachment {
    id: Option<String>,
    path: Option<PathBuf>,
    read_only: Option<bool>,
}

impl PartialDiskAttachment {
    fn finish(self, index: usize) -> Result<DiskAttachment> {
        Ok(DiskAttachment {
            id: self
                .id
                .ok_or_else(|| anyhow!("cang launch config disk.{index} missing id"))?,
            path: self
                .path
                .ok_or_else(|| anyhow!("cang launch config disk.{index} missing path"))?,
            read_only: self
                .read_only
                .ok_or_else(|| anyhow!("cang launch config disk.{index} missing read_only"))?,
        })
    }
}

fn parse_guest_init_override_mount(
    fields: &BTreeMap<String, String>,
    exec_path: &str,
) -> Result<Option<GuestInitOverrideMount>> {
    let keys_present = [
        "guest_init_override_source",
        "guest_init_override_target",
        "guest_init_override_read_only",
    ]
    .iter()
    .any(|key| fields.contains_key(*key));
    if !keys_present {
        return Ok(None);
    }

    let mount = GuestInitOverrideMount {
        source: PathBuf::from(required_field(fields, "guest_init_override_source")?),
        target: required_field(fields, "guest_init_override_target")?,
        read_only: match required_field(fields, "guest_init_override_read_only")?.as_str() {
            "true" => true,
            "false" => false,
            _ => anyhow::bail!("cang launch config guest_init_override_read_only is invalid"),
        },
    };
    guest_init::validate_guest_init_override_mount(&mount, exec_path)?;
    Ok(Some(mount))
}

fn parse_mounts(
    fields: &BTreeMap<String, String>,
    indexed_mounts: BTreeMap<usize, PartialBindMount>,
) -> Result<Vec<BindMount>> {
    let has_indexed_mounts = !indexed_mounts.is_empty();
    let mounts = if !has_indexed_mounts {
        vec![BindMount::directory(
            PathBuf::from(required_field(fields, "workspace_source")?),
            required_field(fields, "workspace_tag")?,
            required_field(fields, "workspace_target")?,
        )]
    } else {
        indexed_mounts
            .into_iter()
            .map(|(index, mount)| mount.finish(index))
            .collect::<Result<Vec<_>>>()?
    };

    if has_indexed_mounts && has_legacy_workspace_fields(fields) {
        let legacy = BindMount::directory(
            PathBuf::from(required_field(fields, "workspace_source")?),
            required_field(fields, "workspace_tag")?,
            required_field(fields, "workspace_target")?,
        );
        let workspace = mounts::workspace_mount(&mounts)?;
        if *workspace != legacy {
            anyhow::bail!(
                "cang launch config workspace compatibility fields disagree with indexed mounts"
            );
        }
    }

    mounts::validate_mounts(&mounts)?;
    Ok(mounts)
}

fn parse_bool_field(key: &str, value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!("cang launch config entry {key} has invalid boolean value"),
    }
}

fn has_legacy_workspace_fields(fields: &BTreeMap<String, String>) -> bool {
    fields.contains_key("workspace_source")
        || fields.contains_key("workspace_tag")
        || fields.contains_key("workspace_target")
}

fn required_field(fields: &BTreeMap<String, String>, key: &str) -> Result<String> {
    fields
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow!("cang launch config missing required key {key}"))
}

pub(crate) fn push_field(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push('=');
    out.push_str(&encode_hex(value));
    out.push('\n');
}

pub(crate) fn decode_text_for_debug(text: &str) -> Result<String> {
    let mut out = String::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (key, encoded) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("cang launch config line {} is missing '='", line_index + 1))?;
        let value = decode_hex(encoded).with_context(|| {
            format!("cang launch config line {} has invalid hex", line_index + 1)
        })?;
        out.push_str(key);
        out.push('=');
        out.push_str(&value.escape_debug().to_string());
        out.push('\n');
    }
    Ok(out)
}

fn encode_hex(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn decode_hex(encoded: &str) -> Result<String> {
    if !encoded.len().is_multiple_of(2) {
        anyhow::bail!("odd number of hex characters");
    }
    let bytes = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect::<std::result::Result<Vec<_>, anyhow::Error>>()?;
    String::from_utf8(bytes).context("hex decoded value is not UTF-8")
}

fn parse_index(key: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .with_context(|| format!("cang launch config index in {key} is invalid"))
}
