use anyhow::bail;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::guest_init::components::nix::block_upper::{
    CloneKind, IMAGE_DB_DIR, PREVIOUS_DB_DIR, SEEDED_SENTINEL, SeedOutcome, seed_block_upper_with,
};

/// One workspace generation on the build disk: `<root>/<generation>/upper`, with
/// the read-only lower beside it.
struct Harness {
    _tempdir: tempfile::TempDir,
    root: PathBuf,
    upper: PathBuf,
    lower: PathBuf,
    ops: FakeSeedOps,
}

impl Harness {
    fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let root = tempdir.path().join("nix-overlay");
        let upper = root.join("sha256-current").join("upper");
        let lower = tempdir.path().join("nix-lower");
        fs::create_dir_all(lower.join("var/nix/db")).expect("lower db dir should be created");
        fs::write(lower.join("var/nix/db/db.sqlite"), "image-db")
            .expect("lower db should be written");
        Self {
            _tempdir: tempdir,
            root,
            upper,
            lower,
            ops: FakeSeedOps::new(),
        }
    }

    /// A sibling generation whose upper holds one cached store path and the DB of
    /// the image it was last used with.
    fn sibling(&self, name: &str, modified: u64, completed: bool) -> PathBuf {
        let generation = self.root.join(name);
        let upper = generation.join("upper");
        fs::create_dir_all(upper.join("store/abc-hello")).expect("sibling store should be created");
        fs::create_dir_all(upper.join("var/nix/db")).expect("sibling db dir should be created");
        fs::write(
            upper.join("var/nix/db/db.sqlite"),
            format!("sibling-db-{name}"),
        )
        .expect("sibling db should be written");
        fs::write(upper.join("var/nix/db/schema"), "schema").expect("sibling schema");
        if completed {
            fs::write(upper.join(SEEDED_SENTINEL), "seeded\n").expect("sibling sentinel");
        }
        self.ops.set_modified(&upper, modified);
        upper
    }

    fn seed(&self) -> anyhow::Result<SeedOutcome> {
        seed_block_upper_with(&self.upper, &self.lower, &self.ops)
    }

    fn operations(&self) -> Vec<String> {
        self.ops.operations.borrow().clone()
    }

    fn clear_operations(&self) {
        self.ops.operations.borrow_mut().clear();
    }
}

/// A completed sibling is snapshotted in constant time, its DB is parked, the
/// image's DB is staged and the sentinel is written *last* - the sentinel is what
/// declares the generation usable, so nothing may follow it.
#[test]
fn a_fresh_generation_snapshots_the_completed_sibling_and_seeds_its_db() {
    let harness = Harness::new();
    let sibling = harness.sibling("sha256-previous", 100, true);

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(
        outcome,
        SeedOutcome::Cloned(sibling.clone(), CloneKind::Snapshot)
    );
    assert!(
        harness.upper.join("store/abc-hello").is_dir(),
        "the snapshot carries the cached store paths"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(PREVIOUS_DB_DIR).join("db.sqlite")).unwrap(),
        "sibling-db-sha256-previous",
        "the cloned generation's DB is parked for the merge"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db",
        "the image's own DB, from the read-only lower, is staged for the install"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(SEEDED_SENTINEL)).unwrap(),
        "seeded\n"
    );

    let operations = harness.operations();
    let sentinel_write = operations
        .iter()
        .position(|operation| {
            operation == &format!("write:{}", harness.upper.join(SEEDED_SENTINEL).display())
        })
        .expect("the sentinel write is recorded");
    assert_eq!(
        sentinel_write,
        operations.len() - 1,
        "the sentinel must be written last: {operations:?}"
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation.starts_with("snapshot:")),
        "a subvolume sibling is snapshotted rather than copied: {operations:?}"
    );
}

