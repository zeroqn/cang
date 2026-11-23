use super::*;
use crate::runtime::session::profile::RootfsMaterializationRecorder;
use crate::runtime::session::rootfs::image_source::{
    BuildahCommands, ImageSourceCacheProfile, ImageSourceCacheStatus, OciProcessConfig,
};
use crate::{DEFAULT_FALLBACK_IMAGE, DEFAULT_IMAGE};
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
enum BtrfsCall {
    Snapshot {
        source: PathBuf,
        destination: PathBuf,
    },
    Delete(PathBuf),
}

#[derive(Debug)]
struct FakeBtrfsRootfsCommands {
    calls: RefCell<Vec<BtrfsCall>>,
    snapshot_errors: RefCell<VecDeque<&'static str>>,
    delete_errors: RefCell<VecDeque<&'static str>>,
}

impl FakeBtrfsRootfsCommands {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            snapshot_errors: RefCell::new(VecDeque::new()),
            delete_errors: RefCell::new(VecDeque::new()),
        }
    }

    fn fail_delete(self, message: &'static str) -> Self {
        self.delete_errors.borrow_mut().push_back(message);
        self
    }

    fn calls(&self) -> Vec<BtrfsCall> {
        self.calls.borrow().clone()
    }
}

impl BtrfsRootfsCommands for FakeBtrfsRootfsCommands {
    fn snapshot_btrfs_subvolume(&self, source: &Path, destination: &Path) -> Result<()> {
        self.calls.borrow_mut().push(BtrfsCall::Snapshot {
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
        });
        if let Some(message) = self.snapshot_errors.borrow_mut().pop_front() {
            anyhow::bail!(message);
        }
        copy_rootfs_tree(source, destination)
    }

    fn delete_btrfs_subvolume(&self, subvolume: &Path) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(BtrfsCall::Delete(subvolume.to_path_buf()));
        if let Some(message) = self.delete_errors.borrow_mut().pop_front() {
            anyhow::bail!(message);
        }
        if subvolume.exists() {
            fs::remove_dir_all(subvolume).with_context(|| {
                format!("failed to remove fake subvolume '{}'", subvolume.display())
            })?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct FakeBuildahCommands {
    output: RefCell<Result<String, &'static str>>,
    image_digest: Option<String>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeBuildahCommands {
    fn success(rootfs_path: &Path, selected_image: &str) -> Self {
        Self {
            output: RefCell::new(Ok(format!(
                "selected_image={selected_image}\nimage_digest=sha256:feedface\nrootfs_path={}\noci_env.0=504154483d2f6e69782f73746f72652f666973682f62696e\n",
                rootfs_path.display()
            ))),
            image_digest: None,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn fail(message: &'static str) -> Self {
        Self {
            output: RefCell::new(Err(message)),
            image_digest: None,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn with_image_digest(mut self, digest: &str) -> Self {
        self.image_digest = Some(digest.to_owned());
        self
    }
}

impl BuildahCommands for FakeBuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        match args {
            ["--version"] => Ok("buildah version 1.42.0\n".to_owned()),
            ["inspect", "--type", "image", "--format", "{{.Digest}}", _]
            | [
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.FromImageDigest}}",
                _,
            ] => self
                .image_digest
                .as_ref()
                .map(|digest| format!("{digest}\n"))
                .ok_or_else(|| anyhow!("image digest unavailable")),
            ["pull", _] => Ok("pulled\n".to_owned()),
            other => anyhow::bail!("unexpected buildah args: {other:?}"),
        }
    }

    fn status(&self, args: &[&str]) -> Result<bool> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        Ok(matches!(
            args,
            ["inspect", "--type", "image", DEFAULT_IMAGE]
        ))
    }

    fn run_unshare_materializer(&self, args: &[&str]) -> Result<String> {
        self.calls.borrow_mut().push(
            std::iter::once("unshare-materializer".to_owned())
                .chain(args.iter().map(|arg| (*arg).to_owned()))
                .collect(),
        );
        let destination = Path::new(args[2]);
        match &*self.output.borrow() {
            Ok(output) => {
                fs::create_dir_all(destination).expect("fake rootfs should be created");
                Ok(output.clone())
            }
            Err(message) => {
                fs::create_dir_all(destination).expect("fake partial rootfs should be created");
                anyhow::bail!(*message)
            }
        }
    }
}

#[derive(Debug, Default)]
struct RecordingRootfsProfile {
    labels: Vec<&'static str>,
}

impl RecordingRootfsProfile {
    fn labels(&self) -> Vec<&'static str> {
        self.labels.clone()
    }
}

impl RootfsMaterializationRecorder for RecordingRootfsProfile {
    fn measure_result<T, F>(&mut self, label: &'static str, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        self.labels.push(label);
        f()
    }
}

#[test]
fn manager_materializes_btrfs_task_rootfs_handle_from_buildah_source() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let state_root = temp.path().join("state");
    let task_id = "task-1";
    let rootfs_path = state_root
        .join(TASKS_DIR)
        .join(task_id)
        .join(BTRFS_ROOTFS_DIR);
    let buildah = FakeBuildahCommands::success(&rootfs_path, DEFAULT_IMAGE);
    let btrfs = FakeBtrfsRootfsCommands::new();
    let manager = TaskRootfsManager::new(state_root.clone(), temp.path().join("image-cache"));

