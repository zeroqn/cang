use anyhow::{Context, Result, bail};
use std::ffi::CString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::runtime::launch::config::{BindMountSourceKind, LaunchConfig, canonical_mount_target};
pub(crate) mod etc;

use etc::RuntimeEtcFiles;

const PREPARED_ROOT_DIR: &str = "prepared-root";

#[derive(Debug)]
pub(crate) struct PreparedRoot {
    root: PathBuf,
    plan: PreparedRootPlan,
    mounted: bool,
}

impl PreparedRoot {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn unmount(mut self) -> Result<()> {
        if !self.mounted {
            return Ok(());
        }
        self.mounted = false;
        HostPreparedRootCommands.cleanup(&self.plan)
    }
}

impl Drop for PreparedRoot {
    fn drop(&mut self) {
        if !self.mounted {
            return;
        }
        if let Err(err) = HostPreparedRootCommands.cleanup(&self.plan) {
            eprintln!(
                "cang: best-effort prepared-root cleanup failed for '{}': {err:#}",
                self.root.display()
            );
        }
    }
}

pub(crate) fn prepare(config: &LaunchConfig, task_state_dir: &Path) -> Result<PreparedRoot> {
    let plan = PreparedRootPlan::new(config, task_state_dir)?;
    HostPreparedRootCommands.apply(&plan)?;
    Ok(PreparedRoot {
        root: plan.root_export.clone(),
        plan,
        mounted: true,
    })
}

