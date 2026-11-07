use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::PathBuf;
use std::str::FromStr;

pub(in crate::guest_init) const DEV_USER: &str = "dev";
pub(in crate::guest_init) const DEV_HOME: &str = "/home/dev";
pub(in crate::guest_init) const DEFAULT_SHELL: &str = "fish";
pub(in crate::guest_init) const RUN_DIR: &str = "/run/cang";
pub(in crate::guest_init) const FD_PRESSURE_STATUS_PATH: &str = "/run/cang/fd-pressure.status";
pub(in crate::guest_init) const SWAP_STATUS_PATH: &str = "/run/cang/swap.status";
pub(in crate::guest_init) const NIX_STATUS_PATH: &str = "/run/cang/nix-prep.status";
pub(in crate::guest_init) const NIX_LOG_PATH: &str = "/run/cang/nix-prep.log";
pub(in crate::guest_init) const NIX_WAIT_TIMEOUT_SECS: u64 = 120;
pub(in crate::guest_init) const NIX_DAEMON_SOCKET_PATH: &str = "/nix/var/nix/daemon-socket/socket";
pub(in crate::guest_init) const NIX_REMOTE_URI: &str = "unix:///nix/var/nix/daemon-socket/socket";
pub(in crate::guest_init) const PODMAN_STATUS_PATH: &str = "/run/cang/podman-prep.status";
pub(in crate::guest_init) const PODMAN_LOG_PATH: &str = "/run/cang/podman-prep.log";
pub(in crate::guest_init) const PODMAN_WAIT_TIMEOUT_SECS: u64 = 120;
pub(in crate::guest_init) const RAW_NIX_DISK_ID: &str = "cang-nix";
pub(in crate::guest_init) const RAW_NIX_DISK_LABEL: &str = "CANG_NIX";
pub(in crate::guest_init) const RAW_BUILD_DISK_ID: &str = "cang-build";
pub(in crate::guest_init) const RAW_BUILD_DISK_LABEL: &str = "CANG_BUILD";
pub(in crate::guest_init) const RAW_CONTAINER_DISK_ID: &str = "cang-containers";
pub(in crate::guest_init) const RAW_CONTAINER_DISK_LABEL: &str = "CANG_CONTAINERS";
/// The read-only `/nix` lower disk, and the host-written overlay directories the
/// guest composes over it. All three names are the host's contract: absent the
/// flag the guest keeps the host-composed overlay.
pub(in crate::guest_init) const RAW_NIX_LOWER_DISK_ID: &str = "cang-nix-lower";
pub(in crate::guest_init) const RAW_NIX_LOWER_DISK_LABEL: &str = "CANG_NIX_LOWER";
pub(in crate::guest_init) const NIX_LOWER_BLOCK_ENV: &str = "CANG_NIX_LOWER_BLOCK";
pub(in crate::guest_init) const NIX_LOWER_DISK_ID_ENV: &str = "CANG_NIX_LOWER_DISK_ID";
pub(in crate::guest_init) const NIX_LOWER_DISK_LABEL_ENV: &str = "CANG_NIX_LOWER_DISK_LABEL";
pub(in crate::guest_init) const NIX_LOWER_UPPERDIR_ENV: &str = "CANG_NIX_LOWER_UPPERDIR";
pub(in crate::guest_init) const NIX_LOWER_WORKDIR_ENV: &str = "CANG_NIX_LOWER_WORKDIR";
pub(in crate::guest_init) const ENTER_AS_ROOT_ENV: &str = "CANG_ENTER_AS_ROOT";
pub(in crate::guest_init) const CONTAINERS_STORE_ENV: &str = "CANG_CONTAINERS_STORE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) enum ContainerStoreBackend {
    RawDisk,
}

impl ContainerStoreBackend {
    pub(in crate::guest_init) fn parse_env_value(value: &str) -> Result<Self> {
        match value {
            "raw-disk" => Ok(Self::RawDisk),
            _ => anyhow::bail!("{CONTAINERS_STORE_ENV} must be 'raw-disk' (got '{value}')"),
        }
    }

