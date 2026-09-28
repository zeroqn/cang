use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::logging::{LogLevel, LogSettings};
use crate::runtime::landlock::LandlockMode;
use crate::runtime::launch::config::{AllocatorMode, GuestPermissions, NetworkMode, PulseServer};
use crate::runtime::seccomp::{AuditMode, SeccompCommand, SeccompMode};
use crate::runtime::vm::gpu::GpuMode;
use crate::task_rootfs::TaskRootfsBackend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeSpec {
    pub(crate) source: PathBuf,
    pub(crate) target: String,
    pub(crate) read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerStoreBackend {
    RawDisk,
}

impl ContainerStoreBackend {
    pub(crate) const DEFAULT: Self = Self::RawDisk;

    pub(crate) fn parse_config_value(value: &str) -> Result<Self, String> {
        match value {
            "raw-disk" => Ok(Self::RawDisk),
            _ => Err("allowed value is raw-disk".to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CliAllocatorMode {
    Mimalloc,
    Hardened,
    Glibc,
}

impl From<CliAllocatorMode> for AllocatorMode {
    fn from(value: CliAllocatorMode) -> Self {
        match value {
            CliAllocatorMode::Mimalloc => Self::Mimalloc,
            CliAllocatorMode::Hardened => Self::Hardened,
            CliAllocatorMode::Glibc => Self::Glibc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CliGpuMode {
    Off,
    Drm,
}

impl From<CliGpuMode> for GpuMode {
    fn from(value: CliGpuMode) -> Self {
        match value {
            CliGpuMode::Off => Self::Off,
            CliGpuMode::Drm => Self::Drm,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtyMode {
    Normalized,
    RawPassthrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PtyOptions {
    pub(crate) mode: PtyMode,
    pub(crate) trace: bool,
    pub(crate) suppress_focus_input: bool,
    pub(crate) focus_report_guard: bool,
}

impl PtyOptions {
    pub(crate) const DEFAULT: Self = Self {
        mode: PtyMode::Normalized,
        trace: false,
        suppress_focus_input: false,
        focus_report_guard: true,
    };
}

#[derive(Debug, Clone, Parser, PartialEq, Eq)]
#[command(
    name = "cang",
    version,
    about = "Launch a direct-libkrun microvm shell with the current directory mounted at /workspace",
    after_help = "Examples:\n  cang\n  cang --mem 8\n  cang --alloc glibc\n  cang --rootfs-backend btrfs-snapshot\n  cang --rootfs-backend fuse-overlay\n  cang --container-store raw-disk\n  cang --guest-init ./cang-guest-init\n  cang --profile\n  cang --root\n  cang --image ghcr.io/example/cang:dev\n  CANG_IMAGE=ghcr.io/example/cang:dev cang\n  cang -- bash -lc 'echo ok'\n  cang decode-launch-conf .cang/.../launch.conf
  cang --daemon
  cang --landlock=all -- bash -lc 'echo ok'
  cang --landlock=best-effort -- bash -lc 'echo ok'
  cang --seccomp=off -- bash -lc 'echo ok'
  cang images list
  cang images sync ghcr.io/example/cang:dev
  cang images sync ba5a514
  cang images remove --dry-run feedfacecafe
  cang images remove feedfacecafe
  cang images remove ghcr.io/example/cang:d
  cang container-store resize --size 128G
  cang container-store reset --force
  cang ps
  cang attach <task-id-or-handle-selector>
  cang a <task-id-or-handle-selector>
  cang kill <task-id-or-handle-selector>
  cang seccomp synthesize --input trace.jsonl --output policy.json"
)]
pub(crate) struct Cli {
    #[arg(
        long,
        env = "CANG_IMAGE",
        conflicts_with = "pull_latest",
        help = "Container image to run",
        long_help = "Container image to run. If omitted, cang prefers localhost/cang:latest and can fall back to ghcr.io/zeroqn/cang:latest in a future image-ingestion slice. Can also be set with CANG_IMAGE."
    )]
    image: Option<String>,

    #[arg(
        long,
        conflicts_with = "image",
        help = "Refresh and use ghcr.io/zeroqn/cang:latest for this run",
        long_help = "Refresh and use ghcr.io/zeroqn/cang:latest for this run when --image is not set. The future runtime implementation will perform this refresh through Buildah, not host Podman."
    )]
    pull_latest: bool,

    #[arg(
        long,
        help = "Enable cang debug logging",
        long_help = "Compatibility flag for cang debug logging. Equivalent to --log-level debug when --log-level/CANG_LOG_LEVEL is not set."
    )]
    debug: bool,

    #[arg(
        long = "log-level",
        env = "CANG_LOG_LEVEL",
        value_enum,
        value_name = "LEVEL",
        help = "Set cang/libkrun diagnostic log level",
        long_help = "Set cang and helper diagnostic log level. Allowed values are off, error, warn, info, debug, and trace. CLI --log-level overrides CANG_LOG_LEVEL; --debug remains a compatibility alias for debug when neither is set."
    )]
    log_level: Option<LogLevel>,

    #[arg(
        long,
        help = "Enable cang component timing collection",
        long_help = "Enable cang component timing collection. Timing reports are emitted to stderr when profiling is requested, so normal command stdout remains reserved for command output."
    )]
    profile: bool,

    #[arg(
        long,
        help = "Enter the task shell as root",
        long_help = "Enter the task shell as root instead of dropping to the host/dev identity. By default, cang drops privileges for the interactive shell."
    )]
    root: bool,

    #[arg(
        long,
        help = "Start the managed task through this TTY, then detach after initial output becomes idle",
        long_help = "Start the managed task through the launching terminal, wait for the guest PTY target to emit initial output and become briefly idle, then detach while leaving the task running for cang attach. This requires a TTY because terminal initialization queries must be answered by the real terminal."
    )]
    daemon: bool,

    #[arg(
        long = "pty",
        value_name = "[MODE,][trace][,no-focus-input][,focus-report-guard]",
        value_parser = parse_pty_arg,
        help = "Configure managed PTY diagnostics for this launched task",
        long_help = "Configure managed PTY diagnostics for this launched task. Allowed mode values are normalize and raw; omit the mode to use normalize with modifier-only forms such as trace, no-focus-input, or focus-report-guard. Add trace to enable cang-terminal.trace; add no-focus-input to suppress all host terminal focus reports (ESC[I and ESC[O). The bounded focus-report guard is enabled by default and suppresses exact focus reports only for a short window after the guest enables or reasserts focus reporting (ESC[?1004h); add focus-report-guard only for explicitness. The default is normalize with tracing disabled and bounded focus-report guarding enabled. The trace token writes cang-terminal.trace in the host current working directory for the new launch; guest-init writes the same workspace file through /workspace/cang-terminal.trace. Raw, trace, no-focus-input, and focus-report-guard are independent diagnostics."
    )]
    pty: Option<PtyOptions>,

    #[arg(
        long = "seccomp",
        value_name = "MODE[:POLICY]:PATH",
        value_parser = parse_seccomp_arg,
        help = "Configure host-side cang seccomp mode for this run",
        long_help = "Configure host-side cang seccomp mode for this run. Allowed values are off, audit:TRACE_JSONL, trace:TRACE_JSONL, audit:POLICY_JSON:MISSING_TRACE_JSONL, trace:POLICY_JSON:MISSING_TRACE_JSONL, audit-default:MISSING_TRACE_JSONL, trace-default:MISSING_TRACE_JSONL, and enforce:POLICY_JSON. If omitted for a normal task launch, cang enforces the packaged default policy at $out/share/cang/seccomp/default.json and fails closed if that policy cannot be loaded; pass --seccomp=off to opt out. Audit mode uses strace/ptrace on the VM worker only to write a tracer-owned record file; gap audit records syscalls missing from the baseline policy by syscall name; audit-default and trace-default use the packaged default policy as the baseline and fail closed if it cannot be loaded; enforce mode applies a seccompiler JSON policy in the VM worker immediately before the VMM is built."
    )]
    seccomp: Option<SeccompMode>,
    #[arg(
        long = "landlock",
        value_enum,
        value_name = "MODE",
        help = "Configure host-side cang Landlock mode for this run",
        long_help = "Configure host-side cang Landlock mode for this run. Allowed values are all, relax, best-effort, and off. If omitted for a normal task launch, cang uses relax: filesystem, device ioctl, IPC scope, and audit-flag Landlock coverage remain fail-closed, but TCP BindTcp is not handled so guest-local listeners can bind without publishing a host port. all preserves the stricter BindTcp policy and allows only simple TCP --publish-derived host ports. best-effort uses the relax policy shape but applies the supported subset and reports degraded coverage when the current kernel lacks target Landlock features. off disables this host VM-worker Landlock layer. Landlock is applied to the VM worker before seccomp and before the VMM is built; it does not confine the guest kernel, guest Podman, or helper/network-manager/passt processes started before the VM worker."
    )]
    landlock: Option<LandlockMode>,

    #[arg(
        long = "new-perms",
        value_name = "PERMISSION[,PERMISSION...]",
        value_parser = parse_permissions_arg,
        help = "Grant additional optional permissions inside the guest VM",
        long_help = "Grant comma-separated additional permissions inside the guest VM. Allowed values are io-uring, net-admin, net-raw, bpf, perf, and sys-admin. io-uring and perf relax their existing guest kernel policies; net-admin authorizes CAP_NET_ADMIN, net-raw authorizes CAP_NET_RAW, bpf authorizes CAP_BPF, and sys-admin authorizes CAP_SYS_ADMIN for commands launched through cang-granted. No optional permission is enabled by default."
    )]
    new_perms: Option<GuestPermissions>,

    #[arg(
        long = "alloc",
        value_enum,
        value_name = "ALLOCATOR",
        default_value = "mimalloc",
        help = "Select the allocator for Nix-linked dynamic binaries",
        long_help = "Select the allocator for Nix-linked dynamic binaries. Allowed values are mimalloc, hardened, and glibc. mimalloc is the default; hardened selects GrapheneOS hardened_malloc; glibc disables /etc/ld-nix.so.preload so binaries use the standard glibc allocator. Foreign/FHS, static, and musl binaries are unchanged."
    )]
    alloc: CliAllocatorMode,

    #[arg(
        long = "rootfs-backend",
        value_name = "BACKEND",
        value_parser = parse_rootfs_backend_arg,
        help = "Override the task rootfs backend for this run",
        long_help = "Override the task rootfs backend for this run. Allowed values are btrfs-snapshot and fuse-overlay. If omitted, cang uses [task-rootfs].backend from cang.toml or defaults to btrfs-snapshot."
    )]
    rootfs_backend: Option<TaskRootfsBackend>,

    #[arg(
        long = "container-store",
        value_name = "BACKEND",
        value_parser = parse_container_store_backend_arg,
        help = "Override the nested Podman container-store backend for this run",
        long_help = "Override the nested Podman container-store backend for this run. The only allowed value is raw-disk, which is also the default."
    )]
    container_store_backend: Option<ContainerStoreBackend>,

    #[arg(
        long = "guest-init",
        value_name = "PATH",
        help = "Override cang-guest-init in the microvm image"
    )]
    guest_init: Option<PathBuf>,

    #[arg(long = "preserve-debug", help = "Preserve task debug state after exit")]
    preserve_debug: bool,

    #[arg(
        long = "mem",
        value_name = "GiB",
        value_parser = parse_mem_gib_arg,
        help = "Set microvm memory in GiB"
    )]
    mem_gib: Option<u32>,

    #[arg(
        long = "tsi",
        help = "Use libkrun virtio-vsock/TSI proxy networking instead of the default virtio-net/passt mode"
    )]
    tsi: bool,

    #[arg(
        long = "pulse",
        value_name = "tcp:HOST:PORT",
        help = "Direct guest Pulse clients to a TCP endpoint or bridge localhost to host loopback"
    )]
    pulse: Option<PulseServer>,

    #[arg(
        long = "gpu",
        value_enum,
        value_name = "MODE",
        default_value = "off",
        help = "Configure libkrun virtio-gpu mode",
        long_help = "Configure libkrun virtio-gpu mode. Allowed values are off and drm. Wayland passthrough requires drm and enables it automatically."
    )]
    gpu: CliGpuMode,

    #[arg(
        long = "zero-copy-shm",
        help = "Ask the guest for the udmabuf zero-copy shared-memory fast path",
        long_help = "Ask the virtio-gpu device for the udmabuf zero-copy shared-memory fast path (wl_shm pools imported as guest blobs with host handles). Requires --gpu=drm. When the host cannot serve it (no /dev/udmabuf) cang logs why and the guest runs the copy path; guest RAM stays anonymous, so the virtio-balloon keeps reclaiming host memory. On fast-path runs the balloon has nothing to reclaim: guest RAM is file-backed and shared."
    )]
    zero_copy_shm: bool,

    #[arg(
        long = "wayland",
        help = "Enable guest Wayland passthrough through wl-cross-domain-proxy",
        long_help = "Enable guest Wayland passthrough through wl-cross-domain-proxy. This starts a guest-local Wayland proxy and enables libkrun virtio-gpu DRM native-context support."
    )]
    wayland: bool,

    #[arg(
        long,
        value_name = "WORKSPACE",
        value_parser = parse_workspace_arg,
        help = "Select the host directory mounted at /workspace",
        long_help = "Select the absolute host directory mounted at /workspace. If omitted, cang uses the current working directory."
    )]
    workspace: Option<PathBuf>,

    #[arg(
        long = "waypipe",
        value_name = "SOCKET",
        value_parser = parse_waypipe_arg,
        num_args = 0..=1,
        require_equals = true,
        conflicts_with_all = ["wayland"],
        help = "Enable persistent remote Waypipe forwarding, optionally activating an existing SSH-forwarded socket"
    )]
    waypipe: Option<Option<PathBuf>>,

    #[arg(
        short = 'p',
        long = "publish",
        value_name = "SPEC",
        value_parser = parse_publish_arg,
        action = ArgAction::Append,
        help = "Publish a host port to the guest; repeatable",
        long_help = "Publish a host port to the guest; repeatable. Default passt mode accepts passt forwarding syntax; tcp:SPEC and udp:SPEC select passt TCP/UDP forwarding, and unprefixed SPEC defaults to TCP. With --tsi, only simple TCP HOST_PORT:GUEST_PORT mappings are accepted."
    )]
    publish: Vec<String>,

    #[arg(
        short = 'v',
        long = "volume",
        value_name = "SOURCE:TARGET[:ro|:rw]",
        value_parser = parse_volume_arg,
        action = ArgAction::Append,
        help = "Bind-mount a host file or directory into the guest; repeatable",
        long_help = "Bind-mount a host file or directory into the guest; repeatable. Syntax is SOURCE:TARGET, SOURCE:TARGET:rw, or SOURCE:TARGET:ro. Omitted mode defaults to read-write. SELinux, ownership, and propagation options are not supported."
    )]
    volumes: Vec<VolumeSpec>,

    #[arg(
        value_name = "COMMAND",
        last = true,
        num_args = 1..,
        allow_hyphen_values = true,
        help = "Run command inside the guest instead of the default fish login shell"
    )]
    guest_command: Vec<String>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub(crate) enum CliCommand {
    #[command(
        name = "container-store",
        about = "Maintain cang's workspace-scoped raw container-store disk"
    )]
    ContainerStore {
        #[command(subcommand)]
        command: ContainerStoreCommand,
    },

    #[command(
        name = "decode-launch-conf",
        about = "Decode a hex-encoded cang launch.conf for debugging"
    )]
    DecodeLaunchConf {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    #[command(
        name = "images",
        about = "Manage cang's local Buildah-backed image snapshot cache"
    )]
    Images {
        #[command(subcommand)]
        command: ImagesCommand,
    },

    #[command(name = "ps", about = "List active cang task VMs across workspaces")]
    Ps,

    #[command(name = "exec", about = "Run a command in an active cang task VM")]
    Exec {
        #[arg(value_name = "TASK")]
        task_id: String,

        #[arg(
            value_name = "COMMAND",
            required = true,
            num_args = 1..,
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        command: Vec<String>,
    },

    #[command(
        name = "attach",
        visible_alias = "a",
        about = "Attach to an active cang task VM session"
    )]
    Attach {
        #[arg(value_name = "TASK_ID_OR_HANDLE_SELECTOR")]
        task_id: String,
    },

    #[command(name = "kill", about = "Terminate an active cang task VM")]
    Kill {
        #[arg(value_name = "TASK_ID_OR_HANDLE_SELECTOR")]
        task_id: String,
    },

    #[command(
        name = "seccomp",
        about = "Audit and synthesize cang host-side seccomp policies"
    )]
    Seccomp {
        #[command(subcommand)]
        command: SeccompCommand,
    },
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub(crate) enum ContainerStoreCommand {
    #[command(
        name = "resize",
        about = "Grow the raw container-store disk and guest btrfs filesystem"
    )]
    Resize {
        #[arg(
            long = "size",
            value_name = "SIZE",
            help = "New grow-only container-store disk size, for example 128G"
        )]
        size: String,
    },

    #[command(
        name = "reset",
        about = "Delete and recreate the raw container-store disk"
    )]
    Reset {
        #[arg(long = "force", action = ArgAction::SetTrue, help = "Confirm destructive container-store disk reset")]
        force: bool,
    },
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub(crate) enum ImagesCommand {
    #[command(
        name = "sync",
        about = "Sync one Buildah image reference or unique local image selector into cang's local image cache"
    )]
    Sync {
        #[arg(value_name = "REFERENCE_OR_SELECTOR")]
        reference: String,
    },

    #[command(
        name = "list",
        about = "List cang's local image cache and Buildah image rows"
    )]
    List,

    #[command(
        name = "remove",
        about = "Remove a cang image cache entry by unique visible image selector"
    )]
    Remove {
        #[arg(
            long = "dry-run",
            action = ArgAction::SetTrue,
            help = "Preview the exact cache entry and local Buildah image removal without mutating state",
            long_help = "Preview the exact cang cache entry and final guarded local Buildah image target that would be removed, without mutating cache or local Buildah state. Dry-run is stricter than real remove: it fails when local Buildah removal would be skipped."
        )]
        dry_run: bool,

        #[arg(value_name = "IMAGE_SELECTOR")]
        target: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliAction {
    Run(RuntimeOptions),
    DecodeLaunchConf {
        path: PathBuf,
    },
    ContainerStore {
        command: ContainerStoreCommand,
        options: ContainerStoreOptions,
    },
    Images {
        command: ImagesCommand,
        log_settings: LogSettings,
    },
    Ps {
        log_settings: LogSettings,
    },
    Exec {
        task_id: String,
        command: Vec<String>,
        waypipe: Option<Option<PathBuf>>,
        log_settings: LogSettings,
    },
    Kill {
        task_id: String,
        log_settings: LogSettings,
    },
    Attach {
        task_id: String,
        log_settings: LogSettings,
    },
    Seccomp {
        command: SeccompCommand,
        log_settings: LogSettings,
    },
}