pub(crate) fn cleanup_existing(config: &LaunchConfig, task_state_dir: &Path) -> Result<()> {
    let plan = PreparedRootPlan::new_for_cleanup(config, task_state_dir)?;
    HostPreparedRootCommands.cleanup(&plan)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRootPlan {
    root_export: PathBuf,
    bind_grafts: Vec<PreparedRootBind>,
    runtime_etc: RuntimeEtcFiles,
}

impl PreparedRootPlan {
    fn new(config: &LaunchConfig, task_state_dir: &Path) -> Result<Self> {
        Self::new_with_runtime_etc(config, task_state_dir, etc::build(&config.hostname)?)
    }

    fn new_for_cleanup(config: &LaunchConfig, task_state_dir: &Path) -> Result<Self> {
        Self::new_with_runtime_etc(
            config,
            task_state_dir,
            RuntimeEtcFiles {
                hostname: String::new(),
                hosts: String::new(),
                resolv_conf: String::new(),
            },
        )
    }

    fn unmount_targets(&self) -> Vec<PathBuf> {
        let mut targets = self
            .bind_grafts
            .iter()
            .map(|bind| bind.target.clone())
            .collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            path_depth(right)
                .cmp(&path_depth(left))
                .then_with(|| right.cmp(left))
        });
        targets.dedup();
        targets
    }

    fn new_with_runtime_etc(
        config: &LaunchConfig,
        task_state_dir: &Path,
        runtime_etc: RuntimeEtcFiles,
    ) -> Result<Self> {
        let root_export = task_state_dir.join(PREPARED_ROOT_DIR);
        let mut bind_grafts = Vec::with_capacity(
            config.mounts.len() + 1 + usize::from(config.guest_init_override.is_some()),
        );
        bind_grafts.push(PreparedRootBind {
            source: config.task_rootfs.clone(),
            target: root_export.clone(),
            source_kind: BindMountSourceKind::Directory,
            read_only: false,
        });
        for mount in &config.mounts {
            bind_grafts.push(PreparedRootBind {
                source: mount.source.clone(),
                target: prepared_target(&root_export, &mount.target)?,
                source_kind: mount.source_kind,
                read_only: mount.read_only,
            });
        }
        if let Some(mount) = &config.guest_init_override {
            bind_grafts.push(PreparedRootBind {
                source: mount.source.clone(),
                target: prepared_target(&root_export, &mount.target)?,
                source_kind: BindMountSourceKind::File,
                read_only: true,
            });
        }
        Ok(Self {
            root_export,
            bind_grafts,
            runtime_etc,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRootBind {
    source: PathBuf,
    target: PathBuf,
    source_kind: BindMountSourceKind,
    read_only: bool,
}

trait PreparedRootCommands {
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn create_file_target(&self, path: &Path) -> Result<()>;
    fn bind_mount(&self, source: &Path, target: &Path) -> Result<()>;
    fn remount_read_only(&self, target: &Path) -> Result<()>;
    fn unmount(&self, target: &Path) -> Result<()>;
    fn materialize_runtime_etc(&self, root_export: &Path, files: &RuntimeEtcFiles) -> Result<()>;

    fn apply(&self, plan: &PreparedRootPlan) -> Result<()> {
        for bind in &plan.bind_grafts {
            match bind.source_kind {
                BindMountSourceKind::Directory => {
                    validate_source_dir(&bind.source)?;
                    self.create_dir_all(&bind.target)?;
                }
                BindMountSourceKind::File => {
                    validate_source_file(&bind.source)?;
                    self.create_file_target(&bind.target)?;
                }
            }
            self.bind_mount(&bind.source, &bind.target)?;
            if bind.read_only {
                self.remount_read_only(&bind.target)?;
            }
        }
        self.materialize_runtime_etc(&plan.root_export, &plan.runtime_etc)?;
        Ok(())
    }

    fn cleanup(&self, plan: &PreparedRootPlan) -> Result<()> {
        for target in plan.unmount_targets() {
            self.unmount(&target).with_context(|| {
                format!(
                    "failed to unmount cang prepared-root bind target '{}'",
                    target.display()
                )
            })?;
        }
        Ok(())
    }
}

struct HostPreparedRootCommands;

impl PreparedRootCommands for HostPreparedRootCommands {
    fn create_dir_all(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create prepared root path '{}'", path.display()))
    }

    fn create_file_target(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create prepared root file parent '{}'",
                    parent.display()
                )
            })?;
        }
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("failed to create prepared root file '{}'", path.display()))?;
        Ok(())
    }

    fn bind_mount(&self, source: &Path, target: &Path) -> Result<()> {
        let status = Command::new("mount")
            .args(["--bind"])
            .arg(source)
            .arg(target)
            .status()
            .with_context(|| {
                format!(
                    "failed to run rootless prepared-root bind mount '{}'=>'{}'",
                    source.display(),
                    target.display()
                )
            })?;
        if !status.success() {
            bail!(
                "rootless prepared-root bind mount '{}'=>'{}' failed with {status}",
                source.display(),
                target.display()
            );
        }
        Ok(())
    }

    fn remount_read_only(&self, target: &Path) -> Result<()> {
        let status = Command::new("mount")
            .args(["-o", "remount,bind,ro"])
            .arg(target)
            .status()
            .with_context(|| {
                format!(
                    "failed to run rootless prepared-root read-only remount '{}'",
                    target.display()
                )
            })?;
        if !status.success() {
            bail!(
                "rootless prepared-root read-only remount '{}' failed with {status}",
                target.display()
            );
        }
        Ok(())
    }

    fn unmount(&self, target: &Path) -> Result<()> {
        umount_path(target).with_context(|| {
            format!(
                "failed to unmount cang prepared-root bind target '{}'",
                target.display()
            )
        })
    }

    fn materialize_runtime_etc(&self, root_export: &Path, files: &RuntimeEtcFiles) -> Result<()> {
        etc::materialize(root_export, files)
    }
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn validate_source_file(source: &Path) -> Result<()> {
    if !source.is_absolute() {
        bail!(
            "cang prepared-root bind source '{}' must be absolute",
            source.display()
        );
    }
    let metadata = fs::metadata(source).with_context(|| {
        format!(
            "failed to inspect prepared-root source '{}'",
            source.display()
        )
    })?;
    if !metadata.is_file() {
        bail!(
            "cang prepared-root bind source '{}' must be a file",
            source.display()
        );
    }
    Ok(())
}

fn validate_source_dir(source: &Path) -> Result<()> {
    if !source.is_absolute() {
        bail!(
            "cang prepared-root bind source '{}' must be absolute",
            source.display()
        );
    }
    let metadata = fs::metadata(source).with_context(|| {
        format!(
            "failed to inspect prepared-root source '{}'",
            source.display()
        )
    })?;
    if !metadata.is_dir() {
        bail!(
            "cang prepared-root bind source '{}' must be a directory",
            source.display()
        );
    }
    Ok(())
}

