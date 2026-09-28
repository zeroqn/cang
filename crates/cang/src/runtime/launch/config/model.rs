use anyhow::Result;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use crate::logging::LogLevel;
use crate::runtime::landlock::LandlockMode;
use crate::runtime::seccomp::SeccompMode;
use crate::runtime::session::rootfs::image_source::OciProcessConfig;
use crate::runtime::vm::gpu::GpuMode;

pub(crate) const DEFAULT_PULSE_BRIDGE_PORT: u32 = 50_429;
pub(crate) const DEFAULT_WAYPIPE_PORT: u32 = 50_427;
pub(crate) const WORKSPACE_TAG: &str = "cang-workspace";
pub(crate) const WORKSPACE_TARGET: &str = "/workspace";
pub const CODEX_TAG: &str = "cang-codex";
pub const CODEX_TARGET: &str = "/home/dev/.codex";
pub const PI_TAG: &str = "cang-pi";
pub const PI_TARGET: &str = "/home/dev/.pi";
pub const CORTEXKIT_TAG: &str = "cang-cortexkit";
pub const CORTEXKIT_TARGET: &str = "/home/dev/.local/share/cortexkit";
pub const OMP_TAG: &str = "cang-omp";
pub const OMP_TARGET: &str = "/home/dev/.omp";
pub const DIRGE_CONFIG_TAG: &str = "cang-dirge-config";
pub const DIRGE_CONFIG_TARGET: &str = "/home/dev/.config/dirge";
pub const DIRGE_DATA_TAG: &str = "cang-dirge-data";
pub const DIRGE_DATA_TARGET: &str = "/home/dev/.local/share/dirge";
pub const DIRGE_HOME_TAG: &str = "cang-dirge-home";
pub const DIRGE_HOME_TARGET: &str = "/home/dev/.dirge";
pub const CARGO_TAG: &str = "cang-cargo";
pub const CARGO_TARGET: &str = "/home/dev/.cargo";
pub const SCCACHE_TAG: &str = "cang-sccache";
pub const SCCACHE_TARGET: &str = "/home/dev/.cache/sccache";
pub const NIX_TAG: &str = "cang-nix";
pub const NIX_TARGET: &str = "/nix";
pub(super) const SCCACHE_DIR_ENV: &str = "SCCACHE_DIR";
pub(super) const HOST_UID_ENV: &str = "CANG_HOST_UID";
pub(super) const HOST_GID_ENV: &str = "CANG_HOST_GID";
pub(super) const ENTER_AS_ROOT_ENV: &str = "CANG_ENTER_AS_ROOT";
pub(super) const GUEST_PROFILE_ENV: &str = "CANG_GUEST_PROFILE";
pub(super) const GUEST_DEBUG_ENV: &str = "CANG_GUEST_DEBUG";
pub(super) const NIX_ALLOCATOR_ENV: &str = "CANG_NIX_ALLOCATOR";
pub(super) const GUEST_USE_PASST_ENV: &str = "CANG_USE_PASST";
pub(super) const GUEST_GPU_DRM_ENV: &str = "CANG_GPU_DRM";
pub(super) const GUEST_WAYLAND_ENV: &str = "CANG_WAYLAND";
pub(super) const GUEST_WAYPIPE_PORT_ENV: &str = "CANG_WAYPIPE_PORT";
pub(super) const GUEST_PULSE_SERVER_ENV: &str = "CANG_PULSE_SERVER";
pub(super) const GUEST_PULSE_BRIDGE_PORT_ENV: &str = "CANG_PULSE_BRIDGE_PORT";
pub(super) const GUEST_EXEC_PORT_ENV: &str = "CANG_EXEC_PORT";
pub(super) const GUEST_EXEC_PROTOCOL_VERSION_ENV: &str = "CANG_EXEC_PROTOCOL_VERSION";
pub(super) const GUEST_PERMISSIONS_ENV: &str = "CANG_PERMISSIONS";
pub(super) const GUEST_SESSION_MANAGED_ENV: &str = "CANG_SESSION_MANAGED";
pub(super) const GUEST_ATTACH_PORT_ENV: &str = "CANG_ATTACH_PORT";
pub(super) const GUEST_ATTACH_PROTOCOL_VERSION_ENV: &str = "CANG_ATTACH_PROTOCOL_VERSION";
pub(super) const IMAGE_PATH_ENV: &str = "PATH";
pub(super) const KRUN_CONFIG_ENV: &str = "KRUN_CONFIG";
pub(crate) const CANG_KRUN_CONFIG_PATH: &str = "/.cang_config.json";
pub(super) const KIB: u64 = 1024;
pub(super) const MIB_PER_GIB: u32 = 1024;
pub(super) const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;
pub(super) const MAX_GIB_FOR_KRUN_RAM_MIB: u32 = u32::MAX / MIB_PER_GIB;
pub(super) const HOST_MEMINFO: &str = "/proc/meminfo";
pub(super) const IMAGE_CANG_ENV_ALLOWLIST: &[&str] = &[
    "NIX_CONFIG",
    "SSL_CERT_FILE",
    "NIX_SSL_CERT_FILE",
    "CANG_FISH_CONFIG_SOURCE",
    "CANG_STARSHIP_CONFIG_SOURCE",
    "CANG_MIMALLOC_LIB",
    "CANG_GRAPHENE_HARDENED_MALLOC_LIB",
    "CANG_REAL_PODMAN",
    "MONTY_BIN",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestPermission {
    IoUring,
    NetAdmin,
    NetRaw,
    Bpf,
    Perf,
    SysAdmin,
}

impl GuestPermission {
    const ALL: [Self; 6] = [
        Self::IoUring,
        Self::NetAdmin,
        Self::NetRaw,
        Self::Bpf,
        Self::Perf,
        Self::SysAdmin,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::IoUring => "io-uring",
            Self::NetAdmin => "net-admin",
            Self::NetRaw => "net-raw",
            Self::Bpf => "bpf",
            Self::Perf => "perf",
            Self::SysAdmin => "sys-admin",
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::IoUring => 1 << 0,
            Self::NetAdmin => 1 << 1,
            Self::NetRaw => 1 << 2,
            Self::Bpf => 1 << 3,
            Self::Perf => 1 << 4,
            Self::SysAdmin => 1 << 5,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GuestPermissions(u8);

impl GuestPermissions {
    pub(crate) const fn contains(self, permission: GuestPermission) -> bool {
        self.0 & permission.bit() != 0
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl FromStr for GuestPermissions {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err("permissions must not be empty".to_owned());
        }

        let mut permissions = Self::default();
        for token in value.split(',') {
            let permission = match token {
                "io-uring" => GuestPermission::IoUring,
                "net-admin" => GuestPermission::NetAdmin,
                "net-raw" => GuestPermission::NetRaw,
                "bpf" => GuestPermission::Bpf,
                "perf" => GuestPermission::Perf,
                "sys-admin" => GuestPermission::SysAdmin,
                "" => return Err("permissions must not contain empty values".to_owned()),
                other => {
                    return Err(format!(
                        "unsupported permission '{other}'; use io-uring, net-admin, net-raw, bpf, perf, or sys-admin"
                    ));
                }
            };
            permissions.0 |= permission.bit();
        }
        Ok(permissions)
    }
}

impl std::fmt::Display for GuestPermissions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for permission in GuestPermission::ALL {
            if self.contains(permission) {
                if !first {
                    formatter.write_str(",")?;
                }
                formatter.write_str(permission.as_str())?;
                first = false;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    pub source: PathBuf,
    pub tag: String,
    pub target: String,
    pub source_kind: BindMountSourceKind,
    pub read_only: bool,
}

impl BindMount {
    pub(crate) fn directory(
        source: impl Into<PathBuf>,
        tag: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            tag: tag.into(),
            target: target.into(),
            source_kind: BindMountSourceKind::Directory,
            read_only: false,
        }
    }

    pub(crate) fn file(
        source: impl Into<PathBuf>,
        tag: impl Into<String>,
        target: impl Into<String>,
        read_only: bool,
    ) -> Self {
        Self {
            source: source.into(),
            tag: tag.into(),
            target: target.into(),
            source_kind: BindMountSourceKind::File,
            read_only,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindMountSourceKind {
    Directory,
    File,
}

impl BindMountSourceKind {
    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }

    pub(crate) fn parse_config_value(value: &str) -> Result<Self> {
        match value {
            "directory" => Ok(Self::Directory),
            "file" => Ok(Self::File),
            _ => anyhow::bail!("cang launch config bind mount source_kind is invalid"),
        }
    }
}

pub(crate) fn canonical_mount_target(target: &str) -> Result<String> {
    let path = Path::new(target);
    if !path.is_absolute() {
        anyhow::bail!("cang bind mount target '{target}' must be absolute");
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    anyhow::anyhow!("cang bind mount target '{target}' must be valid UTF-8")
                })?;
                components.push(value);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::bail!("cang bind mount target '{target}' must not contain '..'");
            }
            Component::Prefix(_) => {
                anyhow::bail!("cang bind mount target '{target}' has an unsupported prefix");
            }
        }
    }
    if components.is_empty() {
        anyhow::bail!("cang bind mount target must not be /");
    }
    Ok(format!("/{}", components.join("/")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocatorMode {
    Mimalloc,
    Hardened,
    Glibc,
}

impl AllocatorMode {
    pub(crate) fn as_env_value(self) -> &'static str {
        match self {
            Self::Mimalloc => "mimalloc",
            Self::Hardened => "hardened",
            Self::Glibc => "glibc",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestInitOverrideMount {
    pub(crate) source: PathBuf,
    pub(crate) target: String,
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiskAttachment {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostNixOverlay {
    pub(crate) selected_reference: String,
    pub(crate) image_digest: String,
    pub(crate) digest_key: String,
    pub(crate) lowerdir: PathBuf,
    pub(crate) upperdir: PathBuf,
    pub(crate) workdir: PathBuf,
    pub(crate) mergeddir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkMode {
    Tsi,
    Passt,
}

impl NetworkMode {
    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::Tsi => "tsi",
            Self::Passt => "passt",
        }
    }

    pub(super) fn parse_config_value(value: &str) -> Result<Self> {
        match value {
            "tsi" => Ok(Self::Tsi),
            "passt" => Ok(Self::Passt),
            _ => anyhow::bail!("cang launch config network_mode is invalid"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PulseServer {
    Direct(SocketAddr),
    HostLoopback { port: u16 },
}

impl PulseServer {
    pub(crate) fn direct_env_value(self) -> Option<String> {
        match self {
            Self::Direct(address) => Some(format!("tcp:{address}")),
            Self::HostLoopback { .. } => None,
        }
    }

    pub(crate) fn host_loopback_port(self) -> Option<u16> {
        match self {
            Self::Direct(_) => None,
            Self::HostLoopback { port } => Some(port),
        }
    }
}

impl FromStr for PulseServer {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let endpoint = value
            .strip_prefix("tcp:")
            .ok_or_else(|| "pulse server must use tcp:HOST:PORT".to_owned())?;

        if let Some(port) = endpoint.strip_prefix("localhost:") {
            let port = port
                .parse::<u16>()
                .map_err(|_| "pulse server must contain a valid nonzero port".to_owned())?;
            if port == 0 {
                return Err("pulse server port must be nonzero".to_owned());
            }
            return Ok(Self::HostLoopback { port });
        }

        let address = endpoint.parse::<SocketAddr>().map_err(|_| {
            "pulse server must use localhost or a valid literal IP address and port".to_owned()
        })?;
        if address.port() == 0 {
            return Err("pulse server port must be nonzero".to_owned());
        }

        match address {
            SocketAddr::V4(address) if *address.ip() == std::net::Ipv4Addr::LOCALHOST => {
                Ok(Self::HostLoopback {
                    port: address.port(),
                })
            }
            address => Ok(Self::Direct(address)),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LaunchSpec<'a> {
    pub(crate) task_rootfs: &'a Path,
    pub(crate) hostname: &'a str,
    pub mounts: &'a [BindMount],
    pub(crate) guest_init_override: Option<GuestInitOverrideMount>,
    pub(crate) guest_init_exec: &'a str,
    pub(crate) guest_command: &'a [String],
    pub(crate) image_process_config: &'a OciProcessConfig,
    pub(crate) mem_gib: Option<u32>,
    pub(crate) log_level: LogLevel,
    pub(crate) network_mode: NetworkMode,
    pub(crate) pulse: Option<PulseServer>,
    pub(crate) gpu_mode: GpuMode,
    pub(crate) zero_copy_shm: bool,
    pub(crate) wayland: bool,
    pub(crate) new_perms: GuestPermissions,
    pub(crate) publish: &'a [String],
    pub(crate) profile: bool,
    pub(crate) root: bool,
    pub(crate) allocator: AllocatorMode,
    pub(crate) host_uid: u32,
    pub(crate) host_gid: u32,
    pub(crate) vcpus: u8,
    pub(crate) disks: Vec<DiskAttachment>,
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) host_nix_overlay: Option<HostNixOverlay>,
    pub(crate) pulse_bridge: Option<PulseBridgeConfig>,
    pub(crate) waypipe: Option<WaypipeConfig>,
    pub(crate) exec: Option<ExecConfig>,
    pub(crate) managed_session: Option<ManagedSessionConfig>,
}

/// Serialized helper/libkrun execution contract.
///
/// `LaunchConfig` is derived from a resolved launch plan plus materialized task
/// rootfs data and is written into the task rootfs for the helper process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PulseBridgeConfig {
    pub(crate) socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) host_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaypipeConfig {
    pub(crate) socket: PathBuf,
    pub(crate) guest_port: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecConfig {
    pub(crate) socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) protocol_version: u16,
    pub(crate) socket_uid: u32,
    pub(crate) socket_gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedSessionConfig {
    pub(crate) attach_socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) protocol_version: u16,
    pub(crate) attach_socket_uid: u32,
    pub(crate) attach_socket_gid: u32,
    pub(crate) cleanup_task_rootfs_on_exit: bool,
    /// Guest kernel console destination inside the task state directory.
    ///
    /// The kernel explains an OOM kill or a panic only on its console, and a
    /// managed guest discards that console by default, so the launch routes it
    /// to a file the supervisor can diagnose afterwards.
    pub(crate) guest_kernel_console_log: PathBuf,
}

/// File name libkrun uses for the guest kernel console of a managed task.
pub(crate) const MANAGED_GUEST_KERNEL_CONSOLE_LOG: &str = "guest-kernel-console.log";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchConfig {
    pub(crate) task_rootfs: PathBuf,
    pub(crate) hostname: String,
    pub mounts: Vec<BindMount>,
    pub(crate) host_nix_overlay: Option<HostNixOverlay>,
    pub(crate) guest_init_override: Option<GuestInitOverrideMount>,
    pub(crate) disks: Vec<DiskAttachment>,
    pub(crate) ram_mib: u32,
    pub(crate) vcpus: u8,
    pub(crate) log_level: LogLevel,
    pub(crate) network_mode: NetworkMode,
    pub(crate) gpu_mode: GpuMode,
    pub(crate) zero_copy_shm: bool,
    pub(crate) new_perms: GuestPermissions,
    pub(crate) publish: Vec<String>,
    pub(crate) workdir: String,
    pub(crate) exec_path: String,
    pub(crate) argv: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) guest_config_env: Vec<(String, String)>,
    pub(crate) passt_fd: Option<i32>,
    pub(crate) pulse_bridge: Option<PulseBridgeConfig>,
    pub(crate) waypipe: Option<WaypipeConfig>,
    pub(crate) exec: Option<ExecConfig>,
    pub(crate) managed_session: Option<ManagedSessionConfig>,
    pub(crate) seccomp: SeccompMode,
    pub(crate) landlock: LandlockMode,
}