impl Cli {
    pub(crate) fn into_action(self) -> CliAction {
        if let Some(command) = self.command.clone() {
            match command {
                CliCommand::ContainerStore { command } => CliAction::ContainerStore {
                    command,
                    options: self.container_store_options(),
                },
                CliCommand::DecodeLaunchConf { path } => CliAction::DecodeLaunchConf { path },
                CliCommand::Images { command } => CliAction::Images {
                    command,
                    log_settings: LogSettings::from_process_env(self.log_level, self.debug),
                },
                CliCommand::Ps => CliAction::Ps {
                    log_settings: LogSettings::from_process_env(self.log_level, self.debug),
                },
                CliCommand::Exec { task_id, command } => CliAction::Exec {
                    task_id,
                    command,
                    waypipe: self.waypipe,
                    log_settings: LogSettings::from_process_env(self.log_level, self.debug),
                },
                CliCommand::Kill { task_id } => CliAction::Kill {
                    task_id,
                    log_settings: LogSettings::from_process_env(self.log_level, self.debug),
                },
                CliCommand::Attach { task_id } => CliAction::Attach {
                    task_id,
                    log_settings: LogSettings::from_process_env(self.log_level, self.debug),
                },
                CliCommand::Seccomp { command } => CliAction::Seccomp {
                    command,
                    log_settings: LogSettings::from_process_env(self.log_level, self.debug),
                },
            }
        } else {
            CliAction::Run(self.into_runtime_options())
        }
    }

