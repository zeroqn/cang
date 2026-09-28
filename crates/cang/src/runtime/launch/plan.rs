use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use crate::cli::{ContainerStoreBackend, RuntimeOptions, VolumeSpec};
use crate::config;
use crate::logging::LogLevel;
use crate::naming::derive_workspace_slug;
use crate::runtime::host_tools;
use crate::runtime::landlock::LandlockMode;
use crate::runtime::launch::components::mounts;
use crate::runtime::launch::config::{
    AllocatorMode, BindMount, BindMountSourceKind, GuestPermissions, NIX_TARGET, NetworkMode,
    PulseServer, canonical_mount_target,
};
use crate::runtime::seccomp::{self, AuditMode, SeccompMode};
use crate::runtime::vm::gpu::GpuMode;
use crate::state::{self, StateLayout};
use crate::task_rootfs::TaskRootfsBackend;
use crate::{DEFAULT_FALLBACK_IMAGE, DEFAULT_IMAGE};

/// Resolved host-side launch intent and session inputs.
///
/// `LaunchPlan` is built from CLI/config/environment before any task rootfs or
/// helper/libkrun execution contract is materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchPlan {
    pub(crate) workspace_dir: PathBuf,
    pub(crate) workspace_slug: String,
    pub(crate) hostname: String,
    pub(crate) state_layout: StateLayout,
    pub(crate) image_cache_dir: PathBuf,
    pub(crate) sccache_dir: PathBuf,
    pub bind_mounts: Vec<BindMount>,
    pub(crate) image_selection: ImageSelection,
    pub(crate) task_rootfs_backend: TaskRootfsBackend,
    pub(crate) container_store_backend: ContainerStoreBackend,
    pub(crate) guest_init: Option<PathBuf>,
    pub(crate) mem_gib: Option<u32>,
    pub(crate) network_mode: NetworkMode,
    pub(crate) pulse: Option<PulseServer>,
    pub(crate) gpu_mode: GpuMode,
    pub(crate) zero_copy_shm: bool,
    pub(crate) wayland: bool,
    pub(crate) waypipe: bool,
    pub(crate) waypipe_socket: Option<PathBuf>,
    pub(crate) new_perms: GuestPermissions,
    pub(crate) publish: Vec<String>,
    pub(crate) guest_command: Vec<String>,
    pub(crate) debug: bool,
    pub(crate) log_level: LogLevel,
    pub(crate) profile: bool,
    pub(crate) root: bool,
    pub(crate) daemon: bool,
    pub(crate) seccomp: SeccompMode,
    pub(crate) landlock: LandlockMode,
    pub(crate) allocator: AllocatorMode,
    pub(crate) preserve_debug: bool,
    pub(crate) config_diagnostics: ConfigDiagnostics,
}

impl LaunchPlan {
    pub(crate) fn from_env(options: RuntimeOptions, workspace_dir: PathBuf) -> Result<Self> {
        let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let home_dir = env::var_os("HOME").map(PathBuf::from);

        Self::from_env_values(
            options,
            workspace_dir,
            xdg_state_home.as_deref(),
            xdg_config_home.as_deref(),
            home_dir.as_deref(),
        )
    }

