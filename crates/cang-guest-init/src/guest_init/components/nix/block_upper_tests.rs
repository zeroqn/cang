use anyhow::bail;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::guest_init::components::nix::block_upper::{
    CloneKind, IMAGE_DB_DIR, PREVIOUS_DB_DIR, SEEDED_SENTINEL, SeedOutcome, seed_block_upper_with,
};

/// One workspace generation on the build disk:
/// `<root>/<generation>/{upper,work}`, with the read-only lower beside it. The
/// generation directory is the btrfs subvolume; `upper` and `work` are plain
/// directories inside it, which is the only shape overlayfs can compose.
struct Harness {
    _tempdir: tempfile::TempDir,
    root: PathBuf,
    generation: PathBuf,
    upper: PathBuf,
    work: PathBuf,
    lower: PathBuf,
    ops: FakeSeedOps,
}

impl Harness {
    fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let root = tempdir.path().join("nix-overlay");
        let generation = root.join("sha256-current");
        let upper = generation.join("upper");
        let work = generation.join("work");
        let lower = tempdir.path().join("nix-lower");
        fs::create_dir_all(lower.join("var/nix/db")).expect("lower db dir should be created");
        fs::write(lower.join("var/nix/db/db.sqlite"), "image-db")
            .expect("lower db should be written");
        Self {
            _tempdir: tempdir,
            root,
            generation,
            upper,
            work,
            lower,
            ops: FakeSeedOps::new(),
        }
    }

    /// A sibling generation whose upper holds one cached store path and the DB of
    /// the image it was last used with.
    fn sibling(&self, name: &str, modified: u64, completed: bool) -> PathBuf {
        self.make_sibling(name, modified, completed, true)
    }

    /// The same, on a filesystem that does not offer subvolumes: only `cp -a` can
    /// clone it.
    fn plain_sibling(&self, name: &str, modified: u64, completed: bool) -> PathBuf {
        self.make_sibling(name, modified, completed, false)
    }

    fn make_sibling(&self, name: &str, modified: u64, completed: bool, subvolume: bool) -> PathBuf {
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
        // Staging and parking directories of the *sibling's* seed, and the junk
        // overlayfs leaves in a work dir it used: none of it may survive into this
        // generation.
        fs::create_dir_all(upper.join(IMAGE_DB_DIR)).expect("sibling staged db dir");
        fs::write(
            upper.join(IMAGE_DB_DIR).join("db.sqlite"),
            format!("sibling-staged-db-{name}"),
        )
        .expect("sibling staged db");
        fs::create_dir_all(upper.join(PREVIOUS_DB_DIR)).expect("sibling parked db dir");
        fs::write(
            upper.join(PREVIOUS_DB_DIR).join("db.sqlite"),
            format!("sibling-parked-db-{name}"),
        )
        .expect("sibling parked db");
        fs::create_dir_all(generation.join("work/work")).expect("sibling work dir");
        fs::write(generation.join("work/work/junk"), "junk").expect("sibling work junk");
        if completed {
            fs::write(upper.join(SEEDED_SENTINEL), "seeded\n").expect("sibling sentinel");
        }
        if subvolume {
            self.ops.mark_subvolume(&generation);
        }
        self.ops.set_modified(&upper, modified);
        generation
    }

    fn seed(&self) -> anyhow::Result<SeedOutcome> {
        self.seed_with_import(None)
    }

    fn seed_with_import(&self, import: Option<&Path>) -> anyhow::Result<SeedOutcome> {
        seed_block_upper_with(&self.upper, &self.work, &self.lower, import, &self.ops)
    }

    /// A host-seeded upper from before `/nix` moved onto the disk: a store with one
    /// object in it, and the DB of the image it was last used with.
    fn host_upper(&self) -> PathBuf {
        let path = self._tempdir.path().join("share/nix-overlay/upper");
        fs::create_dir_all(path.join("store/abc-cached")).expect("host upper store");
        fs::write(
            path.join("store/abc-cached/marker"),
            "was on the host share\n",
        )
        .expect("host upper marker");
        fs::create_dir_all(path.join("var/nix/db")).expect("host upper db dir");
        fs::write(path.join("var/nix/db/db.sqlite"), "host-db").expect("host upper db");
        fs::write(path.join(SEEDED_SENTINEL), "seeded\n").expect("host upper sentinel");
        path
    }

    fn generation_is_subvolume(&self) -> bool {
        self.ops.knows_subvolume(&self.generation)
    }

    fn is_subvolume(&self, path: &Path) -> bool {
        self.ops.knows_subvolume(path)
    }

    fn operations(&self) -> Vec<String> {
        self.ops.operations.borrow().clone()
    }

    fn clear_operations(&self) {
        self.ops.operations.borrow_mut().clear();
    }

    /// The sentinel write is the last operation of any seed: it is what declares
    /// the generation usable, so nothing may follow it.
    fn assert_sentinel_written_last(&self) {
        let operations = self.operations();
        let sentinel_write = operations
            .iter()
            .position(|operation| {
                operation == &format!("write:{}", self.upper.join(SEEDED_SENTINEL).display())
            })
            .expect("the sentinel write is recorded");
        assert_eq!(
            sentinel_write,
            operations.len() - 1,
            "the sentinel must be written last: {operations:?}"
        );
    }
}