    fn container_store_options(&self) -> ContainerStoreOptions {
        ContainerStoreOptions {
            image: self.image.clone(),
            pull_latest: self.pull_latest,
            guest_init: self.guest_init.clone(),
            mem_gib: self.mem_gib,
            log_settings: LogSettings::from_process_env(self.log_level, self.debug),
        }
    }

    pub(crate) fn into_runtime_options(self) -> RuntimeOptions {
        let log_settings = LogSettings::from_process_env(self.log_level, self.debug);
        RuntimeOptions {
            image: self.image,
            pull_latest: self.pull_latest,
            debug: self.debug,
            log_settings,
            profile: self.profile,
            root: self.root,
            daemon: self.daemon,
            pty: self.pty.unwrap_or(PtyOptions::DEFAULT),
            seccomp: self.seccomp,
            landlock: self.landlock,
            new_perms: self.new_perms.unwrap_or_default(),
            allocator: self.alloc.into(),
            rootfs_backend: self.rootfs_backend,
            container_store_backend: self.container_store_backend,
            guest_init: self.guest_init,
            preserve_debug: self.preserve_debug,
            mem_gib: self.mem_gib,
            network_mode: if self.tsi {
                NetworkMode::Tsi
            } else {
                NetworkMode::Passt
            },
            pulse: self.pulse,
            gpu_mode: if self.wayland {
                GpuMode::Drm
            } else {
                self.gpu.into()
            },
            zero_copy_shm: self.zero_copy_shm,
            wayland: self.wayland,
            workspace: self.workspace,
            waypipe: self.waypipe,
            publish: self.publish,
            volumes: self.volumes,
            guest_command: self.guest_command,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerStoreOptions {
    pub(crate) image: Option<String>,
    pub(crate) pull_latest: bool,
    pub(crate) guest_init: Option<PathBuf>,
    pub(crate) mem_gib: Option<u32>,
    pub(crate) log_settings: LogSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeOptions {
    pub(crate) image: Option<String>,
    pub(crate) pull_latest: bool,
    pub(crate) debug: bool,
    pub(crate) log_settings: LogSettings,
    pub(crate) profile: bool,
    pub(crate) root: bool,
    pub(crate) daemon: bool,
    pub(crate) pty: PtyOptions,
    pub(crate) seccomp: Option<SeccompMode>,
    pub(crate) landlock: Option<LandlockMode>,
    pub(crate) new_perms: GuestPermissions,
    pub(crate) allocator: AllocatorMode,
    pub(crate) rootfs_backend: Option<TaskRootfsBackend>,
    pub(crate) container_store_backend: Option<ContainerStoreBackend>,
    pub(crate) guest_init: Option<PathBuf>,
    pub(crate) preserve_debug: bool,
    pub(crate) mem_gib: Option<u32>,
    pub(crate) network_mode: NetworkMode,
    pub(crate) pulse: Option<PulseServer>,
    pub(crate) gpu_mode: GpuMode,
    pub(crate) zero_copy_shm: bool,
    pub(crate) wayland: bool,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) waypipe: Option<Option<PathBuf>>,
    pub(crate) publish: Vec<String>,
    pub(crate) volumes: Vec<VolumeSpec>,
    pub(crate) guest_command: Vec<String>,
}

fn parse_permissions_arg(value: &str) -> Result<GuestPermissions, String> {
    value.parse()
}

pub(crate) fn parse_mem_gib_arg(value: &str) -> Result<u32, String> {
    let mem_gib = value
        .parse::<u32>()
        .map_err(|_| "memory must be a positive integer GiB value".to_owned())?;

    if mem_gib == 0 {
        return Err("memory must be greater than 0 GiB".to_owned());
    }

    Ok(mem_gib)
}

fn parse_rootfs_backend_arg(value: &str) -> Result<TaskRootfsBackend, String> {
    TaskRootfsBackend::parse_config_value(value)
}

fn parse_container_store_backend_arg(value: &str) -> Result<ContainerStoreBackend, String> {
    ContainerStoreBackend::parse_config_value(value)
}

fn parse_pty_arg(value: &str) -> Result<PtyOptions, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(
            "pty value must include normalize, raw, trace, no-focus-input, or focus-report-guard"
                .to_owned(),
        );
    }

    let mut mode = None;
    let mut trace = false;
    let mut suppress_focus_input = false;
    let mut focus_report_guard = true;
    let mut focus_report_guard_token_seen = false;
    for token in value.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err("pty mode tokens must not be empty".to_owned());
        }
        match token {
            "normalize" => {
                if mode.replace(PtyMode::Normalized).is_some() {
                    return Err("pty mode must specify at most one of normalize or raw".to_owned());
                }
            }
            "raw" => {
                if mode.replace(PtyMode::RawPassthrough).is_some() {
                    return Err("pty mode must specify at most one of normalize or raw".to_owned());
                }
            }
            "trace" => {
                if trace {
                    return Err("pty trace token must not be duplicated".to_owned());
                }
                trace = true;
            }
            "no-focus-input" => {
                if suppress_focus_input {
                    return Err("pty no-focus-input token must not be duplicated".to_owned());
                }
                suppress_focus_input = true;
            }
            "focus-report-guard" => {
                if focus_report_guard_token_seen {
                    return Err("pty focus-report-guard token must not be duplicated".to_owned());
                }
                focus_report_guard_token_seen = true;
                focus_report_guard = true;
            }
            other => {
                return Err(format!(
                    "unsupported pty token '{other}'; use normalize, raw, trace, no-focus-input, or focus-report-guard"
                ));
            }
        }
    }

    Ok(PtyOptions {
        mode: mode.unwrap_or(PtyMode::Normalized),
        trace,
        suppress_focus_input,
        focus_report_guard,
    })
}