fn prepared_target(root_export: &Path, mount_target: &str) -> Result<PathBuf> {
    let canonical = canonical_mount_target(mount_target)?;
    if canonical != mount_target {
        bail!(
            "cang prepared-root bind target '{}' must be canonical absolute path '{}'",
            mount_target,
            canonical
        );
    }
    let mut relative = PathBuf::new();
    for component in Path::new(&canonical).components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => relative.push(value),
            Component::CurDir => {}
            Component::ParentDir => unreachable!("canonical target rejects parent dirs"),
            Component::Prefix(_) => unreachable!("canonical target rejects prefixes"),
        }
    }
    Ok(root_export.join(relative))
}

fn umount_path(target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(target.as_os_str().as_bytes()).with_context(|| {
        format!(
            "prepared-root unmount target '{}' contains an interior NUL byte",
            target.display()
        )
    })?;
    // SAFETY: `raw` is a NUL-terminated copy of the target path and remains
    // valid for the duration of the syscall.
    let rc = unsafe { libc::umount(raw.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if matches!(
            err.raw_os_error(),
            Some(code) if code == libc::EINVAL || code == libc::ENOENT
        ) {
            return Ok(());
        }
        Err(err).with_context(|| {
            format!(
                "rootless prepared-root unmount '{}' failed",
                target.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogLevel;
    use crate::runtime::launch::config::{
        BindMount, CARGO_TAG, CARGO_TARGET, CODEX_TAG, CODEX_TARGET, CORTEXKIT_TAG,
        CORTEXKIT_TARGET, DIRGE_CONFIG_TAG, DIRGE_CONFIG_TARGET, DIRGE_DATA_TAG, DIRGE_DATA_TARGET,
        DIRGE_HOME_TAG, DIRGE_HOME_TARGET, GuestInitOverrideMount, LaunchSpec, NetworkMode,
        OMP_TAG, OMP_TARGET, PI_TAG, PI_TARGET, SCCACHE_TAG, SCCACHE_TARGET, WORKSPACE_TAG,
        WORKSPACE_TARGET,
    };
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Default)]
    struct RecordingCommands {
        calls: RefCell<Vec<Call>>,
        fail_unmount: RefCell<Option<String>>,
    }

    impl RecordingCommands {
        fn fail_unmount(&self, path: &Path) {
            *self.fail_unmount.borrow_mut() = Some(path.display().to_string());
        }

        fn unmount_calls(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .filter_map(|call| match call {
                    Call::Unmount(target) => Some(target.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        CreateDir(String),
        CreateFile(String),
        Bind(String, String),
        RuntimeEtc(String),
        ReadOnly(String),
        Unmount(String),
    }

    impl PreparedRootCommands for RecordingCommands {
        fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::CreateDir(path.display().to_string()));
            Ok(())
        }

        fn create_file_target(&self, path: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::CreateFile(path.display().to_string()));
            Ok(())
        }

        fn bind_mount(&self, source: &Path, target: &Path) -> Result<()> {
            self.calls.borrow_mut().push(Call::Bind(
                source.display().to_string(),
                target.display().to_string(),
            ));
            Ok(())
        }

        fn remount_read_only(&self, target: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::ReadOnly(target.display().to_string()));
            Ok(())
        }

        fn unmount(&self, target: &Path) -> Result<()> {
            let target = target.display().to_string();
            self.calls.borrow_mut().push(Call::Unmount(target.clone()));
            if self.fail_unmount.borrow().as_deref() == Some(target.as_str()) {
                bail!("synthetic unmount failure for {target}");
            }
            Ok(())
        }

        fn materialize_runtime_etc(
            &self,
            root_export: &Path,
            _files: &RuntimeEtcFiles,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::RuntimeEtc(root_export.display().to_string()));
            Ok(())
        }
    }

    fn test_mounts(root: &Path) -> Vec<BindMount> {
        vec![
            BindMount::directory(root.join("workspace-src"), WORKSPACE_TAG, WORKSPACE_TARGET),
            BindMount::directory(root.join("home/.codex"), CODEX_TAG, CODEX_TARGET),
            BindMount::directory(root.join("home/.omp"), OMP_TAG, OMP_TARGET),
            BindMount::directory(root.join("home/.pi"), PI_TAG, PI_TARGET),
            BindMount::directory(
                root.join("home/.local/share/cortexkit"),
                CORTEXKIT_TAG,
                CORTEXKIT_TARGET,
            ),
            BindMount::directory(
                root.join("home/.config/dirge"),
                DIRGE_CONFIG_TAG,
                DIRGE_CONFIG_TARGET,
            ),
            BindMount::directory(
                root.join("home/.local/share/dirge"),
                DIRGE_DATA_TAG,
                DIRGE_DATA_TARGET,
            ),
            BindMount::directory(root.join("home/.dirge"), DIRGE_HOME_TAG, DIRGE_HOME_TARGET),
            BindMount::directory(root.join("state/cargo"), CARGO_TAG, CARGO_TARGET),
            BindMount::directory(root.join("state/sccache"), SCCACHE_TAG, SCCACHE_TARGET),
        ]
    }

    fn config(root: &Path) -> LaunchConfig {
        LaunchConfig::build_for_task(LaunchSpec {
            task_rootfs: &root.join("task/rootfs"),
            hostname: "cang-workspace",
            mounts: &test_mounts(root),
            guest_init_override: None,
            guest_init_exec: "/nix/store/hash-cang/bin/cang-guest-init",
            guest_command: &[],
            image_process_config:
                &crate::runtime::session::rootfs::image_source::OciProcessConfig::default(),
            mem_gib: Some(4),
            log_level: LogLevel::Off,
            network_mode: NetworkMode::Tsi,
            pulse: None,
            gpu_mode: crate::runtime::vm::gpu::GpuMode::Off,
            zero_copy_shm: false,
            wayland: false,
            new_perms: crate::runtime::launch::config::GuestPermissions::default(),
            publish: &[],
            profile: false,
            root: false,
            allocator: crate::runtime::launch::config::AllocatorMode::Mimalloc,
            host_uid: 1000,
            host_gid: 1001,
            vcpus: 2,
            disks: Vec::new(),
            extra_env: Vec::new(),
            host_nix_overlay: None,
            pulse_bridge: None,
            waypipe: None,
            exec: None,
            managed_session: None,
        })
        .expect("config should build")
    }

    fn runtime_etc() -> RuntimeEtcFiles {
        RuntimeEtcFiles {
            hostname: "cang-workspace\n".to_owned(),
            hosts:
                "127.0.0.1\tlocalhost cang-workspace\n::1\tlocalhost ip6-localhost ip6-loopback\n"
                    .to_owned(),
            resolv_conf: "nameserver 192.0.2.53\n".to_owned(),
        }
    }

    #[test]
    fn prepared_root_plan_binds_task_root_before_developer_grafts() {
        let dir = tempfile::tempdir().expect("tempdir");
        for path in [
            "task/rootfs",
            "workspace-src",
            "home/.codex",
            "home/.omp",
            "home/.pi",
            "home/.local/share/cortexkit",
            "home/.config/dirge",
            "home/.local/share/dirge",
            "home/.dirge",
            "state/cargo",
            "state/sccache",
        ] {
            fs::create_dir_all(dir.path().join(path)).expect("source dir");
        }
        let state = dir.path().join("task");
        let config = config(dir.path());
        let plan =
            PreparedRootPlan::new_with_runtime_etc(&config, &state, runtime_etc()).expect("plan");
        let commands = RecordingCommands::default();

        commands.apply(&plan).expect("apply should plan commands");

        let root = state.join(PREPARED_ROOT_DIR);
        let calls = commands.calls.borrow();
        assert_eq!(calls[0], Call::CreateDir(root.display().to_string()));
        assert_eq!(
            calls[1],
            Call::Bind(
                dir.path().join("task/rootfs").display().to_string(),
                root.display().to_string()
            )
        );
        assert_eq!(
            calls[3],
            Call::Bind(
                dir.path().join("workspace-src").display().to_string(),
                root.join("workspace").display().to_string()
            )
        );
        assert_eq!(
            calls[5],
            Call::Bind(
                dir.path().join("home/.codex").display().to_string(),
                root.join("home/dev/.codex").display().to_string()
            )
        );
        assert_eq!(
            calls[21],
            Call::Bind(
                dir.path().join("state/sccache").display().to_string(),
                root.join("home/dev/.cache/sccache").display().to_string()
            )
        );
        assert_eq!(calls[22], Call::RuntimeEtc(root.display().to_string()));
    }

    #[test]
    fn prepared_root_plan_applies_guest_init_override_as_read_only_file_bind() {
        let dir = tempfile::tempdir().expect("tempdir");
        for path in [
            "task/rootfs/nix/store/hash-cang/bin",
            "workspace-src",
            "home/.codex",
            "home/.omp",
            "home/.pi",
            "home/.local/share/cortexkit",
            "home/.config/dirge",
            "home/.local/share/dirge",
            "home/.dirge",
            "state/cargo",
            "state/sccache",
        ] {
            fs::create_dir_all(dir.path().join(path)).expect("source dir");
        }
        let image_target = dir
            .path()
            .join("task/rootfs/nix/store/hash-cang/bin/cang-guest-init");
        fs::write(&image_target, "#!/bin/sh\n").expect("image guest init");
        let override_path = dir.path().join("override-cang-guest-init");
        fs::write(&override_path, "#!/bin/sh\necho override\n").expect("override guest init");

        let state = dir.path().join("task");
        let mut config = config(dir.path());
        config.guest_init_override = Some(GuestInitOverrideMount {
            source: override_path.clone(),
            target: "/nix/store/hash-cang/bin/cang-guest-init".to_owned(),
            read_only: true,
        });
        let plan =
            PreparedRootPlan::new_with_runtime_etc(&config, &state, runtime_etc()).expect("plan");
        let commands = RecordingCommands::default();

        commands.apply(&plan).expect("apply should plan commands");

        let calls = commands.calls.borrow();
        let root = state.join(PREPARED_ROOT_DIR);
        assert_eq!(
            calls[22],
            Call::CreateFile(
                root.join("nix/store/hash-cang/bin/cang-guest-init")
                    .display()
                    .to_string()
            )
        );
        assert_eq!(
            calls[23],
            Call::Bind(
                override_path.display().to_string(),
                root.join("nix/store/hash-cang/bin/cang-guest-init")
                    .display()
                    .to_string()
            )
        );
        assert_eq!(
            calls[24],
            Call::ReadOnly(
                root.join("nix/store/hash-cang/bin/cang-guest-init")
                    .display()
                    .to_string()
            )
        );
        assert_eq!(calls[25], Call::RuntimeEtc(root.display().to_string()));
    }

    #[test]
    fn prepared_root_plan_applies_user_file_and_read_only_directory_binds() {
        let dir = tempfile::tempdir().expect("tempdir");
        for path in [
            "task/rootfs",
            "workspace-src",
            "home/.codex",
            "home/.omp",
            "home/.pi",
            "home/.local/share/cortexkit",
            "home/.config/dirge",
            "home/.local/share/dirge",
            "home/.dirge",
            "state/cargo",
            "state/sccache",
            "host-read-only-dir",
        ] {
            fs::create_dir_all(dir.path().join(path)).expect("source dir");
        }
        let host_file = dir.path().join("host-config.json");
        fs::write(&host_file, "{}").expect("host file");

        let state = dir.path().join("task");
        let mut config = config(dir.path());
        let file_volume_tag = format!("cang-user-volume-{}", config.mounts.len());
        config.mounts.push(BindMount::file(
            host_file.clone(),
            file_volume_tag,
            "/workspace/config.json",
            true,
        ));
        let dir_volume_tag = format!("cang-user-volume-{}", config.mounts.len());
        config.mounts.push(BindMount {
            source: dir.path().join("host-read-only-dir"),
            tag: dir_volume_tag,
            target: "/workspace/readonly".to_owned(),
            source_kind: BindMountSourceKind::Directory,
            read_only: true,
        });
        let plan =
            PreparedRootPlan::new_with_runtime_etc(&config, &state, runtime_etc()).expect("plan");
        let commands = RecordingCommands::default();

        commands.apply(&plan).expect("apply should plan commands");

        let root = state.join(PREPARED_ROOT_DIR);
        let calls = commands.calls.borrow();
        assert!(calls.contains(&Call::CreateFile(
            root.join("workspace/config.json").display().to_string()
        )));
        assert!(calls.contains(&Call::Bind(
            host_file.display().to_string(),
            root.join("workspace/config.json").display().to_string()
        )));
        assert!(calls.contains(&Call::ReadOnly(
            root.join("workspace/config.json").display().to_string()
        )));
        assert!(calls.contains(&Call::CreateDir(
            root.join("workspace/readonly").display().to_string()
        )));
        assert!(calls.contains(&Call::ReadOnly(
            root.join("workspace/readonly").display().to_string()
        )));
    }

    #[test]
    fn prepared_root_cleanup_unmounts_deepest_grafts_before_root_export() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("task");
        let root = state.join(PREPARED_ROOT_DIR);
        let mut config = config(dir.path());
        let host_file = dir.path().join("host-config.json");
        let file_volume_tag = format!("cang-user-volume-{}", config.mounts.len());
        config.mounts.push(BindMount::file(
            host_file,
            file_volume_tag,
            "/workspace/config.json",
            true,
        ));
        config.guest_init_override = Some(GuestInitOverrideMount {
            source: dir.path().join("override-cang-guest-init"),
            target: "/nix/store/hash-cang/bin/cang-guest-init".to_owned(),
            read_only: true,
        });
        let plan =
            PreparedRootPlan::new_with_runtime_etc(&config, &state, runtime_etc()).expect("plan");
        let commands = RecordingCommands::default();

        commands.cleanup(&plan).expect("cleanup should unmount");

        let unmounts = commands.unmount_calls();
        assert_eq!(unmounts.last(), Some(&root.display().to_string()));
        let root_position = unmounts
            .iter()
            .position(|target| target == &root.display().to_string())
            .expect("root export unmounted");
        for target in [
            root.join("workspace/config.json"),
            root.join("nix/store/hash-cang/bin/cang-guest-init"),
            root.join("home/dev/.cache/sccache"),
        ] {
            let position = unmounts
                .iter()
                .position(|actual| actual == &target.display().to_string())
                .unwrap_or_else(|| panic!("missing unmount for {}", target.display()));
            assert!(
                position < root_position,
                "{} should unmount before root export",
                target.display()
            );
        }
    }

    #[test]
    fn prepared_root_cleanup_attempts_known_targets_without_mountinfo_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("task");
        let config = config(dir.path());
        let plan =
            PreparedRootPlan::new_with_runtime_etc(&config, &state, runtime_etc()).expect("plan");
        let expected = plan
            .unmount_targets()
            .into_iter()
            .map(|target| target.display().to_string())
            .collect::<Vec<_>>();
        let commands = RecordingCommands::default();

        commands
            .cleanup(&plan)
            .expect("cleanup should be idempotent");

        assert_eq!(commands.unmount_calls(), expected);
    }

    #[test]
    fn prepared_root_cleanup_reports_unmount_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("task");
        let config = config(dir.path());
        let plan =
            PreparedRootPlan::new_with_runtime_etc(&config, &state, runtime_etc()).expect("plan");
        let root = state.join(PREPARED_ROOT_DIR);
        let commands = RecordingCommands::default();
        commands.fail_unmount(&root);

        let err = commands
            .cleanup(&plan)
            .expect_err("unmount failure should surface");

        assert!(format!("{err:#}").contains("failed to unmount cang prepared-root bind target"));
    }

    #[test]
    fn prepared_root_rejects_relative_or_root_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("prepared-root");
        let mut mount = BindMount::directory(dir.path().join("src"), "bad", "workspace");
        assert!(prepared_target(&root, &mount.target).is_err());

        mount.target = "/".to_owned();
        assert!(prepared_target(&root, &mount.target).is_err());

        mount.target = "/workspace/../escape".to_owned();
        assert!(prepared_target(&root, &mount.target).is_err());

        mount.target = "/workspace/.".to_owned();
        assert!(prepared_target(&root, &mount.target).is_err());

        mount.target = "/workspace/".to_owned();
        assert!(prepared_target(&root, &mount.target).is_err());
    }
}