/// The sibling to clone is the generation the workspace used last: newest
/// modification time among the completed ones.
#[test]
fn the_newest_completed_sibling_is_cloned() {
    let harness = Harness::new();
    harness.sibling("sha256-old", 100, true);
    let newest = harness.sibling("sha256-new", 900, true);
    harness.sibling("sha256-newest-interrupted", 1000, false);

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Cloned(newest, CloneKind::Snapshot));
}

/// An interrupted sibling has no sentinel, so it must lose to any completed one
/// even when it is newer: cloning it would carry a half-seeded cache forward.
#[test]
fn a_completed_sibling_beats_a_newer_interrupted_one() {
    let harness = Harness::new();
    let completed = harness.sibling("sha256-completed", 10, true);
    harness.sibling("sha256-partial", 20, false);

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Cloned(completed, CloneKind::Snapshot));
}

/// Two generated at the same moment is not a tie broken by chance: the smaller
/// directory name wins on every boot.
#[test]
fn equal_modification_times_resolve_to_the_smallest_name() {
    let harness = Harness::new();
    harness.sibling("sha256-bbb", 500, true);
    let smallest = harness.sibling("sha256-aaa", 500, true);

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Cloned(smallest, CloneKind::Snapshot));
}

/// A sibling whose upper is not a subvolume cannot be snapshotted; the tree is
/// copied instead, and the generation is still seeded.
#[test]
fn a_sibling_that_is_not_a_subvolume_is_copied() {
    let harness = Harness::new();
    let sibling = harness.sibling("sha256-plain", 100, true);
    harness.ops.subvolumes.set(false);

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Cloned(sibling, CloneKind::Copy));
    assert!(harness.upper.join("store/abc-hello").is_dir());
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

/// A snapshot that fails falls back to a copy rather than failing the boot: the
/// cache is what is at stake, not the mount.
#[test]
fn a_failed_snapshot_falls_back_to_a_copy() {
    let harness = Harness::new();
    harness.sibling("sha256-previous", 100, true);
    harness.ops.snapshot_fails.set(true);

    let outcome = harness.seed().expect("the seed should succeed");

    assert!(matches!(outcome, SeedOutcome::Cloned(_, CloneKind::Copy)));
    assert!(harness.upper.join("store/abc-hello").is_dir());
}

/// Nothing to clone from: the generation starts empty, and the image's DB is
/// still staged so the first boot registers the image's store.
#[test]
fn a_generation_without_a_sibling_starts_empty_with_the_image_db() {
    let harness = Harness::new();

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Fresh);
    assert!(!harness.upper.join(PREVIOUS_DB_DIR).exists());
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db"
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

/// The second boot of the same generation is a no-op: no directory, no snapshot,
/// no copy and no write - the sentinel is the whole decision.
#[test]
fn the_second_boot_of_a_generation_does_nothing() {
    let harness = Harness::new();
    harness.sibling("sha256-previous", 100, true);
    harness.seed().expect("the first seed should succeed");
    // The post-mount install consumed the staged DB, as it does on a real boot.
    fs::remove_dir_all(harness.upper.join(IMAGE_DB_DIR)).expect("staged db consumed");
    harness.clear_operations();

    let outcome = harness.seed().expect("the second seed should succeed");

    assert_eq!(outcome, SeedOutcome::AlreadySeeded);
    assert_eq!(harness.operations(), vec!["mounted".to_owned()]);
}