fn parse_seccomp_arg(value: &str) -> Result<SeccompMode, String> {
    let value = value.trim();
    if value == "off" {
        return Ok(SeccompMode::Off);
    }
    let parts = value.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        ["audit" | "trace", trace_path] if !trace_path.trim().is_empty() => {
            Ok(SeccompMode::Audit(AuditMode::Full {
                trace_path: PathBuf::from(trace_path),
            }))
        }
        ["audit-default" | "trace-default", trace_path] if !trace_path.trim().is_empty() => {
            Ok(SeccompMode::Audit(AuditMode::DefaultGap {
                trace_path: PathBuf::from(trace_path),
            }))
        }
        ["audit" | "trace", baseline_policy_path, trace_path]
            if !baseline_policy_path.trim().is_empty() && !trace_path.trim().is_empty() =>
        {
            Ok(SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path: PathBuf::from(baseline_policy_path),
                trace_path: PathBuf::from(trace_path),
            }))
        }
        ["enforce", policy_path] if !policy_path.trim().is_empty() => Ok(SeccompMode::Enforce {
            policy_path: PathBuf::from(policy_path),
        }),
        ["audit" | "trace", ..] => Err(
            "seccomp audit mode must be audit:TRACE_JSONL or audit:POLICY_JSON:TRACE_JSONL"
                .to_owned(),
        ),
        ["audit-default" | "trace-default", ..] => {
            Err("seccomp default gap audit mode must be audit-default:TRACE_JSONL or trace-default:TRACE_JSONL".to_owned())
        }
        ["enforce", ..] => Err("seccomp enforce mode must be enforce:POLICY_JSON".to_owned()),
        _ => Err(
            "seccomp mode must be off, audit, trace, audit-default, trace-default, or enforce"
                .to_owned(),
        ),
    }
}

fn parse_workspace_arg(value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("workspace path must not be empty".to_owned());
    }

    let workspace = PathBuf::from(value);
    if !workspace.is_absolute() {
        return Err("workspace must be an absolute path".to_owned());
    }

    Ok(workspace)
}

fn parse_waypipe_arg(value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("waypipe socket path must not be empty".to_owned());
    }

    let socket = PathBuf::from(value);
    if !socket.is_absolute() {
        return Err("waypipe socket must be an absolute path".to_owned());
    }

    Ok(socket)
}

fn parse_publish_arg(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("publish spec must not be empty".to_owned());
    }
    Ok(value.to_owned())
}