    fn from_env_values(
        options: RuntimeOptions,
        workspace_dir: PathBuf,
        xdg_state_home: Option<&Path>,
        xdg_config_home: Option<&Path>,
        home_dir: Option<&Path>,
    ) -> Result<Self> {
        let waypipe = options.waypipe.is_some();
        let waypipe_socket = resolve_waypipe(&options)?;
        let config = config::state::read_config(xdg_config_home, home_dir)?;
        let state_layout = state::resolve_state_layout_from_parts(
            &workspace_dir,
            xdg_state_home,
            home_dir,
            config.state_location_override(),
        )?;
        let workspace_slug = derive_workspace_slug(&workspace_dir);
        let hostname = derive_runtime_hostname(&workspace_slug);
        let image_cache_dir = state_layout.image_cache_dir();
        let sccache_dir = state_layout.sccache_dir();
        let home_dir = home_dir.ok_or_else(|| {
            anyhow::anyhow!("HOME is not set; cang cannot prepare built-in home/tool bind mounts")
        })?;
        let container_store_backend = options
            .container_store_backend
            .unwrap_or(ContainerStoreBackend::DEFAULT);
        let mut bind_mounts = mounts::prepare_dev_mounts(&workspace_dir, home_dir, &state_layout)?;
        bind_mounts.extend(prepare_user_volume_mounts(
            &options.volumes,
            &workspace_dir,
            bind_mounts.len(),
        )?);
        crate::runtime::launch::config::validate_mounts(&bind_mounts)?;
        let task_rootfs_backend = options
            .rootfs_backend
            .or_else(|| config.task_rootfs_backend())
            .unwrap_or(TaskRootfsBackend::DEFAULT);
        let seccomp = resolve_normal_launch_seccomp(options.seccomp)?;
        let landlock = resolve_normal_launch_landlock(options.landlock);
        if options.zero_copy_shm && options.gpu_mode != crate::runtime::vm::gpu::GpuMode::Drm {
            anyhow::bail!(
                "--zero-copy-shm requires --gpu=drm: the udmabuf fast path is a virtio-gpu property"
            );
        }

        Ok(Self {
            workspace_dir,
            workspace_slug,
            hostname,
            state_layout,
            image_cache_dir,
            sccache_dir,
            bind_mounts,
            image_selection: ImageSelection::from_runtime_options(
                options.image,
                options.pull_latest,
            ),
            task_rootfs_backend,
            container_store_backend,
            guest_init: options.guest_init,
            mem_gib: options.mem_gib,
            network_mode: options.network_mode,
            pulse: options.pulse,
            gpu_mode: options.gpu_mode,
            zero_copy_shm: options.zero_copy_shm,
            wayland: options.wayland,
            waypipe,
            waypipe_socket,
            new_perms: options.new_perms,
            publish: options.publish,
            guest_command: options.guest_command,
            debug: options.log_settings.level.enables_debug(),
            log_level: options.log_settings.level,
            profile: options.profile,
            root: options.root,
            daemon: options.daemon,
            seccomp,
            landlock,
            allocator: options.allocator,
            preserve_debug: options.preserve_debug,
            config_diagnostics: ConfigDiagnostics {
                config_path: config.path().to_path_buf(),
                config_loaded: config.loaded(),
            },
        })
    }
}
fn resolve_waypipe(options: &RuntimeOptions) -> Result<Option<PathBuf>> {
    let Some(Some(socket)) = &options.waypipe else {
        return Ok(None);
    };
    if !socket.is_absolute() {
        anyhow::bail!(
            "waypipe socket must be an absolute path: {}",
            socket.display()
        );
    }

    let socket_metadata = fs::metadata(socket)
        .with_context(|| format!("waypipe socket does not exist: {}", socket.display()))?;
    if !socket_metadata.file_type().is_socket() {
        anyhow::bail!(
            "waypipe transport is not a Unix socket: {}",
            socket.display()
        );
    }

    Ok(Some(socket.clone()))
}

fn resolve_normal_launch_seccomp(seccomp: Option<SeccompMode>) -> Result<SeccompMode> {
    resolve_normal_launch_seccomp_with(seccomp, host_tools::default_seccomp_policy_path)
}

fn resolve_normal_launch_seccomp_with(
    seccomp: Option<SeccompMode>,
    default_policy_path: impl FnOnce() -> Option<PathBuf>,
) -> Result<SeccompMode> {
    match seccomp {
        Some(SeccompMode::Audit(AuditMode::DefaultGap { trace_path })) => {
            let baseline_policy_path = seccomp::resolve_default_seccomp_policy_path(
                default_policy_path,
                "default seccomp gap audit",
                "pass --seccomp=audit:POLICY_JSON:TRACE_JSONL explicitly",
            )?;
            Ok(SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path,
                trace_path,
            }))
        }
        Some(seccomp) => Ok(seccomp),
        None => {
            let policy_path = seccomp::resolve_default_seccomp_policy_path(
                default_policy_path,
                "normal launch seccomp enforcement",
                "pass --seccomp=off to disable host-side seccomp for this run",
            )?;
            Ok(SeccompMode::Enforce { policy_path })
        }
    }
}