/// An interrupted seed has content but no sentinel. Its DB work is finished, but
/// nothing is cloned again - a partial copy stays partial and the post-mount
/// merge makes the DB consistent with whatever arrived.
#[test]
fn an_interrupted_seed_is_resumed_without_cloning_again() {
    let harness = Harness::new();
    harness.sibling("sha256-previous", 100, true);
    fs::create_dir_all(harness.upper.join("store/half-copied")).expect("partial upper");
    fs::create_dir_all(harness.upper.join("var/nix/db")).expect("partial db dir");
    fs::write(harness.upper.join("var/nix/db/db.sqlite"), "partial-db").expect("partial db");

    let outcome = harness.seed().expect("the resumed seed should succeed");

    assert_eq!(outcome, SeedOutcome::Resumed);
    let operations = harness.operations();
    assert!(
        !operations
            .iter()
            .any(|operation| operation.starts_with("snapshot:") || operation.starts_with("copy:")),
        "a resumed seed clones nothing: {operations:?}"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(PREVIOUS_DB_DIR).join("db.sqlite")).unwrap(),
        "partial-db"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db"
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

/// A seed that died after the clone must not look completed on the next boot: the
/// snapshot carries the sibling's sentinel, so it is dropped before the DB work.
#[test]
fn a_seed_interrupted_after_the_clone_is_retried_instead_of_accepted() {
    let harness = Harness::new();
    harness.sibling("sha256-previous", 100, true);
    harness.ops.fail_copy("db.sqlite");

    harness
        .seed()
        .expect_err("the interrupted seed should report its failure");
    assert!(
        harness.upper.join("store/abc-hello").is_dir(),
        "the snapshot did arrive before the failure"
    );
    assert!(
        !harness.upper.join(SEEDED_SENTINEL).exists(),
        "an unfinished seed must not carry a sentinel"
    );

    harness.ops.copy_failure.borrow_mut().clear();
    let outcome = harness.seed().expect("the retried seed should succeed");

    assert_eq!(outcome, SeedOutcome::Resumed);
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db",
        "the retry installs the image's DB, which the interrupted seed never did"
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

/// The upper is on the build disk because that disk is a local filesystem; on an
/// unmounted path it would be a task-rootfs directory that loses every object at
/// exit, so the seed refuses rather than pretending.
#[test]
fn an_unmounted_build_disk_fails_the_seed() {
    let harness = Harness::new();
    harness.ops.mounted.set(false);

    let err = harness
        .seed()
        .expect_err("an unmounted build disk must fail");

    assert!(
        format!("{err:#}").contains("is on the build disk but"),
        "{err:#}"
    );
}

/// A lower that ships no Nix DB (an image without `/nix/var`) is not a failure:
/// the upper keeps whatever DB it has and the warning names it.
#[test]
fn a_lower_without_a_db_is_not_a_failure() {
    let harness = Harness::new();
    fs::remove_dir_all(harness.lower.join("var")).expect("lower var removed");

    let outcome = harness.seed().expect("the seed should still succeed");

    assert_eq!(outcome, SeedOutcome::Fresh);
    assert!(!harness.upper.join(IMAGE_DB_DIR).exists());
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

struct FakeSeedOps {
    inner: super::SystemSeedOps,
    operations: RefCell<Vec<String>>,
    subvolumes: Cell<bool>,
    snapshot_fails: Cell<bool>,
    mounted: Cell<bool>,
    copy_failure: RefCell<Vec<String>>,
    mtimes: RefCell<Vec<(PathBuf, u64)>>,
}

impl FakeSeedOps {
    fn new() -> Self {
        Self {
            inner: super::SystemSeedOps,
            operations: RefCell::new(Vec::new()),
            subvolumes: Cell::new(true),
            snapshot_fails: Cell::new(false),
            mounted: Cell::new(true),
            copy_failure: RefCell::new(Vec::new()),
            mtimes: RefCell::new(Vec::new()),
        }
    }

    fn record(&self, operation: impl Into<String>) {
        self.operations.borrow_mut().push(operation.into());
    }

    fn set_modified(&self, path: &Path, seconds: u64) {
        self.mtimes.borrow_mut().push((path.to_path_buf(), seconds));
    }

    fn fail_copy(&self, name: &str) {
        self.copy_failure.borrow_mut().push(name.to_owned());
    }

    fn must_fail(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        self.copy_failure.borrow().iter().any(|fail| fail == name)
    }
}

impl super::SeedOps for FakeSeedOps {
    fn create_dir_all(&self, path: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("mkdir:{}", path.display()));
        self.inner.create_dir_all(path)
    }

    fn create_subvolume(&self, path: &Path) -> Result<bool, anyhow::Error> {
        self.record(format!("subvolume:{}", path.display()));
        if self.subvolumes.get() {
            self.inner.create_dir_all(path)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn is_subvolume(&self, path: &Path) -> bool {
        self.record(format!("is-subvolume:{}", path.display()));
        self.subvolumes.get() && self.inner.is_dir(path)
    }

    fn snapshot(&self, from: &Path, to: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("snapshot:{}:{}", from.display(), to.display()));
        if self.snapshot_fails.get() {
            bail!("btrfs snapshot failed");
        }
        self.inner.create_dir_all(to)?;
        self.inner.copy_tree(from, to)
    }

    fn copy_tree(&self, from: &Path, to: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("copy:{}:{}", from.display(), to.display()));
        self.inner.copy_tree(from, to)
    }

    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("copy-file:{}:{}", from.display(), to.display()));
        if self.must_fail(from) {
            bail!("copy of '{}' failed", from.display());
        }
        self.inner.copy_file(from, to)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.inner.is_dir(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.inner.is_file(path)
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>, anyhow::Error> {
        self.inner.read_dir(path)
    }

    fn modified(&self, path: &Path) -> Option<SystemTime> {
        self.mtimes
            .borrow()
            .iter()
            .find(|(candidate, _)| candidate == path)
            .map(|(_, seconds)| SystemTime::UNIX_EPOCH + Duration::from_secs(*seconds))
            .or_else(|| self.inner.modified(path))
    }

    fn remove_file(&self, path: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("remove:{}", path.display()));
        self.inner.remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("remove-dir:{}", path.display()));
        self.inner.remove_dir_all(path)
    }

    fn write_file(&self, path: &Path, contents: &str) -> Result<(), anyhow::Error> {
        self.record(format!("write:{}", path.display()));
        self.inner.write_file(path, contents)
    }

    fn is_mounted(&self, _path: &Path) -> Result<bool, anyhow::Error> {
        self.record("mounted".to_owned());
        Ok(self.mounted.get())
    }
}