    let handle = manager
        .materialize_btrfs_from_buildah(
            &ImageSelection::PreferLocalhostThenCanonical,
            task_id,
            false,
            &buildah,
            &btrfs,
        )
        .expect("handle should materialize");

    assert_eq!(handle.task_id(), task_id);
    assert_eq!(handle.task_dir(), state_root.join(TASKS_DIR).join(task_id));
    assert_eq!(handle.rootfs_path(), rootfs_path);
    assert_eq!(handle.backend(), TaskRootfsBackend::BtrfsSnapshot);
    assert_eq!(handle.selected_image_reference(), DEFAULT_IMAGE);
    assert_eq!(handle.image_digest(), Some("sha256:feedface"));
    assert_eq!(
        handle.process_config().env,
        vec!["PATH=/nix/store/fish/bin".to_owned()]
    );
    assert_eq!(
        handle.cache_profile().status,
        ImageSourceCacheStatus::MissPopulated
    );
    assert_eq!(
        handle.cache_profile().digest_key.as_deref(),
        Some("sha256-feedface")
    );
}

#[test]
fn manager_profile_starts_with_task_directory_reset() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let state_root = temp.path().join("state");
    let task_id = "task-profile";
    let rootfs_path = state_root
        .join(TASKS_DIR)
        .join(task_id)
        .join(BTRFS_ROOTFS_DIR);
    let buildah =
        FakeBuildahCommands::success(&rootfs_path, DEFAULT_IMAGE).with_image_digest("sha256:local");
    let btrfs = FakeBtrfsRootfsCommands::new();
    let manager = TaskRootfsManager::new(state_root, temp.path().join("image-cache"));
    let mut profile = RecordingRootfsProfile::default();

    manager
        .materialize_btrfs_from_buildah_profiled(
            &ImageSelection::PreferLocalhostThenCanonical,
            task_id,
            false,
            &buildah,
            &btrfs,
            &mut profile,
        )
        .expect("handle should materialize");

    assert_eq!(
        profile.labels(),
        vec![
            "task_rootfs_materialization:reset_task_dir",
            "task_rootfs_materialization:buildah_version",
            "task_rootfs_materialization:image_probe",
            "task_rootfs_materialization:cache_entry_read",
            "task_rootfs_materialization:buildah_materializer",
            "task_rootfs_materialization:cache_population",
        ]
    );
}