fn parse_volume_arg(value: &str) -> Result<VolumeSpec, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("volume spec must not be empty".to_owned());
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() < 2 {
        return Err("volume spec must use SOURCE:TARGET syntax".to_owned());
    }
    if parts.len() > 3 {
        return Err(
            "volume spec supports only SOURCE:TARGET, SOURCE:TARGET:ro, or SOURCE:TARGET:rw"
                .to_owned(),
        );
    }
    let source = parts[0].trim();
    let target = parts[1].trim();
    if source.is_empty() {
        return Err("volume source must not be empty".to_owned());
    }
    if target.is_empty() {
        return Err("volume target must not be empty".to_owned());
    }
    let read_only = match parts.get(2).map(|part| part.trim()) {
        None | Some("rw") => false,
        Some("ro") => true,
        Some(option) => {
            return Err(format!(
                "volume option '{option}' is not supported; use ro or rw"
            ));
        }
    };
    Ok(VolumeSpec {
        source: PathBuf::from(source),
        target: target.to_owned(),
        read_only,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::path::PathBuf;

    use crate::cli::{Cli, PtyMode, PtyOptions};
    use crate::logging::LogLevel;
    use crate::runtime::landlock::LandlockMode;
    use crate::runtime::launch::config::NetworkMode;
    use crate::runtime::seccomp::{AuditMode, SeccompCommand, SeccompMode};
    use crate::task_rootfs::TaskRootfsBackend;

    #[test]
    fn parses_single_runtime_options_without_subcommand() {
        let cli = Cli::try_parse_from([
            "cang",
            "--rootfs-backend",
            "fuse-overlay",
            "--mem",
            "8",
            "--guest-init",
            "./cang-guest-init",
            "--preserve-debug",
            "--root",
            "--profile",
            "--debug",
        ])
        .expect("single runtime options should parse");
        let options = cli.into_runtime_options();

        assert_eq!(options.rootfs_backend, Some(TaskRootfsBackend::FuseOverlay));
        assert_eq!(options.container_store_backend, None);
        assert_eq!(options.mem_gib, Some(8));
        assert_eq!(
            options.guest_init.as_deref(),
            Some("./cang-guest-init".as_ref())
        );
        assert!(options.preserve_debug);
        assert!(options.root);
        assert!(!options.daemon);
        assert_eq!(options.seccomp, None);
        assert_eq!(options.landlock, None);
        assert!(options.new_perms.is_empty());
        assert!(options.profile);
        assert!(options.debug);
        assert_eq!(options.pty, PtyOptions::DEFAULT);
        assert!(!options.pty.suppress_focus_input);
        assert!(options.pty.focus_report_guard);
        assert_eq!(options.network_mode, NetworkMode::Passt);
        assert!(options.publish.is_empty());
        assert!(options.volumes.is_empty());
        assert_eq!(options.log_settings.level, LogLevel::Debug);
        assert!(options.guest_command.is_empty());
    }

    #[test]
    fn parses_attach_subcommand() {
        let cli =
            Cli::try_parse_from(["cang", "attach", "workspace-123"]).expect("attach should parse");
        let alias =
            Cli::try_parse_from(["cang", "a", "workspace-123"]).expect("a alias should parse");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Attach { task_id, .. } if task_id == "workspace-123"
        ));
        assert!(matches!(
            alias.into_action(),
            crate::cli::CliAction::Attach { task_id, .. } if task_id == "workspace-123"
        ));
    }

    #[test]
    fn exec_requires_task_and_command() {
        let err = Cli::try_parse_from(["cang", "exec"])
            .expect_err("exec without task and command should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        let err = Cli::try_parse_from(["cang", "exec", "task-a"])
            .expect_err("exec without command should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn exec_parses_trailing_command_arguments() {
        let cli = Cli::try_parse_from([
            "cang",
            "exec",
            "task-a",
            "--",
            "printf",
            "--format=%s",
            "hello world",
        ])
        .expect("exec should parse");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Exec { task_id, command, .. }
                if task_id == "task-a"
                    && command == ["printf", "--format=%s", "hello world"]
        ));
    }

    #[test]
    fn waypipe_capability_without_initial_target_parses() {
        let options = Cli::try_parse_from(["cang", "--waypipe"])
            .expect("valueless Waypipe capability should parse")
            .into_runtime_options();

        assert_eq!(options.waypipe, Some(None));
    }

    #[test]
    fn valueless_waypipe_exec_reuses_running_server() {
        let cli = Cli::try_parse_from([
            "cang",
            "--waypipe",
            "exec",
            "task-a",
            "--",
            "gui-application",
        ])
        .expect("valueless Waypipe exec should parse");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Exec { task_id, command, waypipe, .. }
                if task_id == "task-a"
                    && command == ["gui-application"]
                    && waypipe == Some(None)
        ));
    }

    #[test]
    fn valued_waypipe_exec_replaces_running_server() {
        let cli = Cli::try_parse_from([
            "cang",
            "--waypipe=/tmp/waypipe-client.sock",
            "exec",
            "task-a",
            "--",
            "gui-application",
        ])
        .expect("valued Waypipe exec should parse");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Exec { task_id, command, waypipe, .. }
                if task_id == "task-a"
                    && command == ["gui-application"]
                    && waypipe == Some(Some(PathBuf::from("/tmp/waypipe-client.sock")))
        ));
    }
    #[test]
    fn parses_daemon_runtime_option() {
        let cli =
            Cli::try_parse_from(["cang", "--daemon"]).expect("daemon runtime option should parse");
        let options = cli.into_runtime_options();

        assert!(options.daemon);
    }

    #[test]
    fn parses_pty_runtime_modes() {
        for (arg, expected) in [
            (
                "normalize",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: false,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "raw",
                PtyOptions {
                    mode: PtyMode::RawPassthrough,
                    trace: false,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "normalize,trace",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: true,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "trace",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: true,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "raw,trace",
                PtyOptions {
                    mode: PtyMode::RawPassthrough,
                    trace: true,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "normalize,no-focus-input",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: false,
                    suppress_focus_input: true,
                    focus_report_guard: true,
                },
            ),
            (
                "no-focus-input",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: false,
                    suppress_focus_input: true,
                    focus_report_guard: true,
                },
            ),
            (
                "trace,no-focus-input",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: true,
                    suppress_focus_input: true,
                    focus_report_guard: true,
                },
            ),
            (
                "raw,no-focus-input,trace",
                PtyOptions {
                    mode: PtyMode::RawPassthrough,
                    trace: true,
                    suppress_focus_input: true,
                    focus_report_guard: true,
                },
            ),
            (
                "focus-report-guard",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: false,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "trace,focus-report-guard",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: true,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "raw,focus-report-guard,trace",
                PtyOptions {
                    mode: PtyMode::RawPassthrough,
                    trace: true,
                    suppress_focus_input: false,
                    focus_report_guard: true,
                },
            ),
            (
                "normalize,no-focus-input,focus-report-guard",
                PtyOptions {
                    mode: PtyMode::Normalized,
                    trace: false,
                    suppress_focus_input: true,
                    focus_report_guard: true,
                },
            ),
        ] {
            let options = Cli::try_parse_from(["cang", "--pty", arg])
                .expect("pty mode should parse")
                .into_runtime_options();

            assert_eq!(options.pty, expected);
        }
    }

    #[test]
    fn rejects_malformed_pty_runtime_modes() {
        for arg in [
            "",
            "raw,normalize",
            "normalize,raw",
            "raw,raw",
            "trace,trace",
            "raw,trace,trace",
            "normalize,no-focus-input,no-focus-input",
            "no-focus-input,no-focus-input",
            "focus-report-guard,focus-report-guard",
            "focus-startup-guard",
            "raw,",
            ",raw",
            "passthrough",
        ] {
            let err =
                Cli::try_parse_from(["cang", "--pty", arg]).expect_err("bad pty mode should fail");
            assert!(matches!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation | clap::error::ErrorKind::InvalidValue
            ));
        }
    }

    #[test]
    fn rejects_removed_pty_raw_passthrough_flag() {
        let err = Cli::try_parse_from(["cang", "--pty-raw-passthrough"])
            .expect_err("removed raw passthrough flag should fail");

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn pty_option_is_inert_for_attach_subcommand() {
        let cli = Cli::try_parse_from(["cang", "--pty", "raw,trace", "attach", "workspace-123"])
            .expect("pty option stays parse-compatible for attach");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Attach { task_id, .. } if task_id == "workspace-123"
        ));
    }

    #[test]
    fn daemon_is_inert_for_management_subcommands() {
        let cli = Cli::try_parse_from(["cang", "--daemon", "ps"])
            .expect("daemon stays parse-compatible for management commands");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Ps { .. }
        ));
    }

    #[test]
    fn parses_landlock_runtime_modes() {
        for (value, expected) in [
            ("all", LandlockMode::All),
            ("relax", LandlockMode::Relax),
            ("best-effort", LandlockMode::BestEffort),
            ("off", LandlockMode::Off),
        ] {
            let actual = Cli::try_parse_from(["cang", "--landlock", value])
                .expect("landlock mode should parse")
                .into_runtime_options()
                .landlock;
            assert_eq!(actual, Some(expected));
        }
    }

    #[test]
    fn rejects_malformed_landlock_runtime_modes() {
        for value in ["", "audit", "default", "best_effort", "enforce"] {
            let err = Cli::try_parse_from(["cang", "--landlock", value])
                .expect_err("bad landlock mode should fail");
            assert!(matches!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation | clap::error::ErrorKind::InvalidValue
            ));
        }
    }

    #[test]
    fn parses_seccomp_runtime_modes() {
        let audit = Cli::try_parse_from(["cang", "--seccomp", "audit:/tmp/trace.jsonl"])
            .expect("audit seccomp should parse")
            .into_runtime_options()
            .seccomp;
        assert_eq!(
            audit,
            Some(SeccompMode::Audit(AuditMode::Full {
                trace_path: "/tmp/trace.jsonl".into(),
            }))
        );

        let trace_alias = Cli::try_parse_from(["cang", "--seccomp", "trace:/tmp/trace.jsonl"])
            .expect("trace alias should parse")
            .into_runtime_options()
            .seccomp;
        assert_eq!(
            trace_alias,
            Some(SeccompMode::Audit(AuditMode::Full {
                trace_path: "/tmp/trace.jsonl".into(),
            }))
        );

        let gap_audit = Cli::try_parse_from([
            "cang",
            "--seccomp",
            "audit:/tmp/baseline.json:/tmp/denied.jsonl",
        ])
        .expect("gap audit seccomp should parse")
        .into_runtime_options()
        .seccomp;
        assert_eq!(
            gap_audit,
            Some(SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path: "/tmp/baseline.json".into(),
                trace_path: "/tmp/denied.jsonl".into(),
            }))
        );

        let trace_gap_alias = Cli::try_parse_from([
            "cang",
            "--seccomp",
            "trace:/tmp/baseline.json:/tmp/denied.jsonl",
        ])
        .expect("trace gap alias should parse")
        .into_runtime_options()
        .seccomp;
        assert_eq!(
            trace_gap_alias,
            Some(SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path: "/tmp/baseline.json".into(),
                trace_path: "/tmp/denied.jsonl".into(),
            }))
        );

        let default_gap_audit =
            Cli::try_parse_from(["cang", "--seccomp", "audit-default:/tmp/denied.jsonl"])
                .expect("default gap audit seccomp should parse")
                .into_runtime_options()
                .seccomp;
        assert_eq!(
            default_gap_audit,
            Some(SeccompMode::Audit(AuditMode::DefaultGap {
                trace_path: "/tmp/denied.jsonl".into(),
            }))
        );

        let default_trace_alias =
            Cli::try_parse_from(["cang", "--seccomp", "trace-default:/tmp/denied.jsonl"])
                .expect("default trace alias should parse")
                .into_runtime_options()
                .seccomp;
        assert_eq!(
            default_trace_alias,
            Some(SeccompMode::Audit(AuditMode::DefaultGap {
                trace_path: "/tmp/denied.jsonl".into(),
            }))
        );

        let enforce = Cli::try_parse_from(["cang", "--seccomp", "enforce:/tmp/policy.json"])
            .expect("enforce seccomp should parse")
            .into_runtime_options()
            .seccomp;
        assert_eq!(
            enforce,
            Some(SeccompMode::Enforce {
                policy_path: "/tmp/policy.json".into(),
            })
        );
    }

    #[test]
    fn seccomp_rejects_malformed_runtime_modes() {
        for args in [
            ["cang", "--seccomp", ""],
            ["cang", "--seccomp", "audit:"],
            ["cang", "--seccomp", "audit::trace.jsonl"],
            ["cang", "--seccomp", "audit:policy.json:"],
            ["cang", "--seccomp", "audit:policy.json:trace.jsonl:extra"],
            ["cang", "--seccomp", "audit-default:"],
            ["cang", "--seccomp", "audit-default:policy.json:trace.jsonl"],
            ["cang", "--seccomp", "trace-default:"],
            ["cang", "--seccomp", "enforce:"],
            ["cang", "--seccomp", "enforce:/tmp/policy.json:extra"],
            ["cang", "--seccomp", "default"],
            ["cang", "--seccomp", "log:/tmp/trace.jsonl"],
        ] {
            let err = Cli::try_parse_from(args).expect_err("bad seccomp mode should fail");
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn parses_seccomp_synthesize_subcommand() {
        let cli = Cli::try_parse_from([
            "cang",
            "seccomp",
            "synthesize",
            "--input",
            "trace.jsonl",
            "--output",
            "policy.json",
        ])
        .expect("seccomp synthesize command should parse");

        assert!(matches!(
            cli.into_action(),
            crate::cli::CliAction::Seccomp { .. }
        ));
    }

    #[test]
    fn parses_seccomp_extend_subcommand() {
        let cli = Cli::try_parse_from([
            "cang",
            "seccomp",
            "extend",
            "--policy",
            "baseline.json",
            "--trace",
            "denied.jsonl",
            "--output",
            "updated.json",
        ])
        .expect("seccomp extend command should parse");

        match cli.into_action() {
            crate::cli::CliAction::Seccomp {
                command:
                    SeccompCommand::Extend {
                        policy,
                        default_policy,
                        trace,
                        output,
                    },
                ..
            } => {
                assert_eq!(policy, Some(PathBuf::from("baseline.json")));
                assert!(!default_policy);
                assert_eq!(trace, PathBuf::from("denied.jsonl"));
                assert_eq!(output, PathBuf::from("updated.json"));
            }
            other => panic!("expected seccomp extend action, got {other:?}"),
        }
    }

    #[test]
    fn parses_seccomp_extend_default_policy_subcommand() {
        let cli = Cli::try_parse_from([
            "cang",
            "seccomp",
            "extend",
            "--default-policy",
            "--trace",
            "denied.jsonl",
            "--output",
            "updated.json",
        ])
        .expect("seccomp extend default policy command should parse");

        match cli.into_action() {
            crate::cli::CliAction::Seccomp {
                command:
                    SeccompCommand::Extend {
                        policy,
                        default_policy,
                        trace,
                        output,
                    },
                ..
            } => {
                assert_eq!(policy, None);
                assert!(default_policy);
                assert_eq!(trace, PathBuf::from("denied.jsonl"));
                assert_eq!(output, PathBuf::from("updated.json"));
            }
            other => panic!("expected seccomp extend action, got {other:?}"),
        }
    }

    #[test]
    fn seccomp_extend_requires_exactly_one_baseline_source() {
        let missing = Cli::try_parse_from([
            "cang",
            "seccomp",
            "extend",
            "--trace",
            "denied.jsonl",
            "--output",
            "updated.json",
        ])
        .expect_err("extend without a baseline source should fail");
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let conflict = Cli::try_parse_from([
            "cang",
            "seccomp",
            "extend",
            "--policy",
            "baseline.json",
            "--default-policy",
            "--trace",
            "denied.jsonl",
            "--output",
            "updated.json",
        ])
        .expect_err("extend with both baseline sources should fail");
        assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn tsi_flag_selects_tsi_network_mode() {
        let cli = Cli::try_parse_from(["cang", "--tsi"]).expect("tsi flag should parse");
        let options = cli.into_runtime_options();

        assert_eq!(options.network_mode, NetworkMode::Tsi);
    }

    #[test]
    fn pulse_server_accepts_ip_socket_addresses_and_canonicalizes_them() {
        let ipv4 = Cli::try_parse_from(["cang", "--pulse=tcp:192.0.2.10:4713"])
            .expect("IPv4 Pulse endpoint should parse")
            .into_runtime_options();
        let ipv6 = Cli::try_parse_from(["cang", "--pulse=tcp:[2001:db8::10]:4713"])
            .expect("IPv6 Pulse endpoint should parse")
            .into_runtime_options();

        assert_eq!(
            ipv4.pulse
                .expect("Pulse endpoint should be present")
                .direct_env_value()
                .as_deref(),
            Some("tcp:192.0.2.10:4713")
        );
        assert_eq!(
            ipv6.pulse
                .expect("Pulse endpoint should be present")
                .direct_env_value()
                .as_deref(),
            Some("tcp:[2001:db8::10]:4713")
        );
    }

    #[test]
    fn pulse_server_accepts_host_loopback_bridge_spellings() {
        for value in ["tcp:localhost:4714", "tcp:127.0.0.1:4714"] {
            let options = Cli::try_parse_from(["cang", &format!("--pulse={value}")])
                .expect("host-loopback Pulse endpoint should parse")
                .into_runtime_options();
            assert_eq!(
                options
                    .pulse
                    .expect("Pulse endpoint should be present")
                    .host_loopback_port(),
                Some(4714)
            );
        }
    }

    #[test]
    fn pulse_server_coexists_with_tsi() {
        let options = Cli::try_parse_from(["cang", "--tsi", "--pulse=tcp:127.0.0.1:4713"])
            .expect("Pulse endpoint should coexist with TSI")
            .into_runtime_options();

        assert_eq!(options.network_mode, NetworkMode::Tsi);
        assert_eq!(
            options
                .pulse
                .expect("Pulse endpoint should be present")
                .host_loopback_port(),
            Some(4713)
        );
    }

    #[test]
    fn pulse_server_rejects_invalid_endpoints() {
        for value in [
            "udp:192.0.2.10:4713",
            "tcp:pulse.example.test:4713",
            "tcp:192.0.2.10",
            "tcp:192.0.2.10:0",
            "tcp:2001:db8::10:4713",
            "/run/user/1000/pulse/native",
        ] {
            let error = Cli::try_parse_from(["cang", &format!("--pulse={value}")])
                .expect_err("invalid Pulse endpoint should fail");

            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn passt_flag_is_removed() {
        let err =
            Cli::try_parse_from(["cang", "--passt"]).expect_err("removed passt flag should fail");

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn new_perms_flag_parses_supported_values() {
        let cli = Cli::try_parse_from([
            "cang",
            "--new-perms=perf,bpf,sys-admin,io-uring,net-admin,net-raw,bpf,sys-admin",
        ])
        .expect("new permissions should parse");
        let options = cli.into_runtime_options();

        assert_eq!(
            options.new_perms.to_string(),
            "io-uring,net-admin,net-raw,bpf,perf,sys-admin"
        );
    }

    #[test]
    fn new_perms_flag_rejects_unknown_and_empty_values() {
        for value in ["", "io-uring,,perf", "mount-admin"] {
            let error = Cli::try_parse_from(["cang", &format!("--new-perms={value}")])
                .expect_err("invalid new permissions should fail");

            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn permissions_flag_is_rejected() {
        let error = Cli::try_parse_from(["cang", "--permissions=net-raw"])
            .expect_err("removed permissions flag should fail");

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn removed_guest_permission_flags_fail() {
        for flag in ["--io-uring", "--perf"] {
            let error = Cli::try_parse_from(["cang", flag])
                .expect_err("removed permission flag should fail");

            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn workspace_and_waypipe_flags_parse_independently() {
        let cli = Cli::try_parse_from([
            "cang",
            "--workspace=/home/dev/foo",
            "--waypipe=/tmp/cang-waypipe.sock",
            "--",
            "gui-application",
        ])
        .expect("workspace and waypipe flags should parse");
        let options = cli.into_runtime_options();

        assert_eq!(options.workspace, Some(PathBuf::from("/home/dev/foo")));
        assert_eq!(
            options.waypipe,
            Some(Some(PathBuf::from("/tmp/cang-waypipe.sock")))
        );
        assert_eq!(options.guest_command, ["gui-application"]);
    }

    #[test]
    fn workspace_flag_is_independent_of_waypipe() {
        let cli = Cli::try_parse_from(["cang", "--workspace=/home/dev/foo"])
            .expect("workspace flag should parse without waypipe");
        let options = cli.into_runtime_options();

        assert_eq!(options.workspace, Some(PathBuf::from("/home/dev/foo")));
        assert_eq!(options.waypipe, None);
    }

    #[test]
    fn workspace_and_waypipe_flags_require_absolute_nonempty_paths() {
        for args in [
            vec!["cang", "--workspace", ""],
            vec!["cang", "--workspace", "relative"],
            vec!["cang", "--waypipe="],
            vec!["cang", "--waypipe=relative.sock"],
        ] {
            let err = Cli::try_parse_from(args).expect_err("invalid path should fail");

            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }
}

#[test]
fn waypipe_flag_conflicts_with_local_wayland_but_accepts_drm() {
    let err = Cli::try_parse_from(["cang", "--waypipe=/tmp/waypipe.sock", "--wayland"])
        .expect_err("waypipe and local Wayland should conflict");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

    let cli = Cli::try_parse_from(["cang", "--waypipe=/tmp/waypipe.sock", "--gpu=drm"])
        .expect("waypipe and DRM should parse");
    let options = cli.into_runtime_options();

    assert_eq!(options.gpu_mode, GpuMode::Drm);
    assert_eq!(
        options.waypipe,
        Some(Some(PathBuf::from("/tmp/waypipe.sock")))
    );
}

#[test]
fn publish_flag_is_repeatable() {
    let cli = Cli::try_parse_from(["cang", "-p", "8080:80", "--publish", "8443:443"])
        .expect("publish flags should parse");
    let options = cli.into_runtime_options();

    assert_eq!(options.publish, ["8080:80", "8443:443"]);
}

#[test]
fn volume_flag_is_repeatable_and_parses_access_modes() {
    let cli = Cli::try_parse_from([
        "cang",
        "-v",
        "/host/dir:/guest/dir",
        "--volume",
        "/host/file:/guest/file:ro",
        "-v",
        "/host/cache:/home/dev/cache:rw",
    ])
    .expect("volume flags should parse");
    let options = cli.into_runtime_options();

    assert_eq!(
        options.volumes,
        [
            VolumeSpec {
                source: "/host/dir".into(),
                target: "/guest/dir".to_owned(),
                read_only: false,
            },
            VolumeSpec {
                source: "/host/file".into(),
                target: "/guest/file".to_owned(),
                read_only: true,
            },
            VolumeSpec {
                source: "/host/cache".into(),
                target: "/home/dev/cache".to_owned(),
                read_only: false,
            },
        ]
    );
}

#[test]
fn volume_rejects_empty_or_unsupported_specs() {
    for args in [
        ["cang", "-v", ""],
        ["cang", "-v", "/host-only"],
        ["cang", "-v", ":/guest"],
        ["cang", "-v", "/host:"],
        ["cang", "-v", "/host:/guest:z"],
        ["cang", "-v", "/host:/guest:"],
        ["cang", "-v", "/host:/guest:ro:rshared"],
    ] {
        let err = Cli::try_parse_from(args).expect_err("volume spec should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }
}

#[test]
fn publish_rejects_empty_spec() {
    let empty_err = Cli::try_parse_from(["cang", "-p", ""]).expect_err("empty publish should fail");
    let whitespace_err =
        Cli::try_parse_from(["cang", "--publish", "   "]).expect_err("blank publish should fail");

    assert_eq!(empty_err.kind(), clap::error::ErrorKind::ValueValidation);
    assert_eq!(
        whitespace_err.kind(),
        clap::error::ErrorKind::ValueValidation
    );
}

#[test]
fn parses_allocator_modes() {
    for (value, expected) in [
        ("mimalloc", AllocatorMode::Mimalloc),
        ("hardened", AllocatorMode::Hardened),
        ("glibc", AllocatorMode::Glibc),
    ] {
        let cli =
            Cli::try_parse_from(["cang", "--alloc", value]).expect("allocator mode should parse");

        assert_eq!(cli.into_runtime_options().allocator, expected);
    }
}

#[test]
fn allocator_defaults_to_mimalloc() {
    let options = Cli::try_parse_from(["cang"])
        .expect("default CLI should parse")
        .into_runtime_options();

    assert_eq!(options.allocator, AllocatorMode::Mimalloc);
}

#[test]
fn rejects_unknown_allocator_mode() {
    let err = Cli::try_parse_from(["cang", "--alloc", "jemalloc"])
        .expect_err("unknown allocator should fail");

    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
}

#[test]
fn allocator_is_inert_for_management_subcommands() {
    let cli = Cli::try_parse_from(["cang", "--alloc", "glibc", "images", "list"])
        .expect("allocator stays parse-compatible for management commands");

    match cli.into_action() {
        crate::cli::CliAction::Images { command, .. } => {
            assert_eq!(command, crate::cli::ImagesCommand::List)
        }
        other => panic!("expected images action, got {other:?}"),
    }
}

#[test]
fn parses_explicit_guest_command_after_delimiter() {
    let cli = Cli::try_parse_from(["cang", "--", "bash", "-lc", "echo ok"])
        .expect("guest command should parse after delimiter");
    let options = cli.into_runtime_options();

    assert_eq!(options.guest_command, ["bash", "-lc", "echo ok"]);
}

#[test]
fn publish_preserves_guest_command() {
    let cli = Cli::try_parse_from(["cang", "-p", "8080:80", "--", "bash", "-lc", "echo ok"])
        .expect("publish and command should parse");
    let options = cli.into_runtime_options();

    assert_eq!(options.publish, ["8080:80"]);
    assert!(options.volumes.is_empty());
    assert_eq!(options.guest_command, ["bash", "-lc", "echo ok"]);
}

#[test]
fn parses_explicit_guest_command_after_options_and_delimiter() {
    let cli = Cli::try_parse_from([
        "cang",
        "--guest-init",
        "/tmp/cang-guest-init",
        "--log-level",
        "debug",
        "--profile",
        "--",
        "sh",
        "/workspace/probe.sh",
    ])
    .expect("guest command should parse after options and delimiter");
    let options = cli.into_runtime_options();

    assert_eq!(
        options.guest_init.as_deref(),
        Some("/tmp/cang-guest-init".as_ref())
    );
    assert_eq!(options.log_settings.level, LogLevel::Debug);
    assert!(options.profile);
    assert_eq!(options.guest_command, ["sh", "/workspace/probe.sh"]);
}

#[test]
fn parses_decode_launch_conf_subcommand() {
    let cli = Cli::try_parse_from(["cang", "decode-launch-conf", "/tmp/launch.conf"])
        .expect("decode subcommand should parse");

    assert_eq!(
        cli.into_action(),
        crate::cli::CliAction::DecodeLaunchConf {
            path: "/tmp/launch.conf".into(),
        }
    );
}

#[test]
fn decode_launch_conf_is_not_confused_with_guest_command() {
    let cli = Cli::try_parse_from(["cang", "--", "decode-launch-conf", "/tmp/launch.conf"])
        .expect("delimited words should remain a guest command");
    let options = cli.into_runtime_options();

    assert_eq!(
        options.guest_command,
        ["decode-launch-conf", "/tmp/launch.conf"]
    );
}

#[test]
fn bare_words_are_not_guest_commands() {
    let err = Cli::try_parse_from(["cang", "microvm"])
        .expect_err("guest commands must use an explicit delimiter");

    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn image_env_uses_cang_prefix() {
    let cli =
        Cli::try_parse_from(["cang", "--image", "example/cang:dev"]).expect("image should parse");
    let options = cli.into_runtime_options();

    assert_eq!(options.image.as_deref(), Some("example/cang:dev"));
}

#[test]
fn pull_latest_records_canonical_refresh_intent() {
    let cli = Cli::try_parse_from(["cang", "--pull-latest"]).expect("pull flag should parse");
    let options = cli.into_runtime_options();

    assert!(options.pull_latest);
    assert_eq!(options.image, None);
}

#[test]
fn image_and_pull_latest_are_mutually_exclusive() {
    let err = Cli::try_parse_from(["cang", "--image", "example/cang:dev", "--pull-latest"])
        .expect_err("explicit image and canonical refresh should conflict");

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn rootfs_backend_rejects_removed_values() {
    let auto_err = Cli::try_parse_from(["cang", "--rootfs-backend", "auto"])
        .expect_err("auto backend should fail");
    let reflink_err = Cli::try_parse_from(["cang", "--rootfs-backend", "reflink"])
        .expect_err("reflink backend should fail");

    assert_eq!(auto_err.kind(), clap::error::ErrorKind::ValueValidation);
    assert_eq!(reflink_err.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn container_store_backend_accepts_raw_disk_only() {
    let raw = Cli::try_parse_from(["cang", "--container-store", "raw-disk"])
        .expect("raw disk container store should parse")
        .into_runtime_options();

    assert_eq!(
        raw.container_store_backend,
        Some(ContainerStoreBackend::RawDisk)
    );
}

#[test]
fn container_store_backend_rejects_invalid_values() {
    let bind_err = Cli::try_parse_from(["cang", "--container-store", "bind"])
        .expect_err("bind container store should fail");
    let err = Cli::try_parse_from(["cang", "--container-store", "auto"])
        .expect_err("unknown container store should fail");

    assert_eq!(bind_err.kind(), clap::error::ErrorKind::ValueValidation);
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn legacy_storage_flag_is_not_accepted() {
    let err = Cli::try_parse_from(["cang", "--storage", "auto"])
        .expect_err("storage flag should not exist");

    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn parses_explicit_log_level() {
    let cli =
        Cli::try_parse_from(["cang", "--log-level", "trace"]).expect("log level should parse");
    let options = cli.into_runtime_options();

    assert_eq!(options.log_settings.level, LogLevel::Trace);
}

#[test]
fn rejects_invalid_log_level() {
    let err = Cli::try_parse_from(["cang", "--log-level", "verbose"])
        .expect_err("unknown log level should fail");

    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
}

#[test]
fn explicit_log_level_overrides_debug_compatibility() {
    let cli = Cli::try_parse_from(["cang", "--debug", "--log-level", "info"])
        .expect("log level should parse");
    let options = cli.into_runtime_options();

    assert!(options.debug);
    assert_eq!(options.log_settings.level, LogLevel::Info);
}

#[test]
fn memory_must_be_positive_gib() {
    let err = Cli::try_parse_from(["cang", "--mem", "0"]).expect_err("zero memory should fail");

    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
}
#[test]
fn parses_images_sync_subcommand() {
    let cli = Cli::try_parse_from(["cang", "images", "sync", "ghcr.io/example/cang:dev"])
        .expect("images sync should parse");

    match cli.into_action() {
        crate::cli::CliAction::Images { command, .. } => assert_eq!(
            command,
            crate::cli::ImagesCommand::Sync {
                reference: "ghcr.io/example/cang:dev".to_owned()
            }
        ),
        other => panic!("expected images action, got {other:?}"),
    }
}

#[test]
fn parses_images_list_and_remove_subcommands() {
    let list = Cli::try_parse_from(["cang", "images", "list"]).expect("images list should parse");
    let remove = Cli::try_parse_from(["cang", "images", "remove", "sha256-feedface"])
        .expect("images remove should parse");
    let dry_run = Cli::try_parse_from(["cang", "images", "remove", "--dry-run", "sha256-feedface"])
        .expect("images remove dry-run should parse");

    match list.into_action() {
        crate::cli::CliAction::Images { command, .. } => {
            assert_eq!(command, crate::cli::ImagesCommand::List);
        }
        other => panic!("expected images list action, got {other:?}"),
    }
    match remove.into_action() {
        crate::cli::CliAction::Images { command, .. } => assert_eq!(
            command,
            crate::cli::ImagesCommand::Remove {
                dry_run: false,
                target: "sha256-feedface".to_owned()
            }
        ),
        other => panic!("expected images remove action, got {other:?}"),
    }
    match dry_run.into_action() {
        crate::cli::CliAction::Images { command, .. } => assert_eq!(
            command,
            crate::cli::ImagesCommand::Remove {
                dry_run: true,
                target: "sha256-feedface".to_owned()
            }
        ),
        other => panic!("expected images remove dry-run action, got {other:?}"),
    }
}

#[test]
fn images_remove_help_mentions_dry_run() {
    let err =
        Cli::try_parse_from(["cang", "images", "remove", "--help"]).expect_err("help should exit");
    let rendered = err.to_string();

    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    assert!(rendered.contains("--dry-run"));
    assert!(rendered.contains("fails when local Buildah removal would be skipped"));
}

#[test]
fn parses_task_control_ps_and_kill_subcommands() {
    let ps = Cli::try_parse_from(["cang", "ps"]).expect("ps should parse");
    let kill = Cli::try_parse_from(["cang", "kill", "workspace-1-42"]).expect("kill should parse");

    match ps.into_action() {
        crate::cli::CliAction::Ps { .. } => {}
        other => panic!("expected ps action, got {other:?}"),
    }
    match kill.into_action() {
        crate::cli::CliAction::Kill { task_id, .. } => {
            assert_eq!(task_id, "workspace-1-42");
        }
        other => panic!("expected kill action, got {other:?}"),
    }
}

#[test]
fn parses_container_store_resize_and_reset_subcommands() {
    let resize = Cli::try_parse_from(["cang", "container-store", "resize", "--size", "128G"])
        .expect("container-store resize should parse");
    let reset = Cli::try_parse_from(["cang", "container-store", "reset", "--force"])
        .expect("container-store reset should parse");

    match resize.into_action() {
        crate::cli::CliAction::ContainerStore { command, .. } => assert_eq!(
            command,
            crate::cli::ContainerStoreCommand::Resize {
                size: "128G".to_owned()
            }
        ),
        other => panic!("expected container-store resize action, got {other:?}"),
    }
    match reset.into_action() {
        crate::cli::CliAction::ContainerStore { command, .. } => assert_eq!(
            command,
            crate::cli::ContainerStoreCommand::Reset { force: true }
        ),
        other => panic!("expected container-store reset action, got {other:?}"),
    }
}

#[test]
fn container_store_reset_accepts_missing_force_for_runtime_error() {
    let reset = Cli::try_parse_from(["cang", "container-store", "reset"])
        .expect("runtime should produce force-specific error");

    match reset.into_action() {
        crate::cli::CliAction::ContainerStore { command, .. } => assert_eq!(
            command,
            crate::cli::ContainerStoreCommand::Reset { force: false }
        ),
        other => panic!("expected container-store reset action, got {other:?}"),
    }
}

#[test]
fn images_words_after_delimiter_remain_guest_command() {
    let cli = Cli::try_parse_from(["cang", "--", "images", "list"])
        .expect("delimited images words should parse as guest command");
    let options = cli.into_runtime_options();

    assert_eq!(options.guest_command, ["images", "list"]);
}