fn resolve_normal_launch_landlock(landlock: Option<LandlockMode>) -> LandlockMode {
    landlock.unwrap_or(LandlockMode::Relax)
}

fn prepare_user_volume_mounts(
    volumes: &[VolumeSpec],
    workspace_dir: &Path,
    tag_start: usize,
) -> Result<Vec<BindMount>> {
    volumes
        .iter()
        .enumerate()
        .map(|(index, volume)| prepare_user_volume_mount(volume, workspace_dir, tag_start + index))
        .collect()
}

fn prepare_user_volume_mount(
    volume: &VolumeSpec,
    workspace_dir: &Path,
    tag_index: usize,
) -> Result<BindMount> {
    let target = validate_user_volume_target(&volume.target)?;
    let source = absolute_source_path(&volume.source, workspace_dir);
    let source = fs::canonicalize(&source)
        .with_context(|| format!("failed to inspect volume source '{}'", source.display()))?;
    let metadata = fs::metadata(&source)
        .with_context(|| format!("failed to inspect volume source '{}'", source.display()))?;
    let source_kind = if metadata.is_dir() {
        BindMountSourceKind::Directory
    } else if metadata.is_file() {
        BindMountSourceKind::File
    } else {
        anyhow::bail!(
            "cang volume source '{}' must be a file or directory",
            source.display()
        );
    };
    let tag = format!("cang-user-volume-{tag_index}");
    match source_kind {
        BindMountSourceKind::Directory => Ok(BindMount {
            source,
            tag,
            target,
            source_kind,
            read_only: volume.read_only,
        }),
        BindMountSourceKind::File => Ok(BindMount::file(source, tag, target, volume.read_only)),
    }
}

fn absolute_source_path(source: &Path, workspace_dir: &Path) -> PathBuf {
    if source.is_absolute() {
        source.to_path_buf()
    } else {
        workspace_dir.join(source)
    }
}

fn validate_user_volume_target(target: &str) -> Result<String> {
    let target = canonical_mount_target(target)?;
    if target == NIX_TARGET {
        anyhow::bail!("cang volume target {NIX_TARGET} is reserved");
    }
    if target.contains(".config/codex") {
        anyhow::bail!("cang volume target must not include .config/codex");
    }
    Ok(target)
}