/// A fresh generation*is* the subvolume and `upper`/`work` are plain directories
/// inside it - the shape overlayfs can compose, unlike a subvolume as the upper
/// itself.
#[test]
fn a_fresh_generation_is_a_subvolume_with_plain_upper_and_work_dirs() {
    let harness = Harness::new();

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Fresh);
    assert!(
        harness.generation_is_subvolume(),
        "the generation directory is what the next generation snapshots"
    );
    assert!(harness.upper.is_dir(), "the upper is created inside it");
    assert!(harness.work.is_dir(), "so is the work dir");
    assert!(
        !harness.is_subvolume(&harness.upper),
        "an upper that is a subvolume is exactly the shape that cannot be composed"
    );
    assert!(!harness.is_subvolume(&harness.work));
    assert_eq!(
        fs::read_dir(&harness.work)
            .expect("the work dir is readable")
            .count(),
        0,
        "overlayfs refuses a workdir that is not empty"
    );
    assert!(!harness.upper.join(PREVIOUS_DB_DIR).exists());
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db"
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
    harness.assert_sentinel_written_last();

    let operations = harness.operations();
    let generation_subvolume = operations
        .iter()
        .position(|operation| operation == &format!("subvolume:{}", harness.generation.display()))
        .expect("the generation is created as a subvolume");
    let work_mkdir = operations
        .iter()
        .position(|operation| operation == &format!("mkdir:{}", harness.work.display()))
        .expect("the work dir is created");
    assert!(
        generation_subvolume < work_mkdir,
        "the generation is created before what goes inside it: {operations:?}"
    );
}

/// A completed sibling's whole generation is snapshotted in constant time; the
/// clone's DB is parked, the image's DB is staged, the inherited work dir is
/// emptied and the sentinel is written last.
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
        harness.generation_is_subvolume(),
        "a snapshot is itself a subvolume"
    );
    assert!(!harness.is_subvolume(&harness.upper));
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
    assert_ne!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "sibling-staged-db-sha256-previous",
        "the sibling's own staged DB does not survive the clone"
    );
    assert_ne!(
        fs::read_to_string(harness.upper.join(PREVIOUS_DB_DIR).join("db.sqlite")).unwrap(),
        "sibling-parked-db-sha256-previous",
        "nor the sibling's parked one"
    );
    assert_eq!(
        fs::read_dir(&harness.work)
            .expect("the work dir is readable")
            .count(),
        0,
        "the snapshot carries the sibling's work dir, and overlayfs needs it empty"
    );

    let operations = harness.operations();
    assert!(
        operations.iter().any(|operation| operation
            == &format!(
                "snapshot:{}:{}",
                sibling.display(),
                harness.generation.display()
            )),
        "the sibling *generation* is snapshotted, not its upper: {operations:?}"
    );
    assert!(
        !operations
            .iter()
            .any(|operation| operation.starts_with("copy:")),
        "a snapshot copies nothing: {operations:?}"
    );
    harness.assert_sentinel_written_last();
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