    pub(in crate::guest_init) fn from_optional_env_value(value: Option<String>) -> Result<Self> {
        value.map_or(Ok(Self::RawDisk), |value| Self::parse_env_value(&value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) enum GuestPermission {
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

    pub(in crate::guest_init) const fn as_str(self) -> &'static str {
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
pub(in crate::guest_init) struct GuestPermissions(u8);

impl GuestPermissions {
    pub(in crate::guest_init) const fn contains(self, permission: GuestPermission) -> bool {
        self.0 & permission.bit() != 0
    }
}

impl FromStr for GuestPermissions {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.is_empty() {
            anyhow::bail!("CANG_PERMISSIONS must not be empty");
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
                "" => anyhow::bail!("CANG_PERMISSIONS must not contain empty values"),
                other => {
                    anyhow::bail!("CANG_PERMISSIONS contains unsupported permission '{other}'")
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
pub(in crate::guest_init) struct CangEnv {
    pub(in crate::guest_init) nix_overlay: bool,
    pub(in crate::guest_init) nix_host_overlay: bool,
    pub(in crate::guest_init) containers_storage: bool,
    pub(in crate::guest_init) container_store_backend: ContainerStoreBackend,
    pub(in crate::guest_init) use_passt: bool,
    pub(in crate::guest_init) gpu_drm: bool,
    pub(in crate::guest_init) wayland: bool,
    pub(in crate::guest_init) permissions: GuestPermissions,
    pub(in crate::guest_init) enter_as_root: bool,
    pub(in crate::guest_init) host_uid: Option<u32>,
    pub(in crate::guest_init) host_gid: Option<u32>,
    pub(in crate::guest_init) nix_disk_id: String,
    pub(in crate::guest_init) nix_disk_label: String,
    pub(in crate::guest_init) containers_disk_id: String,
    pub(in crate::guest_init) containers_disk_label: String,
    pub(in crate::guest_init) build_disk_id: String,
    pub(in crate::guest_init) build_disk_label: String,
    /// `Some` only when the host attached a read-only `/nix` lower disk and the
    /// guest is therefore the one composing the `/nix` overlay.
    pub(in crate::guest_init) nix_lower: Option<NixLowerContract>,
}

/// What a guest-composed `/nix` overlay needs from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct NixLowerContract {
    pub(in crate::guest_init) disk_id: String,
    pub(in crate::guest_init) disk_label: String,
    pub(in crate::guest_init) upperdir: PathBuf,
    pub(in crate::guest_init) workdir: PathBuf,
}

impl NixLowerContract {
    /// The host sets the lower directory pair when it attached the read-only image,
    /// and then both are required: a guest mount without them would fail at boot
    /// and leave `/nix` as the host overlay anyway, which is the behaviour the
    /// disk exists to replace.
    pub(in crate::guest_init) fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>> {
        if lookup(NIX_LOWER_BLOCK_ENV).as_deref() != Some("1") {
            return Ok(None);
        }
        Ok(Some(Self {
            disk_id: lookup(NIX_LOWER_DISK_ID_ENV)
                .unwrap_or_else(|| RAW_NIX_LOWER_DISK_ID.to_owned()),
            disk_label: lookup(NIX_LOWER_DISK_LABEL_ENV)
                .unwrap_or_else(|| RAW_NIX_LOWER_DISK_LABEL.to_owned()),
            upperdir: required_path(&lookup, NIX_LOWER_UPPERDIR_ENV)?,
            workdir: required_path(&lookup, NIX_LOWER_WORKDIR_ENV)?,
        }))
    }
}

fn required_path(lookup: &impl Fn(&str) -> Option<String>, name: &str) -> Result<PathBuf> {
    let value =
        lookup(name).ok_or_else(|| anyhow!("{name} is required when {NIX_LOWER_BLOCK_ENV}=1"))?;
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        anyhow::bail!("{name} must be an absolute path (got '{value}')");
    }
    Ok(path)
}

impl CangEnv {
    pub(in crate::guest_init) fn from_process_env() -> Result<Self> {
        Ok(Self {
            nix_overlay: env_flag("CANG_NIX_OVERLAY"),
            nix_host_overlay: env_flag("CANG_NIX_HOST_OVERLAY"),
            containers_storage: env_flag("CANG_CONTAINERS_STORAGE"),
            container_store_backend: ContainerStoreBackend::from_optional_env_value(
                env::var(CONTAINERS_STORE_ENV)
                    .ok()
                    .filter(|value| !value.is_empty()),
            )?,
            use_passt: env_flag("CANG_USE_PASST"),
            gpu_drm: env_flag("CANG_GPU_DRM"),
            wayland: env_flag("CANG_WAYLAND"),
            permissions: env::var("CANG_PERMISSIONS")
                .ok()
                .map_or(Ok(GuestPermissions::default()), |value| value.parse())?,
            enter_as_root: env_flag(ENTER_AS_ROOT_ENV),
            host_uid: parse_optional_u32("CANG_HOST_UID")?,
            host_gid: parse_optional_u32("CANG_HOST_GID")?,
            nix_disk_id: env::var("CANG_NIX_DISK_ID")
                .unwrap_or_else(|_| RAW_NIX_DISK_ID.to_owned()),
            nix_disk_label: env::var("CANG_NIX_DISK_LABEL")
                .unwrap_or_else(|_| RAW_NIX_DISK_LABEL.to_owned()),
            containers_disk_id: env::var("CANG_CONTAINERS_DISK_ID")
                .unwrap_or_else(|_| RAW_CONTAINER_DISK_ID.to_owned()),
            containers_disk_label: env::var("CANG_CONTAINERS_DISK_LABEL")
                .unwrap_or_else(|_| RAW_CONTAINER_DISK_LABEL.to_owned()),
            build_disk_id: env::var("CANG_BUILD_DISK_ID")
                .unwrap_or_else(|_| RAW_BUILD_DISK_ID.to_owned()),
            build_disk_label: env::var("CANG_BUILD_DISK_LABEL")
                .unwrap_or_else(|_| RAW_BUILD_DISK_LABEL.to_owned()),
            nix_lower: NixLowerContract::from_lookup(|name| {
                env::var(name).ok().filter(|value| !value.is_empty())
            })?,
        })
    }

    pub(in crate::guest_init) fn require_host_identity(&self) -> Result<(u32, u32)> {
        let uid = self
            .host_uid
            .ok_or_else(|| anyhow!("CANG_HOST_UID is required for cang guest init"))?;
        let gid = self
            .host_gid
            .ok_or_else(|| anyhow!("CANG_HOST_GID is required for cang guest init"))?;
        Ok((uid, gid))
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name).as_deref() == Ok("1")
}

fn parse_optional_u32(name: &str) -> Result<Option<u32>> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => {
            Ok(Some(value.parse().with_context(|| {
                format!("invalid numeric value in {name}")
            })?))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
