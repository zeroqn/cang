use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{GcOps, Generation, collect, run_with_ops};

/// A build disk on the host side of the fake: real directories under a tempdir,
/// with only the mount, the disk lookup and `du`/`btrfs` faked.
struct Harness {
    _tempdir: tempfile::TempDir,
    mount: PathBuf,
    ops: FakeGcOps,
}

impl Harness {
    fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let mount = tempdir.path().join("cang-build");
        fs::create_dir_all(mount.join("nix-overlay")).expect("generations dir");
        Self {
            _tempdir: tempdir,
            mount,
            ops: FakeGcOps::new(),
        }
    }

    /// One generation: `<mount>/nix-overlay/<name>/{upper,work}`, with a store entry
    /// so its size is not zero, and a sentinel unless `completed` is false.
    fn generation(&self, name: &str, completed: bool, modified_secs: u64) -> PathBuf {
        let path = self.mount.join("nix-overlay").join(name);
        fs::create_dir_all(path.join("upper/store/abc-path")).expect("upper store");
        fs::write(path.join("upper/store/abc-path/marker"), "x").expect("marker");
        fs::create_dir_all(path.join("work")).expect("work dir");
        if completed {
            fs::write(path.join("upper/.cang-upper-seeded"), "seeded\n").expect("sentinel");
        }
        self.ops.set_modified(&path, modified_secs);
        path
    }

    fn collect(&self, all: bool, dry_run: bool) -> anyhow::Result<()> {
        collect(&self.mount, all, dry_run, &self.ops)
    }

    fn run(&self, all: bool, dry_run: bool) -> anyhow::Result<()> {
        run_with_ops(
            all,
            dry_run,
            &self.mount,
            "cang-build",
            "cang-build-disk",
            &self.ops,
        )
    }

    fn removed(&self) -> Vec<String> {
        self.ops.removed.borrow().clone()
    }
}

fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct FakeGcOps {
    removed: RefCell<Vec<String>>,
    mounted: RefCell<bool>,
    mtimes: RefCell<Vec<(PathBuf, u64)>>,
}

impl FakeGcOps {
    fn new() -> Self {
        Self {
            removed: RefCell::new(Vec::new()),
            mounted: RefCell::new(false),
            mtimes: RefCell::new(Vec::new()),
        }
    }

    fn set_modified(&self, path: &Path, secs: u64) {
        self.mtimes.borrow_mut().push((path.to_path_buf(), secs));
    }
}

impl GcOps for FakeGcOps {
    fn find_disk(&self, _label: &str, _id: &str) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from("/dev/vdb"))
    }

    fn create_dir_all(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_mounted(&self, _path: &Path) -> anyhow::Result<bool> {
        Ok(*self.mounted.borrow())
    }

    fn mount_btrfs(&self, _disk: &Path, _mount: &Path) -> anyhow::Result<()> {
        *self.mounted.borrow_mut() = true;
        Ok(())
    }

    fn unmount(&self, _mount: &Path) -> anyhow::Result<()> {
        *self.mounted.borrow_mut() = false;
        Ok(())
    }

    fn generations(&self, mount: &Path) -> anyhow::Result<Vec<Generation>> {
        let root = mount.join("nix-overlay");
        let mut generations = Vec::new();
        for entry in fs::read_dir(&root).expect("generations dir is readable") {
            let entry = entry.expect("entry");
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !is_plain_dir(&path) {
                continue;
            }
            let modified = self
                .mtimes
                .borrow()
                .iter()
                .rev()
                .find(|(candidate, _)| candidate == &path)
                .map(|(_, secs)| SystemTime::UNIX_EPOCH + Duration::from_secs(*secs))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            generations.push(Generation {
                completed: path.join("upper/.cang-upper-seeded").is_file(),
                bytes: 42,
                modified,
                name,
                path,
            });
        }
        generations.sort_by(|left, right| {
            right
                .completed
                .cmp(&left.completed)
                .then_with(|| right.modified.cmp(&left.modified))
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(generations)
    }

    fn remove(&self, path: &Path) -> anyhow::Result<()> {
        self.removed.borrow_mut().push(path.display().to_string());
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }
}

/// The cache the workspace used last stays; every other completed generation, and
/// anything unfinished, goes.
#[test]
fn gc_keeps_the_generation_used_last_and_collects_the_rest() {
    let harness = Harness::new();
    let newest = harness.generation("sha256-newest", true, 3_000);
    let older = harness.generation("sha256-older", true, 1_000);
    let unfinished = harness.generation("sha256-interrupted", false, 2_000);
    let staging = harness.mount.join("nix-overlay/.staging");
    fs::create_dir_all(&staging).expect("staging dir");

    harness.collect(false, false).expect("gc should succeed");

    assert!(newest.is_dir(), "the generation used last is the cache");
    assert!(
        !older.exists(),
        "an older completed generation is collected"
    );
    assert!(
        !unfinished.exists(),
        "an interrupted generation is collected"
    );
    assert!(staging.is_dir(), "a dot-name is not a generation");
    assert_eq!(harness.removed().len(), 2);
}

/// `--all` drops the cache too, so the next launch starts from an empty upper.
#[test]
fn gc_all_collects_every_generation() {
    let harness = Harness::new();
    let newest = harness.generation("sha256-newest", true, 3_000);
    let older = harness.generation("sha256-older", true, 1_000);

    harness.collect(true, false).expect("gc should succeed");

    assert!(!newest.exists());
    assert!(!older.exists());
    assert_eq!(harness.removed().len(), 2);
}

/// A dry run reports and deletes nothing.
#[test]
fn a_dry_run_collects_nothing() {
    let harness = Harness::new();
    let newest = harness.generation("sha256-newest", true, 3_000);
    let older = harness.generation("sha256-older", true, 1_000);

    harness
        .collect(false, true)
        .expect("the dry run should succeed");

    assert!(newest.is_dir());
    assert!(older.is_dir());
    assert!(harness.removed().is_empty(), "{:?}", harness.removed());
}

/// An empty disk is not an error: a workspace that never launched with a block
/// upper has nothing to collect.
#[test]
fn an_empty_build_disk_collects_nothing() {
    let harness = Harness::new();

    harness.collect(false, false).expect("gc should succeed");

    assert!(harness.removed().is_empty());
}

/// The subcommand mounts the disk it collects from, and unmounts it again.
#[test]
fn the_command_mounts_the_build_disk_and_unmounts_it() {
    let harness = Harness::new();
    harness.generation("sha256-older", true, 1_000);

    harness.run(true, false).expect("gc should succeed");

    assert!(
        !*harness.ops.mounted.borrow(),
        "the disk is unmounted before the maintenance VM exits"
    );
    assert_eq!(harness.removed().len(), 1);
}

/// Nothing outside the generations directory is ever deleted, and a generation is
/// never reached through a symlink.
#[test]
fn gc_never_deletes_through_a_symlink() {
    let harness = Harness::new();
    let outside = harness._tempdir.path().join("outside");
    fs::create_dir_all(&outside).expect("outside dir");
    fs::write(outside.join("keep"), "keep").expect("outside file");
    let link = harness.mount.join("nix-overlay/sha256-link");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");

    harness.collect(true, false).expect("gc should succeed");

    assert!(
        outside.join("keep").is_file(),
        "a symlinked tree is untouched"
    );
    assert!(harness.removed().is_empty(), "{:?}", harness.removed());
}