#[test]
fn manager_uses_cached_source_for_distinct_task_rootfs_paths() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let state_root = temp.path().join("state");
    let image_cache = temp.path().join("image-cache");
    let cache_entry = image_cache.join("btrfs-snapshots").join("sha256-feedface");
    let cache_rootfs = cache_entry.join("rootfs");
    let guest_init = cache_rootfs.join("nix/store/hash-cang/bin/cang-guest-init");
    fs::create_dir_all(guest_init.parent().unwrap()).expect("guest init parent");
    fs::write(&guest_init, "#!/bin/sh\n").expect("guest init");
    fs::set_permissions(&guest_init, fs::Permissions::from_mode(0o755)).expect("guest init mode");
    fs::write(
        cache_entry.join("metadata"),
        "selected_image=localhost/cang:latest\nimage_digest=sha256:feedface\noci_env.0=504154483d2f6e69782f73746f72652f666973682f62696e\n",
    )
    .expect("cache metadata");

    let buildah =
        FakeBuildahCommands::fail("unshare should not run").with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();
    let manager = TaskRootfsManager::new(state_root.clone(), image_cache);

    let first = manager
        .materialize_btrfs_from_buildah(
            &ImageSelection::PreferLocalhostThenCanonical,
            "task-1",
            false,
            &buildah,
            &btrfs,
        )
        .expect("first task should hit cache");
    let second = manager
        .materialize_btrfs_from_buildah(
            &ImageSelection::PreferLocalhostThenCanonical,
            "task-2",
            false,
            &buildah,
            &btrfs,
        )
        .expect("second task should hit cache");

    assert_ne!(first.rootfs_path(), second.rootfs_path());
    assert_eq!(first.cache_profile().status, ImageSourceCacheStatus::Hit);
    assert_eq!(second.cache_profile().status, ImageSourceCacheStatus::Hit);
    assert_eq!(
        first.rootfs_path(),
        state_root
            .join(TASKS_DIR)
            .join("task-1")
            .join(BTRFS_ROOTFS_DIR)
    );
    assert_eq!(
        second.rootfs_path(),
        state_root
            .join(TASKS_DIR)
            .join("task-2")
            .join(BTRFS_ROOTFS_DIR)
    );
    assert_eq!(
        btrfs.calls(),
        vec![
            BtrfsCall::Snapshot {
                source: cache_rootfs.clone(),
                destination: first.rootfs_path().to_path_buf(),
            },
            BtrfsCall::Snapshot {
                source: cache_rootfs,
                destination: second.rootfs_path().to_path_buf(),
            },
        ]
    );
}

#[test]
fn materialization_failure_cleans_partial_task_rootfs_without_fallback() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let state_root = temp.path().join("state");
    let task_dir = state_root.join(TASKS_DIR).join("task-fail");
    let buildah = FakeBuildahCommands::fail("snapshot failed");
    let btrfs = FakeBtrfsRootfsCommands::new();
    let manager = TaskRootfsManager::new(state_root, temp.path().join("image-cache"));

    let error = manager
        .materialize_btrfs_from_buildah(
            &ImageSelection::CanonicalWithRefresh,
            "task-fail",
            false,
            &buildah,
            &btrfs,
        )
        .expect_err("required btrfs snapshot should fail");

    assert!(format!("{error:#}").contains("snapshot failed"));
    assert!(!task_dir.exists());
    assert_eq!(
        btrfs.calls(),
        vec![BtrfsCall::Delete(task_dir.join(BTRFS_ROOTFS_DIR))]
    );
}

#[test]
fn preserve_debug_keeps_partial_rootfs_on_failure() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let state_root = temp.path().join("state");
    let task_dir = state_root.join(TASKS_DIR).join("task-preserve");
    let buildah = FakeBuildahCommands::fail("snapshot failed");
    let btrfs = FakeBtrfsRootfsCommands::new();
    let manager = TaskRootfsManager::new(state_root, temp.path().join("image-cache"));

    let error = manager
        .materialize_btrfs_from_buildah(
            &ImageSelection::CanonicalWithRefresh,
            "task-preserve",
            true,
            &buildah,
            &btrfs,
        )
        .expect_err("required btrfs snapshot should fail");

    assert!(format!("{error:#}").contains("preserved partial"));
    assert!(task_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn cleanup_delete_failure_preserves_state_for_manual_recovery() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let state_root = temp.path().join("state");
    let task_dir = state_root.join(TASKS_DIR).join("task-clean");
    let rootfs_path = task_dir.join(BTRFS_ROOTFS_DIR);
    fs::create_dir_all(&rootfs_path).expect("rootfs should exist");
    fs::write(task_dir.join("launch.conf"), "config").expect("state file should exist");
    let handle = TaskRootfsHandle {
        task_id: "task-clean".to_owned(),
        task_dir: task_dir.clone(),
        rootfs_path: rootfs_path.clone(),
        backend: TaskRootfsBackend::BtrfsSnapshot,
        selected_image_reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
        image_digest: None,
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
        preserve_debug: false,
    };
    let commands = FakeBtrfsRootfsCommands::new().fail_delete("operation not permitted");

    let error = handle
        .cleanup_state(&commands)
        .expect_err("delete failure should stop cleanup");

    let message = format!("{error:#}");
    assert!(message.contains("failed to delete btrfs snapshot"));
    assert!(message.contains("user_subvol_rm_allowed"));
    assert!(task_dir.join("launch.conf").exists());
    assert_eq!(commands.calls(), vec![BtrfsCall::Delete(rootfs_path)]);
}

