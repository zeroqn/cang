//! Host-side Landlock confinement for the cang VM worker.
//!
//! This module owns the kernel-independent effective-policy model and the thin
//! adapter that translates that model to the rust-landlock crate immediately
//! before the VMM is built.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, PathBeneath, PathFd,
    RestrictSelfAttr, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    Scope, make_bitflags,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd};
use std::path::{Path, PathBuf};

use crate::runtime::launch::config::{BindMount, DiskAttachment, HostNixOverlay, LaunchConfig};
use crate::runtime::vm::gpu::GpuMode;

const FD_DIR: &str = "/proc/self/fd";
const LIBKRUN_KVM_DEVICE: &str = "/dev/kvm";
const GPU_DRI_DEVICE_DIR: &str = "/dev/dri";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub(crate) enum LandlockMode {
    All,
    #[default]
    Relax,
    BestEffort,
    Off,
}

impl LandlockMode {
    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Relax => "relax",
            Self::BestEffort => "best-effort",
            Self::Off => "off",
        }
    }

    pub(crate) fn parse_config_value(mode: Option<&str>) -> Result<Self> {
        match mode.unwrap_or("relax") {
            "all" => Ok(Self::All),
            "relax" => Ok(Self::Relax),
            "best-effort" => Ok(Self::BestEffort),
            "off" => Ok(Self::Off),
            _ => bail!("cang launch config landlock.mode is invalid"),
        }
    }

    fn compatibility(self) -> CompatLevel {
        match self {
            Self::All | Self::Relax => CompatLevel::HardRequirement,
            Self::BestEffort | Self::Off => CompatLevel::BestEffort,
        }
    }

    fn is_enabled(self) -> bool {
        self != Self::Off
    }

    fn handles_bind_tcp(self) -> bool {
        self == Self::All
    }

    fn requires_full_enforcement(self) -> bool {
        matches!(self, Self::All | Self::Relax)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectivePolicy {
    pub(crate) mode: LandlockMode,
    pub(crate) path_rules: Vec<PathRule>,
    pub(crate) bind_tcp: BindTcpPolicy,
    pub(crate) connect_tcp: ConnectTcpPolicy,
    pub(crate) ipc_scopes: Vec<IpcScope>,
    pub(crate) audit: AuditPolicy,
    pub(crate) fd_report: RetainedFdReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathRule {
    pub(crate) category: PathCategory,
    pub(crate) path: PathBuf,
    pub(crate) access: PathAccess,
    pub(crate) read_only_guarantee: ReadOnlyGuarantee,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathCategory {
    PreparedRoot,
    BindMount { target: String },
    Disk { id: String },
    GuestInitOverride,
    HostNixOverlay { role: HostNixOverlayRole },
    ManagedAttachSocket,
    ManagedSessionState,
    ProfileOutput,
    RuntimeDevice { name: String },
    GpuSystemInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostNixOverlayRole {
    Lower,
    Upper,
    Work,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadOnlyGuarantee {
    LandlockEnforced,
    MountEnforced,
    NotReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectTcpPolicy {
    UnrestrictedByDesign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindTcpPolicy {
    Unrestricted,
    RestrictedPorts(Vec<u16>),
}

impl BindTcpPolicy {
    fn for_mode(mode: LandlockMode, config: &LaunchConfig) -> Result<Self> {
        if mode.handles_bind_tcp() {
            Ok(Self::RestrictedPorts(bind_tcp_ports(config)?))
        } else {
            Ok(Self::Unrestricted)
        }
    }

    fn report_value(&self) -> String {
        match self {
            Self::Unrestricted => "unrestricted".to_owned(),
            Self::RestrictedPorts(ports) => {
                let ports = ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("restricted:[{ports}]")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpcScope {
    AbstractUnixSocket,
    Signal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditPolicy {
    pub(crate) log_same_exec: bool,
    pub(crate) log_new_exec: bool,
    pub(crate) log_subdomains: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RetainedFdReport {
    pub(crate) entries: Vec<RetainedFd>,
    pub(crate) unexpected: Vec<RetainedFd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetainedFd {
    pub(crate) fd: i32,
    pub(crate) target: String,
    pub(crate) classification: RetainedFdClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedFdClass {
    Stdio,
    PasstSocket,
    RuntimeKernelObject,
    BenignDevice,
    Unexpected,
}

pub(crate) fn apply(
    config: &LaunchConfig,
    task_state_dir: &Path,
    profile_enabled: bool,
) -> Result<()> {
    if !config.landlock.is_enabled() {
        tracing::debug!(landlock = "off", "cang internal: Landlock disabled");
        return Ok(());
    }

    let policy = EffectivePolicy::build(config, task_state_dir, profile_enabled)?;
    tracing::debug!(report = %policy.report(), "cang internal: Landlock effective policy");

    apply_policy(&policy).with_context(|| {
        format!(
            "failed to apply cang Landlock policy in {} mode",
            policy.mode.as_config_value()
        )
    })?;
    Ok(())
}

impl EffectivePolicy {
    pub(crate) fn build(
        config: &LaunchConfig,
        task_state_dir: &Path,
        profile_enabled: bool,
    ) -> Result<Self> {
        let fd_report = retained_fd_report(config.landlock, config.passt_fd)?;
        Self::build_with_fd_report(config, task_state_dir, profile_enabled, fd_report)
    }

    fn build_with_fd_report(
        config: &LaunchConfig,
        task_state_dir: &Path,
        profile_enabled: bool,
        fd_report: RetainedFdReport,
    ) -> Result<Self> {
        let mut path_rules = Vec::new();
        path_rules.push(PathRule::new(
            PathCategory::PreparedRoot,
            config.task_rootfs.clone(),
            PathAccess::ReadOnly,
        ));
        for mount in &config.mounts {
            path_rules.push(bind_mount_rule(mount));
        }
        if let Some(mount) = &config.guest_init_override {
            path_rules.push(PathRule::new(
                PathCategory::GuestInitOverride,
                mount.source.clone(),
                if mount.read_only {
                    PathAccess::ReadOnly
                } else {
                    PathAccess::ReadWrite
                },
            ));
        }
        for disk in &config.disks {
            path_rules.push(disk_rule(disk));
        }
        if let Some(overlay) = &config.host_nix_overlay {
            path_rules.extend(host_nix_overlay_rules(overlay));
        }
        if let Some(managed_session) = &config.managed_session {
            path_rules.push(PathRule::new(
                PathCategory::ManagedSessionState,
                task_state_dir.to_path_buf(),
                PathAccess::ReadWrite,
            ));
            path_rules.push(managed_attach_socket_rule(&managed_session.attach_socket)?);
        }
        if profile_enabled {
            path_rules.push(PathRule::new(
                PathCategory::ProfileOutput,
                task_state_dir.to_path_buf(),
                PathAccess::ReadWrite,
            ));
        }
        path_rules.extend(libkrun_runtime_device_rules());
        if config.gpu_mode == GpuMode::Drm {
            path_rules.extend(gpu_runtime_device_rules());
            path_rules.extend(gpu_system_info_rules());
        }

        normalize_path_rules(&mut path_rules);
        compute_read_only_guarantees(&mut path_rules);

        Ok(Self {
            mode: config.landlock,
            path_rules,
            bind_tcp: BindTcpPolicy::for_mode(config.landlock, config)?,
            connect_tcp: ConnectTcpPolicy::UnrestrictedByDesign,
            ipc_scopes: vec![IpcScope::AbstractUnixSocket, IpcScope::Signal],
            audit: AuditPolicy {
                log_same_exec: true,
                log_new_exec: false,
                log_subdomains: true,
            },
            fd_report,
        })
    }

    pub(crate) fn report(&self) -> String {
        let path_summary = self
            .path_rules
            .iter()
            .map(|rule| {
                format!(
                    "{}:{}:{:?}:{:?}",
                    rule.category.as_report_label(),
                    rule.path.display(),
                    rule.access,
                    rule.read_only_guarantee
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let fds = self
            .fd_report
            .entries
            .iter()
            .chain(self.fd_report.unexpected.iter())
            .map(|entry| format!("{}:{}:{:?}", entry.fd, entry.target, entry.classification))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "mode={} fs_rules={} bind_tcp={} connect_tcp={:?} ipc_scopes={:?} audit={{same_exec:{},new_exec:{},subdomains:{}}} retained_fds=[{}]",
            self.mode.as_config_value(),
            path_summary,
            self.bind_tcp.report_value(),
            self.connect_tcp,
            self.ipc_scopes,
            self.audit.log_same_exec,
            self.audit.log_new_exec,
            self.audit.log_subdomains,
            fds,
        )
    }
}

impl PathRule {
    fn new(category: PathCategory, path: PathBuf, access: PathAccess) -> Self {
        Self {
            category,
            path,
            access,
            read_only_guarantee: match access {
                PathAccess::ReadOnly => ReadOnlyGuarantee::LandlockEnforced,
                PathAccess::ReadWrite => ReadOnlyGuarantee::NotReadOnly,
            },
        }
    }
}

impl PathCategory {
    fn as_report_label(&self) -> String {
        match self {
            Self::PreparedRoot => "prepared-root".to_owned(),
            Self::BindMount { target } => format!("bind:{target}"),
            Self::Disk { id } => format!("disk:{id}"),
            Self::GuestInitOverride => "guest-init-override".to_owned(),
            Self::HostNixOverlay { role } => format!("host-nix-overlay:{role:?}"),
            Self::ManagedAttachSocket => "managed-attach-socket".to_owned(),
            Self::ManagedSessionState => "managed-session-state".to_owned(),
            Self::ProfileOutput => "profile-output".to_owned(),
            Self::RuntimeDevice { name } => format!("runtime-device:{name}"),
            Self::GpuSystemInfo => "gpu-system-info".to_owned(),
        }
    }
}

fn bind_mount_rule(mount: &BindMount) -> PathRule {
    PathRule::new(
        PathCategory::BindMount {
            target: mount.target.clone(),
        },
        mount.source.clone(),
        if mount.read_only {
            PathAccess::ReadOnly
        } else {
            PathAccess::ReadWrite
        },
    )
}

fn disk_rule(disk: &DiskAttachment) -> PathRule {
    PathRule::new(
        PathCategory::Disk {
            id: disk.id.clone(),
        },
        disk.path.clone(),
        if disk.read_only {
            PathAccess::ReadOnly
        } else {
            PathAccess::ReadWrite
        },
    )
}

fn host_nix_overlay_rules(overlay: &HostNixOverlay) -> Vec<PathRule> {
    vec![
        PathRule::new(
            PathCategory::HostNixOverlay {
                role: HostNixOverlayRole::Lower,
            },
            overlay.lowerdir.clone(),
            PathAccess::ReadOnly,
        ),
        PathRule::new(
            PathCategory::HostNixOverlay {
                role: HostNixOverlayRole::Upper,
            },
            overlay.upperdir.clone(),
            PathAccess::ReadWrite,
        ),
        PathRule::new(
            PathCategory::HostNixOverlay {
                role: HostNixOverlayRole::Work,
            },
            overlay.workdir.clone(),
            PathAccess::ReadWrite,
        ),
        PathRule::new(
            PathCategory::HostNixOverlay {
                role: HostNixOverlayRole::Merged,
            },
            overlay.mergeddir.clone(),
            PathAccess::ReadOnly,
        ),
    ]
}

fn managed_attach_socket_rule(attach_socket: &Path) -> Result<PathRule> {
    let parent = attach_socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .with_context(|| {
            format!(
                "managed attach socket path '{}' has no parent directory",
                attach_socket.display()
            )
        })?;
    Ok(PathRule::new(
        PathCategory::ManagedAttachSocket,
        parent.to_path_buf(),
        PathAccess::ReadWrite,
    ))
}

fn libkrun_runtime_device_rules() -> Vec<PathRule> {
    runtime_device_rules_from(&[("kvm", PathBuf::from(LIBKRUN_KVM_DEVICE))])
}

/// ReadWrite access to the DRM render-node directory for the in-process
/// virglrenderer (vrend EGL/GL and VA-API video) that runs inside the VM
/// worker under `--gpu=drm`. The standalone venus render server keeps its own
/// broader `/dev` rule in `apply_render_server_rules`.
fn gpu_runtime_device_rules() -> Vec<PathRule> {
    runtime_device_rules_from(&[("dri", PathBuf::from(GPU_DRI_DEVICE_DIR))])
}

/// ReadOnly system-information mounts for the VM worker's in-process
/// virglrenderer under `--gpu=drm`. Libdrm resolves `/dev/dri/renderD*` by
/// reading PCI metadata under `/sys`, and Mesa inspects `/proc` while
/// initializing; without these the vrend winsys fails with
/// `MESA-LOADER: failed to retrieve device information`. Mirrors the `/sys`
/// and `/proc` rules granted to the standalone venus render server.
fn gpu_system_info_rules() -> Vec<PathRule> {
    vec![
        PathRule::new(
            PathCategory::GpuSystemInfo,
            PathBuf::from("/sys"),
            PathAccess::ReadOnly,
        ),
        PathRule::new(
            PathCategory::GpuSystemInfo,
            PathBuf::from("/proc"),
            PathAccess::ReadOnly,
        ),
    ]
}

fn runtime_device_rules_from(candidates: &[(&str, PathBuf)]) -> Vec<PathRule> {
    candidates
        .iter()
        .filter(|(_name, path)| path.exists())
        .map(|(name, path)| {
            PathRule::new(
                PathCategory::RuntimeDevice {
                    name: (*name).to_owned(),
                },
                path.clone(),
                PathAccess::ReadWrite,
            )
        })
        .collect()
}

fn normalize_path_rules(rules: &mut Vec<PathRule>) {
    rules.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| access_rank(right.access).cmp(&access_rank(left.access)))
            .then_with(|| {
                left.category
                    .as_report_label()
                    .cmp(&right.category.as_report_label())
            })
    });
    rules.dedup_by(|left, right| {
        left.path == right.path && left.access == right.access && left.category == right.category
    });
}

fn access_rank(access: PathAccess) -> u8 {
    match access {
        PathAccess::ReadOnly => 0,
        PathAccess::ReadWrite => 1,
    }
}

fn compute_read_only_guarantees(rules: &mut [PathRule]) {
    let writable_paths = rules
        .iter()
        .filter(|rule| rule.access == PathAccess::ReadWrite)
        .map(|rule| rule.path.clone())
        .collect::<Vec<_>>();
    for rule in rules {
        rule.read_only_guarantee = match rule.access {
            PathAccess::ReadWrite => ReadOnlyGuarantee::NotReadOnly,
            PathAccess::ReadOnly => {
                if writable_paths
                    .iter()
                    .any(|write_path| write_path != &rule.path && rule.path.starts_with(write_path))
                {
                    ReadOnlyGuarantee::MountEnforced
                } else {
                    ReadOnlyGuarantee::LandlockEnforced
                }
            }
        };
    }
}

fn bind_tcp_ports(config: &LaunchConfig) -> Result<Vec<u16>> {
    let mut ports = BTreeSet::new();
    for spec in &config.publish {
        if let Some(port) = parse_publish_host_port(spec)? {
            ports.insert(port);
        }
    }
    Ok(ports.into_iter().collect())
}

fn parse_publish_host_port(spec: &str) -> Result<Option<u16>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(None);
    }
    let payload = match spec.split_once(':') {
        Some((selector, payload)) if selector.eq_ignore_ascii_case("tcp") => payload,
        Some((selector, _)) if selector.eq_ignore_ascii_case("udp") => return Ok(None),
        _ => spec,
    };
    let Some((host, _guest)) = payload.split_once(':') else {
        return Ok(None);
    };
    if host.contains(['-', '~', '/', '%', ',']) || host.is_empty() {
        return Ok(None);
    }
    let port = host
        .parse::<u16>()
        .with_context(|| format!("failed to parse TCP publish host port from '{spec}'"))?;
    if port == 0 {
        return Ok(None);
    }
    Ok(Some(port))
}

fn retained_fd_report(mode: LandlockMode, passt_fd: Option<i32>) -> Result<RetainedFdReport> {
    let mut entries = Vec::new();
    let mut unexpected = Vec::new();
    let passt_fd = passt_fd.into_iter().collect::<HashSet<_>>();
    let fd_dir = match fs::read_dir(FD_DIR) {
        Ok(dir) => dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RetainedFdReport::default());
        }
        Err(err) => return Err(err).context("failed to inventory retained file descriptors"),
    };
    for entry in fd_dir {
        let entry = entry.context("failed to read retained file descriptor entry")?;
        let Some(fd) = entry.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let target = fs::read_link(entry.path())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unavailable>".to_owned());
        if let Some(classification) = classify_fd(fd, &target, &passt_fd) {
            entries.push(RetainedFd {
                fd,
                target,
                classification,
            });
        } else {
            let retained = RetainedFd {
                fd,
                target,
                classification: RetainedFdClass::Unexpected,
            };
            if mode.requires_full_enforcement() {
                bail!(
                    "cang Landlock {} mode refuses unexpected retained fd {} -> {}; close or categorize the descriptor before the VMM starts",
                    mode.as_config_value(),
                    retained.fd,
                    retained.target,
                );
            }
            unexpected.push(retained);
        }
    }
    entries.sort_by_key(|entry| entry.fd);
    unexpected.sort_by_key(|entry| entry.fd);
    Ok(RetainedFdReport {
        entries,
        unexpected,
    })
}

fn classify_fd(fd: i32, target: &str, passt_fds: &HashSet<i32>) -> Option<RetainedFdClass> {
    if (0..=2).contains(&fd) {
        return Some(RetainedFdClass::Stdio);
    }
    if passt_fds.contains(&fd) {
        return Some(RetainedFdClass::PasstSocket);
    }
    if target.starts_with("anon_inode:")
        || target.starts_with("socket:")
        || target.starts_with("pipe:")
    {
        return Some(RetainedFdClass::RuntimeKernelObject);
    }
    if target.starts_with("/proc/") && target.ends_with("/fd") {
        return Some(RetainedFdClass::RuntimeKernelObject);
    }
    if target == "/dev/null" || target == "/dev/tty" {
        return Some(RetainedFdClass::BenignDevice);
    }
    None
}

fn apply_policy(policy: &EffectivePolicy) -> Result<()> {
    let compat = policy.mode.compatibility();
    let fs_access = AccessFs::from_all(ABI::V5);
    let scopes = make_bitflags!(Scope::{AbstractUnixSocket | Signal});

    let mut ruleset = if policy.mode.handles_bind_tcp() {
        let net_access = make_bitflags!(AccessNet::{BindTcp});
        Ruleset::default()
            .set_compatibility(compat)
            .handle_access(fs_access)?
            .handle_access(net_access)?
            .scope(scopes)?
            .create()?
            .set_compatibility(compat)
    } else {
        Ruleset::default()
            .set_compatibility(compat)
            .handle_access(fs_access)?
            .scope(scopes)?
            .create()?
            .set_compatibility(compat)
    };

    for rule in &policy.path_rules {
        let path_fd = PathFd::new(&rule.path).with_context(|| {
            format!("failed to open Landlock path rule {}", rule.path.display())
        })?;
        let is_file = path_fd_points_to_file(&path_fd).with_context(|| {
            format!(
                "failed to inspect Landlock path rule {}",
                rule.path.display()
            )
        })?;
        ruleset = ruleset.add_rule(
            PathBeneath::new(path_fd, landlock_access_for(rule.access, is_file))
                .set_compatibility(compat),
        )?;
    }
    if let BindTcpPolicy::RestrictedPorts(ports) = &policy.bind_tcp {
        for port in ports {
            ruleset = ruleset
                .add_rule(NetPort::new(*port, AccessNet::BindTcp).set_compatibility(compat))?;
        }
    }

    let status = ruleset
        .log_same_exec(policy.audit.log_same_exec)?
        .log_new_exec(policy.audit.log_new_exec)?
        .log_subdomains(policy.audit.log_subdomains)?
        .restrict_self()?;
    ensure_status(policy.mode, status)
}

fn path_fd_points_to_file(path_fd: &PathFd) -> Result<bool> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe { libc::fstat(path_fd.as_fd().as_raw_fd(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to stat Landlock path fd");
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_mode & libc::S_IFMT) != libc::S_IFDIR)
}

fn landlock_access_for(access: PathAccess, is_file: bool) -> landlock::BitFlags<AccessFs> {
    let read = make_bitflags!(AccessFs::{Execute | ReadFile | ReadDir});
    let access = match access {
        PathAccess::ReadOnly => read,
        PathAccess::ReadWrite => {
            read | make_bitflags!(AccessFs::{
                WriteFile | RemoveDir | RemoveFile | MakeChar | MakeDir | MakeReg | MakeSock |
                MakeFifo | MakeBlock | MakeSym | Refer | Truncate | IoctlDev
            })
        }
    };
    if is_file {
        access & file_access_rights()
    } else {
        access
    }
}

fn file_access_rights() -> landlock::BitFlags<AccessFs> {
    make_bitflags!(AccessFs::{ReadFile | WriteFile | Execute | Truncate | IoctlDev})
}

/// Landlock confinement for the standalone venus render-server runner.
///
/// The runner executes on the host filesystem (it is spawned before the
/// network-enter helper) and dlopens the full mesa/vulkan-loader/virglrenderer
/// stack, whose transitive dependencies (libLLVM, libdrm, libxcb, ...) live in
/// separate immutable Nix store paths.  The rules therefore grant RO+Execute on
/// the whole `/nix/store` rather than trying to enumerate a closure, RW +
/// device ioctl on `/dev` (dri render node, urandom, null), and RO on `/sys`
/// and `/proc` (libdrm sysfs and loader inspection).
///
/// `/dev/shm` and `/tmp` get their own writable rules: landlock `PathBeneath`
/// rules are tied to the filesystem of the path, so the `/dev` rule does not
/// extend to the separate tmpfs mounted at `/dev/shm`, and Mesa/RADV writes its
/// shader cache and temporary blobs there during context creation.
///
/// TCP bind and connect are denied by handling `AccessNet::{BindTcp |
/// ConnectTcp}` and adding no `NetPort` rules.  This is defense in depth: the
/// seccomp allowlist already permits `socket`, `bind`, and `connect` (for the
/// venus AF_UNIX socketpair and Chromium's engine), so without a Landlock net
/// deny-all the render server could open TCP sockets.  UDP is a documented
/// residual — Landlock ABI V5 only covers TCP, and the seccomp policy permits
/// it.
pub(crate) fn apply_render_server_rules() -> Result<()> {
    let fs_access = AccessFs::from_all(ABI::V5);
    let net_access = make_bitflags!(AccessNet::{BindTcp | ConnectTcp});
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(fs_access)?
        .handle_access(net_access)?
        .create()?;
    for (path, access) in render_server_rule_paths() {
        let path_fd = PathFd::new(&path)
            .with_context(|| format!("failed to open Landlock rule {}", path.display()))?;
        ruleset = ruleset.add_rule(PathBeneath::new(path_fd, access))?;
    }
    let status = ruleset.restrict_self()?;
    tracing::debug!(?status, "render server Landlock restriction status");
    Ok(())
}

/// The render-server landlock rule set.
///
/// Landlock `PathBeneath` rules are tied to the filesystem of the path, so a
/// rule on `/dev` does NOT extend to the separate tmpfs mounted at `/dev/shm`
/// (nor to the `/tmp` mount). The render server needs writable scratch for
/// Mesa/RADV's shader cache and temporary blobs during context creation.
fn render_server_rule_paths() -> Vec<(PathBuf, landlock::BitFlags<AccessFs>)> {
    let read_execute = make_bitflags!(AccessFs::{Execute | ReadFile | ReadDir});
    let read = make_bitflags!(AccessFs::{ReadFile | ReadDir});
    let writable = make_bitflags!(AccessFs::{
        ReadFile | WriteFile | ReadDir | Execute | Truncate | MakeReg | MakeDir
        | RemoveFile | RemoveDir
    });
    vec![
        (PathBuf::from("/nix/store"), read_execute),
        // libdrm lists DRM render nodes by opening `/dev/dri` as a directory,
        // which requires ReadDir in addition to the file/device rights.
        (
            PathBuf::from("/dev"),
            file_access_rights() | make_bitflags!(AccessFs::{ReadDir}),
        ),
        (PathBuf::from("/sys"), read),
        (PathBuf::from("/proc"), read),
        // Separate mount points from `/dev` and `/`; writable scratch for Mesa.
        (PathBuf::from("/dev/shm"), writable),
        (PathBuf::from("/tmp"), writable),
    ]
}

fn ensure_status(mode: LandlockMode, status: RestrictionStatus) -> Result<()> {
    if mode.requires_full_enforcement()
        && (status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs)
    {
        bail!(
            "cang Landlock {} mode was not fully enforced: {status:?}",
            mode.as_config_value()
        );
    }
    tracing::debug!(?status, "cang internal: Landlock restriction status");
    Ok(())
}

#[cfg(test)]
pub(crate) fn ensure_fully_enforced_for_test(
    mode: LandlockMode,
    fully_enforced: bool,
) -> Result<()> {
    if mode.requires_full_enforcement() && !fully_enforced {
        bail!(
            "cang Landlock {} mode was not fully enforced",
            mode.as_config_value()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogLevel;
    use crate::runtime::launch::config::ManagedSessionConfig;
    use crate::runtime::launch::config::{BindMountSourceKind, NetworkMode};
    use crate::runtime::seccomp::SeccompMode;
    use crate::runtime::vm::gpu::GpuMode;

    fn test_config(task_rootfs: &Path) -> LaunchConfig {
        LaunchConfig {
            task_rootfs: task_rootfs.to_path_buf(),
            hostname: "cang-test".to_owned(),
            mounts: Vec::new(),
            host_nix_overlay: None,
            guest_init_override: None,
            disks: Vec::new(),
            ram_mib: 1024,
            vcpus: 1,
            log_level: LogLevel::Off,
            network_mode: NetworkMode::Tsi,
            gpu_mode: GpuMode::Off,
            new_perms: crate::runtime::launch::config::GuestPermissions::default(),
            publish: Vec::new(),
            workdir: "/workspace".to_owned(),
            exec_path: "/cang-guest-init".to_owned(),
            argv: Vec::new(),
            env: Vec::new(),
            guest_config_env: Vec::new(),
            passt_fd: None,
            pulse_bridge: None,
            waypipe: None,
            exec: None,
            managed_session: None,
            seccomp: SeccompMode::Off,
            landlock: LandlockMode::Relax,
        }
    }

    fn managed_session_config(attach_socket: PathBuf) -> ManagedSessionConfig {
        let guest_kernel_console_log = attach_socket.join("guest-kernel-console.log");
        ManagedSessionConfig {
            attach_socket,
            guest_port: 50_426,
            protocol_version: 1,
            attach_socket_uid: 1000,
            attach_socket_gid: 1000,
            cleanup_task_rootfs_on_exit: true,
            guest_kernel_console_log,
        }
    }

    #[test]
    fn parses_config_modes_with_relax_default_and_rejects_enforce() {
        assert_eq!(
            LandlockMode::parse_config_value(None).unwrap(),
            LandlockMode::Relax
        );
        assert_eq!(
            LandlockMode::parse_config_value(Some("all")).unwrap(),
            LandlockMode::All
        );
        assert_eq!(
            LandlockMode::parse_config_value(Some("relax")).unwrap(),
            LandlockMode::Relax
        );
        assert_eq!(
            LandlockMode::parse_config_value(Some("best-effort")).unwrap(),
            LandlockMode::BestEffort
        );
        assert_eq!(
            LandlockMode::parse_config_value(Some("off")).unwrap(),
            LandlockMode::Off
        );
        assert!(LandlockMode::parse_config_value(Some("enforce")).is_err());
        assert!(LandlockMode::parse_config_value(Some("audit")).is_err());
    }

    #[test]
    fn effective_policy_marks_connect_tcp_unrestricted_by_design() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        assert_eq!(policy.connect_tcp, ConnectTcpPolicy::UnrestrictedByDesign);
    }

    #[test]
    fn effective_policy_uses_kernel_default_audit_flags() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        assert_eq!(
            policy.audit,
            AuditPolicy {
                log_same_exec: true,
                log_new_exec: false,
                log_subdomains: true,
            }
        );
        assert!(
            policy
                .report()
                .contains("audit={same_exec:true,new_exec:false,subdomains:true}")
        );
    }

    #[test]
    fn effective_policy_preserves_read_only_and_read_write_path_intent() {
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        let rw = dir.path().join("rw");
        fs::create_dir_all(&ro).unwrap();
        fs::create_dir_all(&rw).unwrap();
        let mut config = test_config(dir.path().join("rootfs").as_path());
        config.mounts = vec![
            BindMount {
                source: ro.clone(),
                tag: "ro".to_owned(),
                target: "/ro".to_owned(),
                source_kind: BindMountSourceKind::Directory,
                read_only: true,
            },
            BindMount {
                source: rw.clone(),
                tag: "rw".to_owned(),
                target: "/rw".to_owned(),
                source_kind: BindMountSourceKind::Directory,
                read_only: false,
            },
        ];

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        let ro_rule = policy
            .path_rules
            .iter()
            .find(|rule| rule.path == ro)
            .unwrap();
        assert_eq!(ro_rule.access, PathAccess::ReadOnly);
        assert_eq!(
            ro_rule.read_only_guarantee,
            ReadOnlyGuarantee::LandlockEnforced
        );
        let rw_rule = policy
            .path_rules
            .iter()
            .find(|rule| rule.path == rw)
            .unwrap();
        assert_eq!(rw_rule.access, PathAccess::ReadWrite);
        assert_eq!(rw_rule.read_only_guarantee, ReadOnlyGuarantee::NotReadOnly);
    }

    #[test]
    fn read_only_child_reports_mount_enforced_when_parent_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        fs::create_dir_all(&child).unwrap();
        let config = test_config(child.as_path());

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            true,
            RetainedFdReport::default(),
        )
        .unwrap();
        let prepared_root = policy
            .path_rules
            .iter()
            .find(|rule| rule.category == PathCategory::PreparedRoot)
            .unwrap();

        assert_eq!(
            prepared_root.read_only_guarantee,
            ReadOnlyGuarantee::MountEnforced
        );
    }

    #[test]
    fn managed_sessions_allow_task_state_for_attach_socket_without_profile() {
        let dir = tempfile::tempdir().unwrap();
        let task_state = dir.path().join("task");
        let rootfs = dir.path().join("rootfs");
        fs::create_dir_all(&task_state).unwrap();
        fs::create_dir_all(&rootfs).unwrap();
        let mut config = test_config(&rootfs);
        config.managed_session = Some(managed_session_config(task_state.join("attach.sock")));

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            &task_state,
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        let state_rule = policy
            .path_rules
            .iter()
            .find(|rule| rule.category == PathCategory::ManagedSessionState)
            .expect("managed policy should allow per-task state");
        assert_eq!(state_rule.path, task_state);
        assert_eq!(state_rule.access, PathAccess::ReadWrite);
        assert_eq!(
            state_rule.read_only_guarantee,
            ReadOnlyGuarantee::NotReadOnly
        );
    }

    #[test]
    fn managed_sessions_allow_configured_attach_socket_parent_outside_task_state() {
        let dir = tempfile::tempdir().unwrap();
        let task_state = dir.path().join("task");
        let rootfs = dir.path().join("rootfs");
        let tmp = dir.path().join("tmp");
        let socket_parent = tmp.join("cang-1000");
        fs::create_dir_all(&task_state).unwrap();
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&socket_parent).unwrap();
        let mut config = test_config(&rootfs);
        config.managed_session = Some(managed_session_config(socket_parent.join("attach.sock")));

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            &task_state,
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        let attach_rule = policy
            .path_rules
            .iter()
            .find(|rule| rule.category == PathCategory::ManagedAttachSocket)
            .expect("managed policy should allow the attach socket parent");
        assert_eq!(attach_rule.path, socket_parent);
        assert_eq!(attach_rule.access, PathAccess::ReadWrite);
        assert_eq!(
            attach_rule.read_only_guarantee,
            ReadOnlyGuarantee::NotReadOnly
        );
        assert!(
            !policy
                .path_rules
                .iter()
                .any(|rule| rule.path == tmp && rule.access == PathAccess::ReadWrite),
            "managed attach socket rule must not grant broad tmp access"
        );
        assert!(
            policy.report().contains("managed-attach-socket"),
            "policy report should identify the attach socket rule"
        );
    }

    #[test]
    fn unmanaged_policy_does_not_allow_task_state_without_profile() {
        let dir = tempfile::tempdir().unwrap();
        let task_state = dir.path().join("task");
        let rootfs = dir.path().join("rootfs");
        let socket_parent = dir.path().join("tmp").join("cang-1000");
        fs::create_dir_all(&task_state).unwrap();
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&socket_parent).unwrap();
        let config = test_config(&rootfs);

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            &task_state,
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        assert!(
            !policy
                .path_rules
                .iter()
                .any(|rule| rule.path == task_state && rule.access == PathAccess::ReadWrite)
        );
        assert!(
            !policy
                .path_rules
                .iter()
                .any(|rule| rule.category == PathCategory::ManagedSessionState)
        );
        assert!(
            !policy
                .path_rules
                .iter()
                .any(|rule| rule.path == socket_parent && rule.access == PathAccess::ReadWrite)
        );
        assert!(
            !policy
                .path_rules
                .iter()
                .any(|rule| rule.category == PathCategory::ManagedAttachSocket)
        );
    }

    #[test]
    fn managed_attach_socket_requires_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let task_state = dir.path().join("task");
        let rootfs = dir.path().join("rootfs");
        fs::create_dir_all(&task_state).unwrap();
        fs::create_dir_all(&rootfs).unwrap();
        let mut config = test_config(&rootfs);
        config.managed_session = Some(managed_session_config(PathBuf::from("attach.sock")));

        let err = EffectivePolicy::build_with_fd_report(
            &config,
            &task_state,
            false,
            RetainedFdReport::default(),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("has no parent directory"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn bind_tcp_ports_include_simple_tcp_publish_and_ignore_connect_tcp() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.publish = vec![
            "8080:80".to_owned(),
            "tcp:8443:443".to_owned(),
            "udp:5353:5353".to_owned(),
            "10000-10010:80-90".to_owned(),
        ];

        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();

        assert_eq!(bind_tcp_ports(&config).unwrap(), vec![8080, 8443]);
        assert_eq!(policy.connect_tcp, ConnectTcpPolicy::UnrestrictedByDesign);
    }

    #[test]
    fn bind_tcp_policy_is_restricted_only_in_all_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.publish = vec!["8080:80".to_owned(), "tcp:8443:443".to_owned()];

        config.landlock = LandlockMode::All;
        let policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();
        assert_eq!(
            policy.bind_tcp,
            BindTcpPolicy::RestrictedPorts(vec![8080, 8443])
        );
        assert!(policy.report().contains("bind_tcp=restricted:[8080,8443]"));

        for mode in [LandlockMode::Relax, LandlockMode::BestEffort] {
            config.landlock = mode;
            let policy = EffectivePolicy::build_with_fd_report(
                &config,
                dir.path(),
                false,
                RetainedFdReport::default(),
            )
            .unwrap();
            assert_eq!(policy.bind_tcp, BindTcpPolicy::Unrestricted);
            assert!(policy.report().contains("bind_tcp=unrestricted"));
        }
    }

    #[test]
    fn strict_status_helper_fails_when_not_fully_enforced() {
        assert!(ensure_fully_enforced_for_test(LandlockMode::All, false).is_err());
        assert!(ensure_fully_enforced_for_test(LandlockMode::Relax, false).is_err());
        assert!(ensure_fully_enforced_for_test(LandlockMode::BestEffort, false).is_ok());
        assert!(ensure_fully_enforced_for_test(LandlockMode::All, true).is_ok());
        assert!(ensure_fully_enforced_for_test(LandlockMode::Relax, true).is_ok());
    }

    #[test]
    fn render_server_rules_grant_writable_scratch_mounts() {
        let rules = render_server_rule_paths();
        let rule_for = |path: &str| -> landlock::BitFlags<AccessFs> {
            rules
                .iter()
                .find(|(p, _)| p == Path::new(path))
                .unwrap_or_else(|| panic!("render server rules must include {path}"))
                .1
        };

        // `/dev/shm` and `/tmp` are separate mounts from `/dev` and `/`, so
        // they need explicit rules; Mesa/RADV writes its shader cache and
        // temporary blobs there during context creation.
        for path in ["/dev/shm", "/tmp"] {
            let access = rule_for(path);
            assert!(
                access.contains(AccessFs::WriteFile),
                "{path} must be writable"
            );
            assert!(
                access.contains(AccessFs::ReadFile),
                "{path} must be readable"
            );
            assert!(
                access.contains(AccessFs::ReadDir),
                "{path} must be listable"
            );
            assert!(
                access.contains(AccessFs::MakeReg) && access.contains(AccessFs::MakeDir),
                "{path} must allow creating files and directories"
            );
            // Mesa/RADV's shader-cache atomic write does `rename(.tmp, final)` then
            // `unlink(.tmp)`; those need the Remove* rights on the parent dir.
            assert!(
                access.contains(AccessFs::RemoveFile) && access.contains(AccessFs::RemoveDir),
                "{path} must allow removing files and directories for atomic cache writes"
            );
        }

        let store = rule_for("/nix/store");
        assert!(
            store.contains(AccessFs::Execute),
            "/nix/store must be executable for dlopen"
        );
        let dev = rule_for("/dev");
        assert!(
            dev.contains(AccessFs::IoctlDev),
            "/dev must allow device ioctl"
        );
        assert!(
            dev.contains(AccessFs::ReadDir),
            "/dev must be listable for DRM node discovery"
        );
        for path in ["/sys", "/proc"] {
            let access = rule_for(path);
            assert!(
                access.contains(AccessFs::ReadFile),
                "{path} must be readable"
            );
        }
    }

    #[test]
    fn landlock_access_for_regular_files_excludes_directory_only_rights() {
        let file_rw = landlock_access_for(PathAccess::ReadWrite, true);
        let directory_only = make_bitflags!(AccessFs::{
            ReadDir | RemoveDir | RemoveFile | MakeChar | MakeDir | MakeReg | MakeSock |
            MakeFifo | MakeBlock | MakeSym | Refer
        });

        assert_eq!(file_rw & directory_only, landlock::BitFlags::EMPTY);
        assert!(file_rw.contains(AccessFs::ReadFile));
        assert!(file_rw.contains(AccessFs::WriteFile));
        assert!(file_rw.contains(AccessFs::Truncate));
        assert!(file_rw.contains(AccessFs::IoctlDev));
    }

    #[test]
    fn runtime_device_rules_allow_existing_kvm_device_only() {
        let dir = tempfile::tempdir().unwrap();
        let device_parent = dir.path().join("dev");
        let kvm = device_parent.join("kvm");
        let missing = device_parent.join("missing");
        fs::create_dir_all(&device_parent).unwrap();
        fs::write(&kvm, b"test-device").unwrap();

        let rules =
            runtime_device_rules_from(&[("kvm", kvm.clone()), ("missing", missing.clone())]);

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].path, kvm);
        assert_eq!(rules[0].access, PathAccess::ReadWrite);
        assert_eq!(
            rules[0].category,
            PathCategory::RuntimeDevice {
                name: "kvm".to_owned()
            }
        );
        assert!(!rules.iter().any(|rule| rule.path == device_parent));
        assert!(!rules.iter().any(|rule| rule.path == missing));
    }

    #[test]
    fn gpu_runtime_device_rules_grant_read_write_dri_when_device_present() {
        let rules = gpu_runtime_device_rules();
        if Path::new(GPU_DRI_DEVICE_DIR).exists() {
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].path, PathBuf::from(GPU_DRI_DEVICE_DIR));
            assert_eq!(rules[0].access, PathAccess::ReadWrite);
            assert_eq!(
                rules[0].category,
                PathCategory::RuntimeDevice {
                    name: "dri".to_owned()
                }
            );
        } else {
            assert!(
                rules.is_empty(),
                "no DRM device directory on this host; gpu device rules must be empty"
            );
        }
    }

    #[test]
    fn vm_worker_policy_adds_dri_rule_only_for_gpu_mode_drm() {
        let is_dri_rule = |rule: &PathRule| matches!(&rule.category, PathCategory::RuntimeDevice { name } if name.as_str() == "dri");
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.gpu_mode = GpuMode::Off;

        let off_policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();
        assert!(
            !off_policy.path_rules.iter().any(is_dri_rule),
            "gpu device rule must not be granted when gpu mode is off"
        );

        config.gpu_mode = GpuMode::Drm;
        let drm_policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();
        let dri_rule_granted = drm_policy.path_rules.iter().any(is_dri_rule);
        if Path::new(GPU_DRI_DEVICE_DIR).exists() {
            assert!(
                dri_rule_granted,
                "gpu mode drm must grant the DRM device directory"
            );
        } else {
            assert!(
                !dri_rule_granted,
                "no DRM device directory on this host; nothing to grant"
            );
        }
    }

    #[test]
    fn vm_worker_policy_grants_read_only_sys_and_proc_only_for_gpu_mode_drm() {
        let is_sys_rule = |rule: &PathRule| matches!(&rule.category, PathCategory::GpuSystemInfo);
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.gpu_mode = GpuMode::Off;

        let off_policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();
        assert!(
            !off_policy.path_rules.iter().any(is_sys_rule),
            "system info rules must not be granted when gpu mode is off"
        );

        config.gpu_mode = GpuMode::Drm;
        let drm_policy = EffectivePolicy::build_with_fd_report(
            &config,
            dir.path(),
            false,
            RetainedFdReport::default(),
        )
        .unwrap();
        for path in ["/sys", "/proc"] {
            let rule = drm_policy
                .path_rules
                .iter()
                .find(|rule| rule.path == Path::new(path))
                .unwrap_or_else(|| panic!("gpu mode drm must grant read access to {path}"));
            assert_eq!(rule.category, PathCategory::GpuSystemInfo);
            assert_eq!(
                rule.access,
                PathAccess::ReadOnly,
                "{path} must be read-only for the VM worker"
            );
        }
    }

    #[test]
    fn gpu_system_info_report_label_is_explicit() {
        assert_eq!(
            PathCategory::GpuSystemInfo.as_report_label(),
            "gpu-system-info"
        );
    }

    #[test]
    fn render_server_rules_deny_all_tcp() {
        // Landlock net restrictions require kernel support for `AccessNet`
        // (Landlock ABI V5+); probe it with a HardRequirement net-only ruleset
        // and skip explicitly on older kernels (the seccomp allowlist remains
        // the enforcement fallback there).
        let net_supported = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(make_bitflags!(AccessNet::{BindTcp | ConnectTcp}))
            .and_then(|ruleset| ruleset.create())
            .is_ok();
        if !net_supported {
            eprintln!(
                "skipping render_server_rules_deny_all_tcp: kernel lacks Landlock net support"
            );
            return;
        }

        // A live loopback listener makes the child's connect attempt succeed
        // without Landlock, so a denied connect proves the net deny-all is
        // enforced rather than just failing for lack of a listener.
        // SAFETY: socket/bind/listen create a test-local TCP listener.
        let listener = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(listener >= 0, "failed to create listener socket");
        let bind_addr = loopback_tcp_addr(0);
        assert_eq!(
            unsafe {
                libc::bind(
                    listener,
                    &bind_addr as *const libc::sockaddr_in as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            },
            0,
            "failed to bind listener"
        );
        assert_eq!(unsafe { libc::listen(listener, 1) }, 0, "failed to listen");
        let mut bound = loopback_tcp_addr(0);
        let mut bound_len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        // SAFETY: getsockname writes the bound ephemeral port into `bound`.
        assert_eq!(
            unsafe {
                libc::getsockname(
                    listener,
                    &mut bound as *mut libc::sockaddr_in as *mut libc::sockaddr,
                    &mut bound_len,
                )
            },
            0,
            "getsockname failed"
        );

        // SAFETY: forks an isolated child that applies the render-server
        // landlock ruleset and attempts a TCP bind and connect; the parent
        // reaps it so no state is shared with the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let outcome = render_server_tcp_deny_probe(bound.sin_port);
            // SAFETY: _exit is async-signal-safe and skips destructors.
            unsafe { libc::_exit(outcome) };
        }
        let mut status = 0;
        // SAFETY: pid is the direct fork child created above.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        // SAFETY: closing the test's own listener socket.
        unsafe { libc::close(listener) };
        assert!(
            libc::WIFEXITED(status),
            "render server probe child terminated abnormally"
        );
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "render server landlock did not deny TCP bind/connect (probe exit {})",
            libc::WEXITSTATUS(status)
        );
    }

    /// Loopback `sockaddr_in` with the given port in network byte order.
    fn loopback_tcp_addr(port: u16) -> libc::sockaddr_in {
        libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: port.to_be(),
            sin_addr: libc::in_addr {
                s_addr: libc::INADDR_LOOPBACK.to_be(),
            },
            sin_zero: [0; 8],
        }
    }

    /// In an isolated child, apply the render-server landlock ruleset (fs +
    /// net deny-all, `CompatLevel::BestEffort`) and report via exit code
    /// whether a TCP bind and a connect to `connect_port` are both denied.
    fn render_server_tcp_deny_probe(connect_port: u16) -> i32 {
        let ruleset = match Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(ABI::V5))
            .and_then(|ruleset| {
                ruleset.handle_access(make_bitflags!(AccessNet::{BindTcp | ConnectTcp}))
            })
            .and_then(|ruleset| ruleset.create())
        {
            Ok(ruleset) => ruleset,
            Err(err) => {
                eprintln!("render server ruleset creation failed: {err:#}");
                return 2;
            }
        };
        if let Err(err) = ruleset.restrict_self() {
            eprintln!("render server restrict_self failed: {err:#}");
            return 3;
        }
        // SAFETY: socket creates a TCP socket in the restricted child.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            eprintln!("probe socket failed: {}", std::io::Error::last_os_error());
            return 4;
        }
        let bind_addr = loopback_tcp_addr(0);
        let connect_addr = loopback_tcp_addr(connect_port);
        let bind_rc = unsafe {
            libc::bind(
                fd,
                &bind_addr as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        let connect_rc = unsafe {
            libc::connect(
                fd,
                &connect_addr as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        // SAFETY: closing the probe socket.
        unsafe { libc::close(fd) };
        if bind_rc == 0 || connect_rc == 0 {
            eprintln!(
                "render server landlock did not deny TCP: bind_rc={bind_rc} connect_rc={connect_rc}"
            );
            return 5;
        }
        0
    }

    #[test]
    fn runtime_device_report_labels_are_explicit() {
        assert_eq!(
            PathCategory::RuntimeDevice {
                name: "kvm".to_owned()
            }
            .as_report_label(),
            "runtime-device:kvm"
        );
    }

    #[test]
    fn managed_session_state_report_label_is_explicit() {
        assert_eq!(
            PathCategory::ManagedSessionState.as_report_label(),
            "managed-session-state"
        );
    }

    #[test]
    fn managed_attach_socket_report_label_is_explicit() {
        assert_eq!(
            PathCategory::ManagedAttachSocket.as_report_label(),
            "managed-attach-socket"
        );
    }

    #[test]
    fn landlock_access_for_directories_keeps_hierarchy_rights() {
        let directory_rw = landlock_access_for(PathAccess::ReadWrite, false);

        assert!(directory_rw.contains(AccessFs::ReadDir));
        assert!(directory_rw.contains(AccessFs::MakeReg));
        assert!(directory_rw.contains(AccessFs::Refer));
    }

    #[test]
    fn landlock_path_fd_type_detection_distinguishes_files_from_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let file_fd = PathFd::new(file.path()).unwrap();
        let dir_fd = PathFd::new(dir.path()).unwrap();

        assert!(path_fd_points_to_file(&file_fd).unwrap());
        assert!(!path_fd_points_to_file(&dir_fd).unwrap());
    }

    #[test]
    fn fd_classifier_rejects_unexpected_regular_files() {
        assert_eq!(
            classify_fd(0, "/tmp/file", &HashSet::new()),
            Some(RetainedFdClass::Stdio)
        );
        assert_eq!(
            classify_fd(9, "socket:[123]", &HashSet::new()),
            Some(RetainedFdClass::RuntimeKernelObject)
        );
        assert_eq!(classify_fd(9, "/tmp/file", &HashSet::new()), None);
    }

    #[test]
    fn unexpected_fd_policy_is_strict_only() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let target = temp.path().display().to_string();
        let fd = 99;
        let report = RetainedFdReport {
            entries: Vec::new(),
            unexpected: vec![RetainedFd {
                fd,
                target: target.clone(),
                classification: RetainedFdClass::Unexpected,
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.landlock = LandlockMode::BestEffort;

        let policy = EffectivePolicy::build_with_fd_report(&config, dir.path(), false, report)
            .expect("best-effort should carry unexpected retained fd in report");

        assert_eq!(
            policy.fd_report.unexpected[0].classification,
            RetainedFdClass::Unexpected
        );
        assert_eq!(policy.fd_report.unexpected[0].target, target);
    }
}