fn derive_runtime_hostname(workspace_slug: &str) -> String {
    format!("cang-{workspace_slug}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigDiagnostics {
    pub(crate) config_path: PathBuf,
    pub(crate) config_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageSelection {
    PreferLocalhostThenCanonical,
    CanonicalWithRefresh,
    Explicit { reference: String },
}

impl ImageSelection {
    fn from_runtime_options(explicit_image: Option<String>, pull_latest: bool) -> Self {
        match explicit_image {
            Some(reference) => Self::Explicit { reference },
            None if pull_latest => Self::CanonicalWithRefresh,
            None => Self::PreferLocalhostThenCanonical,
        }
    }

    pub(crate) fn selected_reference(&self) -> &str {
        match self {
            Self::PreferLocalhostThenCanonical => DEFAULT_IMAGE,
            Self::CanonicalWithRefresh => DEFAULT_FALLBACK_IMAGE,
            Self::Explicit { reference } => reference,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use crate::cli::{ContainerStoreBackend, RuntimeOptions, VolumeSpec};
    use crate::logging::{LogLevel, LogSettings};
    use crate::runtime::landlock::LandlockMode;
    use crate::runtime::launch::config::{AllocatorMode, BindMountSourceKind, NetworkMode};
    use crate::runtime::launch::plan::{
        ImageSelection, LaunchPlan, resolve_normal_launch_landlock,
        resolve_normal_launch_seccomp_with,
    };
    use crate::runtime::seccomp::{AuditMode, SeccompMode};
    use crate::task_rootfs::TaskRootfsBackend;
    use crate::{DEFAULT_FALLBACK_IMAGE, DEFAULT_IMAGE};

    fn runtime_options() -> RuntimeOptions {
        RuntimeOptions {
            image: None,
            pull_latest: false,
            debug: false,
            log_settings: LogSettings::resolve(None, false, None),
            profile: false,
            root: false,
            daemon: false,
            pty: crate::cli::PtyOptions::DEFAULT,
            seccomp: Some(SeccompMode::Off),
            landlock: None,
            new_perms: crate::runtime::launch::config::GuestPermissions::default(),
            allocator: AllocatorMode::Mimalloc,
            rootfs_backend: None,
            container_store_backend: None,
            guest_init: None,
            preserve_debug: false,
            mem_gib: None,
            network_mode: NetworkMode::Tsi,
            pulse: None,
            gpu_mode: crate::runtime::vm::gpu::GpuMode::Off,
            zero_copy_shm: false,
            wayland: false,
            workspace: None,
            waypipe: None,
            publish: Vec::new(),
            volumes: Vec::new(),
            guest_command: Vec::new(),
        }
    }

    #[test]
    fn default_plan_prefers_local_image_and_btrfs_backend() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = PathBuf::from("/tmp/example-project");
        let plan = LaunchPlan::from_env_values(
            runtime_options(),
            workspace.clone(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.workspace_dir, workspace);
        assert_eq!(plan.workspace_slug, "example-project");
        assert_eq!(plan.hostname, "cang-example-project");
        assert_eq!(
            plan.image_selection,
            ImageSelection::PreferLocalhostThenCanonical
        );
        assert_eq!(plan.image_selection.selected_reference(), DEFAULT_IMAGE);
        assert_eq!(plan.task_rootfs_backend, TaskRootfsBackend::BtrfsSnapshot);
        assert_eq!(plan.container_store_backend, ContainerStoreBackend::RawDisk);
        assert_eq!(plan.log_level, LogLevel::Off);
        assert_eq!(plan.landlock, LandlockMode::Relax);
        assert!(!plan.debug);
        assert!(!plan.daemon);
        assert!(!plan.config_diagnostics.config_loaded);
    }

    #[test]
    fn normal_launch_landlock_defaults_to_relax_and_carries_explicit_modes() {
        assert_eq!(resolve_normal_launch_landlock(None), LandlockMode::Relax);
        assert_eq!(
            resolve_normal_launch_landlock(Some(LandlockMode::All)),
            LandlockMode::All
        );
        assert_eq!(
            resolve_normal_launch_landlock(Some(LandlockMode::Relax)),
            LandlockMode::Relax
        );
        assert_eq!(
            resolve_normal_launch_landlock(Some(LandlockMode::BestEffort)),
            LandlockMode::BestEffort
        );
        assert_eq!(
            resolve_normal_launch_landlock(Some(LandlockMode::Off)),
            LandlockMode::Off
        );
    }

    #[test]
    fn omitted_normal_launch_seccomp_resolves_to_valid_packaged_default_policy() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let policy = dir.path().join("default.json");
        fs::write(
            &policy,
            include_bytes!("../../../assets/seccomp/default.json"),
        )
        .expect("default policy fixture should be written");
        let seccomp = resolve_normal_launch_seccomp_with(None, || Some(policy.clone()))
            .expect("default seccomp should resolve");

        assert_eq!(
            seccomp,
            SeccompMode::Enforce {
                policy_path: policy
            }
        );
    }

    #[test]
    fn default_gap_audit_resolves_to_valid_packaged_default_policy() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let policy = dir.path().join("default.json");
        fs::write(
            &policy,
            include_bytes!("../../../assets/seccomp/default.json"),
        )
        .expect("default policy fixture should be written");
        let trace_path = dir.path().join("missing.jsonl");

        let seccomp = resolve_normal_launch_seccomp_with(
            Some(SeccompMode::Audit(AuditMode::DefaultGap {
                trace_path: trace_path.clone(),
            })),
            || Some(policy.clone()),
        )
        .expect("default gap audit should resolve");

        assert_eq!(
            seccomp,
            SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path: policy,
                trace_path,
            })
        );
    }

    #[test]
    fn default_gap_audit_fails_closed_without_default_policy() {
        let err = resolve_normal_launch_seccomp_with(
            Some(SeccompMode::Audit(AuditMode::DefaultGap {
                trace_path: PathBuf::from("missing.jsonl"),
            })),
            || None,
        )
        .expect_err("missing default policy should fail closed");

        let message = format!("{err:#}");
        assert!(message.contains("default seccomp gap audit"));
        assert!(message.contains("audit:POLICY_JSON:TRACE_JSONL"));
        assert!(!message.contains("--seccomp=off"));
    }

    #[test]
    fn default_gap_audit_fails_closed_with_invalid_default_policy() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let policy = dir.path().join("invalid.json");
        fs::write(&policy, b"not json").expect("invalid policy fixture should be written");

        let err = resolve_normal_launch_seccomp_with(
            Some(SeccompMode::Audit(AuditMode::DefaultGap {
                trace_path: PathBuf::from("missing.jsonl"),
            })),
            || Some(policy),
        )
        .expect_err("invalid default policy should fail closed");

        let message = format!("{err:#}");
        assert!(message.contains("failed to load default cang seccomp policy"));
        assert!(message.contains("default seccomp gap audit"));
        assert!(message.contains("audit:POLICY_JSON:TRACE_JSONL"));
    }

    #[test]
    fn explicit_audit_modes_bypass_default_policy_lookup() {
        let full = resolve_normal_launch_seccomp_with(
            Some(SeccompMode::Audit(AuditMode::Full {
                trace_path: PathBuf::from("trace.jsonl"),
            })),
            || panic!("full audit must not resolve the packaged default policy"),
        )
        .expect("full audit should resolve");
        assert_eq!(
            full,
            SeccompMode::Audit(AuditMode::Full {
                trace_path: PathBuf::from("trace.jsonl"),
            })
        );

        let gap = resolve_normal_launch_seccomp_with(
            Some(SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path: PathBuf::from("baseline.json"),
                trace_path: PathBuf::from("missing.jsonl"),
            })),
            || panic!("explicit gap audit must not resolve the packaged default policy"),
        )
        .expect("explicit gap audit should resolve");
        assert_eq!(
            gap,
            SeccompMode::Audit(AuditMode::Gap {
                baseline_policy_path: PathBuf::from("baseline.json"),
                trace_path: PathBuf::from("missing.jsonl"),
            })
        );
    }

    #[test]
    fn explicit_normal_launch_seccomp_off_bypasses_default_policy_lookup() {
        let seccomp = resolve_normal_launch_seccomp_with(Some(SeccompMode::Off), || {
            panic!("explicit seccomp mode should not resolve the packaged default policy")
        })
        .expect("explicit off should resolve");

        assert_eq!(seccomp, SeccompMode::Off);
    }

    #[test]
    fn omitted_normal_launch_seccomp_fails_closed_without_default_policy() {
        let err = resolve_normal_launch_seccomp_with(None, || None)
            .expect_err("missing default policy should fail closed");

        let message = format!("{err:#}");
        assert!(message.contains("default seccomp policy"));
        assert!(message.contains("--seccomp=off"));
    }

    #[test]
    fn plan_prepares_host_dotfile_bind_mounts_without_codex_config() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("project");
        let home = dir.path().join("home");
        fs::create_dir_all(&workspace).expect("workspace should exist");

        let plan = LaunchPlan::from_env_values(
            runtime_options(),
            workspace.clone(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(home.as_path()),
        )
        .expect("plan should build");

        let mount = |target: &str| {
            plan.bind_mounts
                .iter()
                .find(|mount| mount.target == target)
                .expect("mount should exist")
        };
        assert_eq!(plan.bind_mounts.len(), 10);
        assert_eq!(mount("/workspace").source, workspace);
        assert_eq!(mount("/home/dev/.codex").source, home.join(".codex"));
        assert_eq!(mount("/home/dev/.omp").source, home.join(".omp"));
        assert_eq!(mount("/home/dev/.pi").source, home.join(".pi"));
        assert_eq!(
            mount("/home/dev/.local/share/cortexkit").source,
            home.join(".local/share/cortexkit")
        );
        assert_eq!(
            mount("/home/dev/.config/dirge").source,
            home.join(".config/dirge")
        );
        assert_eq!(
            mount("/home/dev/.local/share/dirge").source,
            home.join(".local/share/dirge")
        );
        assert_eq!(mount("/home/dev/.dirge").source, home.join(".dirge"));
        assert_eq!(
            mount("/home/dev/.cargo").source,
            plan.state_layout.root_dir().join("cargo")
        );
        assert_eq!(
            mount("/home/dev/.cache/sccache").source,
            plan.state_layout.sccache_dir()
        );
        assert!(home.join(".codex").is_dir());
        assert!(home.join(".omp").is_dir());
        assert!(home.join(".pi").is_dir());
        assert!(home.join(".local/share/cortexkit").is_dir());
        assert!(home.join(".config/dirge").is_dir());
        assert!(home.join(".local/share/dirge").is_dir());
        assert!(home.join(".dirge").is_dir());
        assert!(plan.state_layout.root_dir().join("cargo").is_dir());
        assert!(!plan.state_layout.root_dir().join("containers").exists());
        assert_eq!(
            fs::metadata(plan.state_layout.sccache_dir())
                .expect("sccache metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            !plan
                .bind_mounts
                .iter()
                .any(|mount| mount.target.contains(".config/codex")
                    || mount.source.to_string_lossy().contains(".config/codex"))
        );
    }

    #[test]
    fn plan_requires_home_for_builtin_home_tool_mounts() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let err = LaunchPlan::from_env_values(
            runtime_options(),
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            None,
        )
        .expect_err("HOME should be required");

        assert!(format!("{err:#}").contains("HOME is not set"));
    }

    #[test]
    fn debug_compatibility_sets_effective_log_level_in_plan() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.debug = true;
        options.log_settings = LogSettings::resolve(None, true, None);

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.log_level, LogLevel::Debug);
        assert!(plan.debug);
    }

    #[test]
    fn pull_latest_uses_canonical_refresh_image_selection() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.pull_latest = true;

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.image_selection, ImageSelection::CanonicalWithRefresh);
        assert_eq!(
            plan.image_selection.selected_reference(),
            DEFAULT_FALLBACK_IMAGE
        );
    }

    #[test]
    fn explicit_image_is_preserved_as_image_selection() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.image = Some("example/cang:dev".to_owned());

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(
            plan.image_selection,
            ImageSelection::Explicit {
                reference: "example/cang:dev".to_owned()
            }
        );
        assert_eq!(
            plan.image_selection.selected_reference(),
            "example/cang:dev"
        );
    }

    #[test]
    fn config_backend_and_state_location_are_loaded_into_plan() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let config_home = dir.path().join("config");
        let state_home = dir.path().join("ignored-state");
        let home = dir.path().join("home");
        let configured_state_root = dir.path().join("configured-state");
        fs::create_dir_all(config_home.join("cang")).expect("config dir should exist");
        fs::write(
            config_home.join("cang").join("cang.toml"),
            format!(
                "[state]\nlocation = \"{}\"\n\n[task-rootfs]\nbackend = \"fuse-overlay\"\n",
                configured_state_root.display()
            ),
        )
        .expect("config should be written");

        let plan = LaunchPlan::from_env_values(
            runtime_options(),
            PathBuf::from("/tmp/project"),
            Some(&state_home),
            Some(&config_home),
            Some(&home),
        )
        .expect("plan should build");

        assert_eq!(plan.task_rootfs_backend, TaskRootfsBackend::FuseOverlay);
        assert_eq!(
            plan.state_layout.root_dir(),
            configured_state_root.join("cang").join("project")
        );
        assert_eq!(
            plan.config_diagnostics.config_path,
            config_home.join("cang").join("cang.toml")
        );
        assert!(plan.config_diagnostics.config_loaded);
    }

    #[test]
    fn cli_backend_overrides_config_backend() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let config_home = dir.path().join("config");
        fs::create_dir_all(config_home.join("cang")).expect("config dir should exist");
        fs::write(
            config_home.join("cang").join("cang.toml"),
            "[task-rootfs]\nbackend = \"fuse-overlay\"\n",
        )
        .expect("config should be written");
        let mut options = runtime_options();
        options.rootfs_backend = Some(TaskRootfsBackend::BtrfsSnapshot);

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(config_home.as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.task_rootfs_backend, TaskRootfsBackend::BtrfsSnapshot);
    }

    #[test]
    fn default_raw_disk_container_store_does_not_add_container_bind_mount() {
        let dir = tempfile::tempdir().expect("tempdir should exist");

        let plan = LaunchPlan::from_env_values(
            runtime_options(),
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.container_store_backend, ContainerStoreBackend::RawDisk);
        assert!(
            !plan
                .bind_mounts
                .iter()
                .any(|mount| mount.target == "/home/dev/.local/share/containers")
        );
    }

    #[test]
    fn launch_plan_carries_shell_and_debug_options() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.guest_init = Some(Path::new("./cang-guest-init").to_path_buf());
        options.mem_gib = Some(8);
        options.pulse = Some(
            "tcp:[2001:db8::10]:4713"
                .parse()
                .expect("Pulse endpoint should parse"),
        );
        options.guest_command = vec!["bash".to_owned(), "-lc".to_owned(), "echo ok".to_owned()];
        options.debug = true;
        options.log_settings = LogSettings::resolve(None, true, None);
        options.profile = true;
        options.root = true;
        options.daemon = true;
        options.preserve_debug = true;

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(
            plan.guest_init,
            Some(Path::new("./cang-guest-init").to_path_buf())
        );
        assert_eq!(plan.mem_gib, Some(8));
        assert_eq!(plan.network_mode, NetworkMode::Tsi);
        assert_eq!(
            plan.pulse
                .expect("Pulse endpoint should be preserved")
                .direct_env_value()
                .as_deref(),
            Some("tcp:[2001:db8::10]:4713")
        );
        assert_eq!(plan.guest_command, ["bash", "-lc", "echo ok"]);
        assert!(plan.debug);
        assert!(plan.profile);
        assert!(plan.root);
        assert!(plan.daemon);
        assert!(plan.preserve_debug);
    }

    #[test]
    fn launch_plan_carries_passt_network_mode() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.network_mode = NetworkMode::Passt;

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.network_mode, NetworkMode::Passt);
    }

    #[test]
    fn launch_plan_carries_publish_specs() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.publish = vec!["8080:80".to_owned(), "8443:443".to_owned()];

        let plan = LaunchPlan::from_env_values(
            options,
            PathBuf::from("/tmp/project"),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("plan should build");

        assert_eq!(plan.publish, ["8080:80", "8443:443"]);
    }

    #[test]
    fn launch_plan_appends_user_directory_and_file_volumes() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("project");
        let home = dir.path().join("home");
        let source_dir = dir.path().join("host-dir");
        let source_file = workspace.join("host-file");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        fs::create_dir_all(&source_dir).expect("source dir");
        fs::write(&source_file, "data").expect("source file");
        let mut options = runtime_options();
        options.volumes = vec![
            VolumeSpec {
                source: source_dir.clone(),
                target: "/guest/dir".to_owned(),
                read_only: false,
            },
            VolumeSpec {
                source: PathBuf::from("host-file"),
                target: "/guest/file".to_owned(),
                read_only: true,
            },
        ];

        let plan = LaunchPlan::from_env_values(
            options,
            workspace.clone(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(home.as_path()),
        )
        .expect("plan should build");

        let dir_mount = plan
            .bind_mounts
            .iter()
            .find(|mount| mount.target == "/guest/dir")
            .expect("dir volume mount");
        assert_eq!(dir_mount.source, source_dir);
        assert_eq!(dir_mount.tag, "cang-user-volume-10");
        assert_eq!(dir_mount.source_kind, BindMountSourceKind::Directory);
        assert!(!dir_mount.read_only);

        let file_mount = plan
            .bind_mounts
            .iter()
            .find(|mount| mount.target == "/guest/file")
            .expect("file volume mount");
        assert_eq!(file_mount.source, source_file);
        assert_eq!(file_mount.tag, "cang-user-volume-11");
        assert_eq!(file_mount.source_kind, BindMountSourceKind::File);
        assert!(file_mount.read_only);
    }

    #[test]
    fn launch_plan_rejects_user_volume_reserved_or_duplicate_targets() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("project");
        let home = dir.path().join("home");
        let source = dir.path().join("host-dir");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        fs::create_dir_all(&source).expect("source dir");

        for target in [
            "/",
            "/.",
            "/nix",
            "/nix//",
            "/workspace",
            "/workspace/",
            "/workspace/.",
            "/home/dev/.codex",
            "/home/dev/.codex/./",
            "/home/dev/.config/dirge",
            "/home/dev/.config/dirge/./",
            "/home/dev/.local/share/dirge",
            "/home/dev/.local/share/dirge/./",
            "/home/dev/.local/share/cortexkit",
            "/home/dev/.local/share/cortexkit/./",
            "/home/dev/.dirge",
            "/home/dev/.dirge/./",
        ] {
            let mut options = runtime_options();
            options.volumes = vec![VolumeSpec {
                source: source.clone(),
                target: target.to_owned(),
                read_only: false,
            }];
            let err = LaunchPlan::from_env_values(
                options,
                workspace.clone(),
                Some(dir.path().join("state").as_path()),
                Some(dir.path().join("config").as_path()),
                Some(home.as_path()),
            )
            .expect_err("reserved or duplicate target should fail");

            let error = format!("{err:#}");
            assert!(
                error.contains("reserved")
                    || error.contains("duplicated")
                    || error.contains("must not be /"),
                "unexpected error for {target}: {error}"
            );
        }
    }

    #[test]
    fn launch_plan_rejects_missing_user_volume_source() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("project");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        let mut options = runtime_options();
        options.volumes = vec![VolumeSpec {
            source: PathBuf::from("missing"),
            target: "/guest/missing".to_owned(),
            read_only: false,
        }];

        let err = LaunchPlan::from_env_values(
            options,
            workspace,
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect_err("missing source should fail");

        assert!(format!("{err:#}").contains("failed to inspect volume source"));
    }

    #[test]
    fn waypipe_plan_uses_selected_workspace_and_socket() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("workspace");
        let socket = dir.path().join("waypipe.sock");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        let _listener = UnixListener::bind(&socket).expect("waypipe socket should exist");
        let mut options = runtime_options();
        options.workspace = Some(workspace.clone());
        options.waypipe = Some(Some(socket.clone()));
        options.guest_command = vec!["gui-application".to_owned()];

        let plan = LaunchPlan::from_env_values(
            options,
            workspace.clone(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("waypipe plan should build");

        assert_eq!(plan.workspace_dir, workspace);
        assert!(plan.waypipe);
        assert_eq!(plan.waypipe_socket, Some(socket));
    }

    #[test]
    fn waypipe_plan_supports_capability_without_initial_target() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        let mut options = runtime_options();
        options.waypipe = Some(None);

        let plan = LaunchPlan::from_env_values(
            options,
            workspace,
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("Waypipe capability plan should build without a target");

        assert!(plan.waypipe);
        assert_eq!(plan.waypipe_socket, None);
    }
    #[test]
    fn ordinary_plan_uses_selected_workspace_without_waypipe() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        let mut options = runtime_options();
        options.workspace = Some(workspace.clone());

        let plan = LaunchPlan::from_env_values(
            options,
            workspace.clone(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("ordinary plan should build");

        assert_eq!(plan.workspace_dir, workspace);
        assert_eq!(plan.waypipe_socket, None);
    }

    #[test]
    fn waypipe_plan_rejects_relative_socket() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut options = runtime_options();
        options.waypipe = Some(Some(PathBuf::from("relative.sock")));
        options.guest_command = vec!["gui-application".to_owned()];

        let err = LaunchPlan::from_env_values(
            options,
            dir.path().to_path_buf(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect_err("relative waypipe socket should fail");

        assert!(format!("{err:#}").contains("waypipe socket must be an absolute path"));
    }

    #[test]
    fn commandless_waypipe_plan_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let socket = dir.path().join("waypipe.sock");
        let _listener = UnixListener::bind(&socket).expect("waypipe socket should exist");
        let mut options = runtime_options();
        options.waypipe = Some(Some(socket.clone()));

        let plan = LaunchPlan::from_env_values(
            options,
            dir.path().to_path_buf(),
            Some(dir.path().join("state").as_path()),
            Some(dir.path().join("config").as_path()),
            Some(dir.path().join("home").as_path()),
        )
        .expect("commandless waypipe plan should build");

        assert!(plan.guest_command.is_empty());
        assert_eq!(plan.waypipe_socket, Some(socket));
    }
}