#[test]
fn cleanup_task_rootfs_dir_removes_task_snapshot_without_touching_image_cache() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_dir = temp.path().join("state/tasks/task-clean");
    let rootfs_path = task_dir.join(BTRFS_ROOTFS_DIR);
    let image_cache_rootfs = temp
        .path()
        .join("image-cache/btrfs-snapshots/sha256-feedface/rootfs");
    fs::create_dir_all(&rootfs_path).expect("task rootfs should exist");
    fs::create_dir_all(&image_cache_rootfs).expect("image cache rootfs should exist");
    fs::write(image_cache_rootfs.join("etc-release"), "cached").expect("cache marker");
    let commands = FakeBtrfsRootfsCommands::new();

    cleanup_task_rootfs_dir(&task_dir, &commands).expect("task cleanup should succeed");

    assert!(!task_dir.exists());
    assert!(
        image_cache_rootfs.join("etc-release").exists(),
        "task cleanup must not delete image-source cache roots"
    );
    assert_eq!(commands.calls(), vec![BtrfsCall::Delete(rootfs_path)]);
}

#[test]
fn lease_explicit_cleanup_reports_delete_failure() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_dir = temp.path().join("task-clean");
    let rootfs_path = task_dir.join(BTRFS_ROOTFS_DIR);
    fs::create_dir_all(&rootfs_path).expect("rootfs should exist");
    let handle = TaskRootfsHandle {
        task_id: "task-clean".to_owned(),
        task_dir: task_dir.clone(),
        rootfs_path,
        backend: TaskRootfsBackend::BtrfsSnapshot,
        selected_image_reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
        image_digest: None,
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
        preserve_debug: false,
    };
    let commands = FakeBtrfsRootfsCommands::new().fail_delete("operation not permitted");

    let error = TaskRootfsLease::new(handle, commands)
        .cleanup()
        .expect_err("explicit cleanup failure should be reportable");

    assert!(format!("{error:#}").contains("failed to delete btrfs snapshot"));
    assert!(task_dir.exists());
}

#[test]
fn lease_drop_fallback_is_best_effort_and_non_panicking() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_dir = temp.path().join("task-drop");
    let rootfs_path = task_dir.join(BTRFS_ROOTFS_DIR);
    fs::create_dir_all(&rootfs_path).expect("rootfs should exist");
    let handle = TaskRootfsHandle {
        task_id: "task-drop".to_owned(),
        task_dir: task_dir.clone(),
        rootfs_path,
        backend: TaskRootfsBackend::BtrfsSnapshot,
        selected_image_reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
        image_digest: None,
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
        preserve_debug: false,
    };
    let commands = FakeBtrfsRootfsCommands::new().fail_delete("operation not permitted");

    drop(TaskRootfsLease::new(handle, commands));

    assert!(task_dir.exists(), "failed drop cleanup preserves state");
}

#[test]
fn lease_preserve_debug_disables_automatic_cleanup() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_dir = temp.path().join("task-preserved");
    let rootfs_path = task_dir.join(BTRFS_ROOTFS_DIR);
    fs::create_dir_all(&rootfs_path).expect("rootfs should exist");
    let handle = TaskRootfsHandle {
        task_id: "task-preserved".to_owned(),
        task_dir: task_dir.clone(),
        rootfs_path: rootfs_path.clone(),
        backend: TaskRootfsBackend::BtrfsSnapshot,
        selected_image_reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
        image_digest: None,
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
        preserve_debug: true,
    };
    let commands = FakeBtrfsRootfsCommands::new();

    let result = TaskRootfsLease::new(handle, commands).preserve();

    assert_eq!(result, CleanupResult::Preserved(rootfs_path));
    assert!(task_dir.exists());
}

#[test]
fn snapshot_mounted_rootfs_uses_btrfs_snapshot_command() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    fs::create_dir_all(source.join("etc")).expect("source should exist");
    fs::write(source.join("etc/os-release"), "NAME=cang\n").expect("source file");
    let commands = FakeBtrfsRootfsCommands::new();

    snapshot_mounted_rootfs(&source, &destination, &commands).expect("snapshot should work");

    assert_eq!(
        fs::read_to_string(destination.join("etc/os-release")).expect("snapshot content"),
        "NAME=cang\n"
    );
    assert_eq!(
        commands.calls(),
        vec![BtrfsCall::Snapshot {
            source,
            destination,
        }]
    );
}