/// The build disk's `nix-overlay/` parent is not the host's to create: the first
/// generation of a workspace may be the first thing that ever writes there, so the
/// seed makes the whole path.
#[test]
fn a_missing_generation_parent_is_created() {
    let harness = Harness::new();
    assert!(
        !harness.root.exists(),
        "the harness starts without the nix-overlay parent"
    );

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Fresh);
    assert!(
        harness.root.is_dir(),
        "the seed creates <build-disk>/nix-overlay itself"
    );
    assert!(harness.upper.is_dir());
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

/// A generation is never its own clone source: only a sibling is, and a sibling
/// that is a generation at all (not a staging or retired tree) at that.
#[test]
fn the_generation_is_never_its_own_clone_source() {
    let harness = Harness::new();
    let own = harness.upper.clone();
    fs::create_dir_all(own.join("store/own-path")).expect("own upper");
    assert_eq!(
        super::newest_sibling_upper(&own, &harness.ops).expect("the scan should succeed"),
        None,
        "the only generation on the disk is this one"
    );

    let sibling = harness.sibling("sha256-other", 100, true);
    assert_eq!(
        super::newest_sibling_upper(&own, &harness.ops).expect("the scan should succeed"),
        Some(sibling),
        "a sibling is a source, the generation itself never is"
    );

    // Staging and retired trees the host keeps beside a generation are skipped by
    // name, so a partial seed cannot become the next generation's cache.
    let staging = harness.root.join(".nix-overlay.new");
    fs::create_dir_all(staging.join("upper/store/staged")).expect("staging tree");
    harness.ops.set_modified(&staging.join("upper"), 999);
    assert_eq!(
        super::newest_sibling_upper(&own, &harness.ops).expect("the scan should succeed"),
        Some(harness.root.join("sha256-other").join("upper"))
    );
}