/// A sibling generation that is not a subvolume cannot be snapshotted; its upper
/// is copied instead, and the generation is still seeded - as a subvolume of its
/// own when the filesystem offers them.
#[test]
fn a_sibling_that_is_not_a_subvolume_is_copied() {
    let harness = Harness::new();
    let sibling = harness.plain_sibling("sha256-plain", 100, true);

    let outcome = harness.seed().expect("the seed should succeed");

    assert_eq!(
        outcome,
        SeedOutcome::Cloned(sibling.clone(), CloneKind::Copy)
    );
    assert!(harness.generation_is_subvolume());
    assert!(!harness.is_subvolume(&harness.upper));
    assert!(harness.upper.join("store/abc-hello").is_dir());
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
    assert_eq!(
        fs::read_dir(&harness.work)
            .expect("the work dir is readable")
            .count(),
        0
    );
    let operations = harness.operations();
    assert!(
        operations.iter().any(|operation| operation
            == &format!(
                "copy:{}:{}",
                sibling.join("upper").display(),
                harness.upper.display()
            )),
        "the sibling's upper is what gets copied: {operations:?}"
    );
    harness.assert_sentinel_written_last();
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
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
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
    fs::create_dir_all(&harness.upper).expect("partial upper");
    fs::create_dir_all(harness.upper.join("store/half-copied")).expect("partial upper");
    fs::create_dir_all(harness.upper.join("var/nix/db")).expect("partial db dir");
    fs::write(harness.upper.join("var/nix/db/db.sqlite"), "partial-db").expect("partial db");
    fs::create_dir_all(harness.work.join("work")).expect("dirty work dir");
    fs::write(harness.work.join("work/junk"), "junk").expect("dirty work content");

    let outcome = harness.seed().expect("the resumed seed should succeed");

    assert_eq!(outcome, SeedOutcome::Resumed);
    let operations = harness.operations();
    assert!(
        !operations
            .iter()
            .any(|operation| operation.starts_with("snapshot:") || operation.starts_with("copy:")),
        "a resumed seed clones nothing: {operations:?}"
    );
    assert!(harness.upper.join("store/half-copied").is_dir());
    assert_eq!(
        fs::read_dir(&harness.work)
            .expect("the work dir is readable")
            .count(),
        0,
        "an interrupted seed's work dir is emptied, not inherited"
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
    harness.assert_sentinel_written_last();
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

/// A clone's inherited sentinel goes before its fallible cleanup: a seed that dies
/// while emptying the snapshot's work dir must be retried on the next boot, not
/// mistaken for a completed one that never installed this image's DB.
#[test]
fn a_clone_that_fails_before_its_db_work_is_retried_not_accepted() {
    let harness = Harness::new();
    harness.sibling("sha256-previous", 100, true);
    harness.ops.fail_remove("work");

    harness
        .seed()
        .expect_err("the interrupted seed should report its failure");
    assert!(
        harness.upper.join("store/abc-hello").is_dir(),
        "the snapshot did arrive before the failure"
    );
    assert!(
        !harness.upper.join(SEEDED_SENTINEL).exists(),
        "the snapshot's inherited sentinel is cleared before the work reset"
    );

    harness.ops.remove_failure.borrow_mut().clear();
    let outcome = harness.seed().expect("the retried seed should succeed");

    assert_eq!(
        outcome,
        SeedOutcome::Resumed,
        "the retry resumes instead of accepting the clone as seeded"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db"
    );
    assert_eq!(
        fs::read_dir(&harness.work)
            .expect("the work dir is readable")
            .count(),
        0
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
}

/// The shape this step used to create - the generation a plain directory and the
/// upper inside it a subvolume - can never be composed: btrfs answers `-EXDEV` to
/// the cross-subvolume rename overlayfs performs on copy-up. Such a generation is
/// deleted and seeded again, and the sibling generation beside it is untouched.
#[test]
fn a_broken_generation_whose_upper_is_a_subvolume_is_replaced() {
    let harness = Harness::new();
    let sibling = harness.sibling("sha256-previous", 100, true);

    // The broken generation: plain directory, `upper` a subvolume, already marked
    // seeded by the buggy code.
    fs::create_dir_all(harness.generation.join("work")).expect("broken work dir");
    fs::create_dir_all(harness.upper.join("store/broken-only")).expect("broken store");
    fs::create_dir_all(harness.upper.join("var/nix/db")).expect("broken db dir");
    fs::write(harness.upper.join("var/nix/db/db.sqlite"), "broken-db").expect("broken db");
    fs::write(harness.upper.join(SEEDED_SENTINEL), "seeded\n").expect("broken sentinel");
    harness.ops.mark_subvolume(&harness.upper);

    let outcome = harness
        .seed()
        .expect("the broken generation should be replaced");

    assert_eq!(
        outcome,
        SeedOutcome::Cloned(sibling.clone(), CloneKind::Snapshot)
    );
    let operations = harness.operations();
    assert!(
        operations
            .iter()
            .any(|operation| operation == &format!("subvolume-delete:{}", harness.upper.display())),
        "the unusable upper is deleted as a subvolume: {operations:?}"
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation == &format!("remove-dir:{}", harness.generation.display())),
        "and its generation directory with it: {operations:?}"
    );
    assert!(
        harness.generation_is_subvolume(),
        "the replacement generation is a subvolume"
    );
    assert!(!harness.is_subvolume(&harness.upper));
    assert!(
        !harness.upper.join("store/broken-only").exists(),
        "nothing of the unusable generation survives"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(PREVIOUS_DB_DIR).join("db.sqlite")).unwrap(),
        "sibling-db-sha256-previous",
        "the replacement is seeded from the sibling, not from the broken tree"
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());

    // The sibling is a source, never a casualty.
    assert!(
        harness.is_subvolume(&sibling),
        "the sibling keeps its subvolume"
    );
    assert!(sibling.join("upper").join(SEEDED_SENTINEL).is_file());
    assert!(sibling.join("upper/store/abc-hello").is_dir());
    assert_eq!(
        fs::read_to_string(sibling.join("upper/var/nix/db/db.sqlite")).unwrap(),
        "sibling-db-sha256-previous"
    );
    assert_eq!(
        fs::read_to_string(sibling.join("upper").join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "sibling-staged-db-sha256-previous",
        "the sibling's own staging directories are not consumed"
    );
}

/// The repair deletes a generation, so it must never be redirected: a generation
/// that is a symlink to another one would make the delete reach a tree the seed
/// was not given, so the shape is refused by name instead.
#[test]
fn a_symlinked_generation_is_refused_instead_of_deleted() {
    let harness = Harness::new();
    let sibling = harness.sibling("sha256-previous", 100, true);
    // This generation's path resolves into the sibling's tree, and its `upper`
    // carries the broken subvolume shape - a delete would take the sibling's
    // upper with it.
    std::os::unix::fs::symlink(&sibling, &harness.generation).expect("generation symlink");
    harness.ops.mark_subvolume(&harness.upper);

    let err = harness
        .seed()
        .expect_err("a symlinked generation must be refused");

    assert!(
        format!("{err:#}").contains("is not a generation this step may replace"),
        "{err:#}"
    );
    assert!(
        harness.is_subvolume(&sibling),
        "the sibling's upper subvolume is not deleted through the symlink"
    );
    assert!(sibling.join("upper").join(SEEDED_SENTINEL).is_file());
    assert!(sibling.join("upper/store/abc-hello").is_dir());
    assert_eq!(
        fs::read_to_string(sibling.join("upper/var/nix/db/db.sqlite")).unwrap(),
        "sibling-db-sha256-previous"
    );
}

/// On a filesystem without subvolumes nothing is snapshotted, but the shape is the
/// same: a plain generation directory with plain `upper` and `work` inside it, and
/// the warning says the next generation will copy.
#[test]
fn no_subvolume_support_falls_back_to_plain_directories() {
    let harness = Harness::new();
    harness.ops.subvolumes.set(false);

    let outcome = harness.seed().expect("the seed should still succeed");

    assert_eq!(outcome, SeedOutcome::Fresh);
    assert!(!harness.generation_is_subvolume());
    assert!(harness.generation.is_dir());
    assert!(harness.upper.is_dir());
    assert!(harness.work.is_dir());
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
    let operations = harness.operations();
    assert!(
        operations
            .iter()
            .any(|operation| operation == &format!("subvolume:{}", harness.generation.display())),
        "the subvolume is attempted first: {operations:?}"
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation == &format!("mkdir:{}", harness.generation.display())),
        "and the plain directory is the fallback: {operations:?}"
    );
}

/// An empty generation directory left by an attempt that stopped before writing
/// anything is replaced, so the generation can still become the subvolume the next
/// one snapshots.
#[test]
fn an_empty_leftover_generation_directory_becomes_a_subvolume() {
    let harness = Harness::new();
    // An attempt that stopped before anything went into the generation.
    fs::create_dir_all(&harness.generation).expect("leftover generation dir");

    harness.seed().expect("the seed should succeed");

    assert!(harness.generation_is_subvolume());
    assert!(harness.upper.is_dir());
    assert!(harness.work.is_dir());
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

/// The work dir is created as part of the generation, so one named outside the
/// generation's own directory is a contract mismatch, not something to guess at.
#[test]
fn a_work_dir_outside_the_generation_is_refused() {
    let harness = Harness::new();
    let elsewhere = harness.root.join("somewhere-else");

    let err = seed_block_upper_with(
        &harness.upper,
        &elsewhere,
        &harness.lower,
        None,
        &harness.ops,
    )
    .expect_err("a work dir outside the generation must be refused");

    assert!(
        format!("{err:#}").contains("is not inside the generation directory"),
        "{err:#}"
    );
    assert!(
        !elsewhere.exists(),
        "nothing is created for a refused contract"
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
    subvolume_paths: RefCell<Vec<PathBuf>>,
    snapshot_fails: Cell<bool>,
    mounted: Cell<bool>,
    copy_failure: RefCell<Vec<String>>,
    remove_failure: RefCell<Vec<String>>,
    mtimes: RefCell<Vec<(PathBuf, u64)>>,
}

impl FakeSeedOps {
    fn new() -> Self {
        Self {
            inner: super::SystemSeedOps,
            operations: RefCell::new(Vec::new()),
            subvolumes: Cell::new(true),
            subvolume_paths: RefCell::new(Vec::new()),
            snapshot_fails: Cell::new(false),
            mounted: Cell::new(true),
            copy_failure: RefCell::new(Vec::new()),
            remove_failure: RefCell::new(Vec::new()),
            mtimes: RefCell::new(Vec::new()),
        }
    }

    fn record(&self, operation: impl Into<String>) {
        self.operations.borrow_mut().push(operation.into());
    }

    /// Declare a path a subvolume without recording an operation, so a fixture can
    /// be built without polluting the seed's own operation list.
    fn mark_subvolume(&self, path: &Path) {
        self.subvolume_paths.borrow_mut().push(path.to_path_buf());
    }

    fn knows_subvolume(&self, path: &Path) -> bool {
        self.subvolumes.get()
            && self
                .subvolume_paths
                .borrow()
                .iter()
                .any(|candidate| candidate == path)
    }

    fn clear_subvolume(&self, path: &Path) {
        self.subvolume_paths
            .borrow_mut()
            .retain(|candidate| candidate != path);
    }

    fn set_modified(&self, path: &Path, seconds: u64) {
        self.mtimes.borrow_mut().push((path.to_path_buf(), seconds));
    }

    fn fail_copy(&self, name: &str) {
        self.copy_failure.borrow_mut().push(name.to_owned());
    }

    fn fail_remove(&self, name: &str) {
        self.remove_failure.borrow_mut().push(name.to_owned());
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
        if !self.subvolumes.get() {
            return Ok(false);
        }
        if self.inner.is_dir(path) {
            bail!("'{}' already exists", path.display());
        }
        self.inner.create_dir_all(path)?;
        self.mark_subvolume(path);
        Ok(true)
    }

    fn is_subvolume(&self, path: &Path) -> bool {
        self.knows_subvolume(path)
    }

    fn remove_subvolume(&self, path: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("subvolume-delete:{}", path.display()));
        if !self.knows_subvolume(path) {
            bail!("'{}' is not a subvolume", path.display());
        }
        self.inner.remove_dir_all(path)?;
        self.clear_subvolume(path);
        Ok(())
    }

    fn snapshot(&self, from: &Path, to: &Path) -> Result<(), anyhow::Error> {
        self.record(format!("snapshot:{}:{}", from.display(), to.display()));
        if self.snapshot_fails.get() {
            bail!("btrfs snapshot failed");
        }
        if !self.knows_subvolume(from) {
            bail!("'{}' is not a subvolume", from.display());
        }
        // `btrfs subvolume snapshot` creates its target and fails if it exists, so
        // the fake must not tolerate one: that is the mistake it has to catch.
        if self.inner.is_dir(to) {
            bail!("'{}' already exists", to.display());
        }
        self.inner.create_dir_all(to)?;
        self.inner.copy_tree(from, to)?;
        self.mark_subvolume(to);
        Ok(())
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

    fn is_plain_dir(&self, path: &Path) -> bool {
        self.inner.is_plain_dir(path)
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
        if self
            .remove_failure
            .borrow()
            .iter()
            .any(|name| path.file_name().and_then(|part| part.to_str()) == Some(name.as_str()))
        {
            bail!("removal of '{}' failed", path.display());
        }
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
    assert!(harness.generation_is_subvolume());
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
        super::newest_sibling_generation(&own, &harness.ops).expect("the scan should succeed"),
        None,
        "the only generation on the disk is this one"
    );

    let sibling = harness.sibling("sha256-other", 100, true);
    assert_eq!(
        super::newest_sibling_generation(&own, &harness.ops).expect("the scan should succeed"),
        Some(sibling),
        "a sibling is a source, the generation itself never is"
    );

    // Staging and retired trees the host keeps beside a generation are skipped by
    // name, so a partial seed cannot become the next generation's cache.
    let staging = harness.root.join(".nix-overlay.new");
    fs::create_dir_all(staging.join("upper/store/staged")).expect("staging tree");
    harness.ops.set_modified(&staging.join("upper"), 999);
    assert_eq!(
        super::newest_sibling_generation(&own, &harness.ops).expect("the scan should succeed"),
        Some(harness.root.join("sha256-other"))
    );
}

/// A workspace that ran with the host-seeded upper and only now moves `/nix` onto
/// the disk keeps its cache: the fresh generation imports the host's upper once,
/// and the object it held is there afterwards.
#[test]
fn a_fresh_generation_imports_the_host_seeded_upper_once() {
    let harness = Harness::new();
    let host = harness.host_upper();

    let outcome = harness
        .seed_with_import(Some(&host))
        .expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Imported(host.clone()));
    assert!(
        harness.upper.join("store/abc-cached/marker").is_file(),
        "the cache the host held is imported"
    );
    // The imported upper is the *previous* image's, so its DB is parked for the
    // merge and the image's own DB is staged - the same contract as a clone.
    assert_eq!(
        fs::read_to_string(harness.upper.join(PREVIOUS_DB_DIR).join("db.sqlite")).unwrap(),
        "host-db"
    );
    assert_eq!(
        fs::read_to_string(harness.upper.join(IMAGE_DB_DIR).join("db.sqlite")).unwrap(),
        "image-db"
    );
    assert!(harness.upper.join(SEEDED_SENTINEL).is_file());
    harness.assert_sentinel_written_last();

    // Only once: the generation is now seeded, so a second boot imports nothing and
    // the host copy is not consulted at all.
    harness.clear_operations();
    let again = harness
        .seed_with_import(Some(&host))
        .expect("the second seed should succeed");
    assert_eq!(again, SeedOutcome::AlreadySeeded);
    assert_eq!(
        harness.operations(),
        vec!["mounted".to_owned()],
        "a seeded generation is only probed for its mount"
    );
}

/// A sibling generation on the disk beats the host copy: it is a snapshot instead
/// of a full copy over virtio-fs.
#[test]
fn a_sibling_generation_is_preferred_over_the_host_upper() {
    let harness = Harness::new();
    harness.sibling("sha256-older", 1_000, true);
    let host = harness.host_upper();

    let outcome = harness
        .seed_with_import(Some(&host))
        .expect("the seed should succeed");

    assert!(
        matches!(outcome, SeedOutcome::Cloned(_, _)),
        "a sibling wins: {outcome:?}"
    );
    assert!(
        !harness.upper.join("store/abc-cached").exists(),
        "nothing is imported when the disk already has a cache"
    );
}

/// An upper the host seeded for an image that never ran carries no store objects,
/// so it is not worth a full copy.
#[test]
fn an_empty_host_upper_is_not_imported() {
    let harness = Harness::new();
    let host = harness._tempdir.path().join("share/nix-overlay/upper");
    fs::create_dir_all(host.join("store")).expect("an empty store dir");
    fs::write(host.join(SEEDED_SENTINEL), "seeded\n").expect("host sentinel");

    let outcome = harness
        .seed_with_import(Some(&host))
        .expect("the seed should succeed");

    assert_eq!(outcome, SeedOutcome::Fresh);
}

/// The disk is shared with the workspace's build caches, so a seed keeps only what
/// it can use: the generation that just booted, and the newest other completed one
/// (the clone source). Everything older goes, and an unfinished generation is left
/// for the boot that resumes it.
#[test]
fn a_seed_prunes_retired_generations_and_keeps_the_clone_source() {
    let harness = Harness::new();
    let oldest = harness.sibling("sha256-oldest", 1_000, true);
    let newest = harness.sibling("sha256-newest", 3_000, true);
    let unfinished = harness.sibling("sha256-unfinished", 2_000, false);

    harness.seed().expect("the seed should succeed");

    assert!(
        !oldest.exists(),
        "the oldest completed generation is dead weight"
    );
    assert!(
        newest.is_dir(),
        "the newest completed one is the clone source"
    );
    assert!(
        unfinished.is_dir(),
        "an unfinished generation is resumable, not garbage"
    );
    assert!(
        harness.generation.is_dir(),
        "never the generation that booted"
    );
}

/// Pruning never reaches outside the generations directory, and never deletes a
/// generation by following a symlink or a dot-name.
#[test]
fn a_seed_prunes_nothing_that_is_not_a_generation() {
    let harness = Harness::new();
    let retired = harness.sibling("sha256-retired", 1_000, true);
    // One newer completed generation, so `retired` is surplus rather than the
    // clone source the seed keeps.
    let _newest = harness.sibling("sha256-newer", 2_000, true);
    let staging = harness.sibling(".staging", 500, true);
    let empty = harness.root.join("sha256-never-seeded");
    fs::create_dir_all(&empty).expect("an empty generation directory");

    harness.seed().expect("the seed should succeed");

    assert!(
        !retired.exists(),
        "a completed generation nobody can clone from goes"
    );
    assert!(staging.is_dir(), "a dot-name is not a generation");
    assert!(
        empty.is_dir(),
        "a generation with no sentinel is not pruned"
    );
}

/// The import copies the cache - the store and the DB - and not the rest of a
/// `/nix` root that was live under the host overlay. A socket in
/// `var/nix/daemon-socket` is what made the first `cp -a` of the whole upper abort
/// with `ENXIO`, and it must never be part of the copy.
#[test]
fn an_import_copies_the_store_and_db_only() {
    let harness = Harness::new();
    let host = harness.host_upper();
    // A live socket and its parent directory, exactly as the host overlay leaves
    // them, plus a work-in-progress directory that is not a cache.
    fs::create_dir_all(host.join("var/nix/daemon-socket")).expect("socket dir");
    fs::write(
        host.join("var/nix/daemon-socket/socket"),
        "not a socket here",
    )
    .expect("socket file");
    fs::create_dir_all(host.join("var/nix/temproots")).expect("temproots");

    harness
        .seed_with_import(Some(&host))
        .expect("the seed should succeed");

    let copies: Vec<String> = harness
        .operations()
        .into_iter()
        .filter(|operation| operation.starts_with("copy:"))
        .collect();
    assert_eq!(
        copies.len(),
        2,
        "only the store and the DB are imported: {copies:?}"
    );
    assert!(
        copies[0].starts_with(&format!("copy:{}", host.join("store").display())),
        "{copies:?}"
    );
    assert!(
        copies[1].starts_with(&format!("copy:{}", host.join("var/nix/db").display())),
        "{copies:?}"
    );
    assert!(
        !harness.upper.join("var/nix/daemon-socket").exists(),
        "a live socket is never copied"
    );
}
