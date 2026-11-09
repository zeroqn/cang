//! The guest-seeded `/nix` upper on the build disk.
//!
//! A workspace `/nix` overlays one image generation's store with a writable upper
//! that outlives that image, and the objects the upper accumulates are what stop
//! a substituted closure from being fetched again on the next boot. When the
//! upper lives on the host's state root the host seeds it
//! (`runtime/session/nix_overlay.rs`): it clones the previous generation, stages
//! the new image's Nix DB at `.cang-image-db` and parks the cloned one at
//! `.cang-previous-db`. A raw image on the block disk cannot be mounted rootless
//! on the host, so for that placement the same seeding happens here, as root,
//! before the overlay is composed - the host's contract is unchanged: the two
//! directories arrive in `CANG_NIX_LOWER_UPPERDIR`/`CANG_NIX_LOWER_WORKDIR` and a
//! generation's first boot finds them empty.
//!
//! One generation is one directory per image digest, and *that directory* is the
//! btrfs subvolume - the same shape the host's own seeding uses:
//!
//! ```text
//! <build-disk>/nix-overlay/<generation>/          <- btrfs subvolume, what clones
//! <build-disk>/nix-overlay/<generation>/upper     <- plain dir, the overlayfs upperdir
//! <build-disk>/nix-overlay/<generation>/work      <- plain dir, the overlayfs workdir
//! ```
//!
//! Neither of the two must itself be a subvolume: btrfs answers `-EXDEV` to the
//! cross-subvolume rename overlayfs performs during copy-up, so every write
//! through the composed overlay failed with `Invalid cross-device link` when the
//! subvolume was the upper. A plain directory inside a subvolume is exactly what
//! overlayfs wants, and the subvolume one level up is still what makes the next
//! generation a constant-time clone.
//!
//! What one seed does:
//!
//! 1. the generation directory is created as a btrfs subvolume when the
//!    filesystem offers them, so the next generation clones the whole cache in
//!    constant time and zero bytes; `upper` and `work` are made as plain
//!    directories inside it;
//! 2. a fresh generation is cloned from the newest *completed* sibling generation
//!    under the same parent (`btrfs subvolume snapshot`, else `cp -a` of its
//!    `upper`), or started empty when there is none;
//! 3. whatever DB the clone carried is parked at `.cang-previous-db` and the
//!    image's own DB - which is inside the read-only lower, because that lower is
//!    the image's `/nix` - is staged at `.cang-image-db`, exactly where the
//!    post-mount `repair_store()` step already looks (`install_staged_image_db`
//!    installs it, `store_reconcile_args` merges the parked rows with
//!    `--purge-only`);
//! 4. `.cang-upper-seeded` is written last, so an interrupted seed is retried
//!    rather than mistaken for a completed one.
//!
//! A generation that is already seeded is left alone: the second boot of the same
//! generation clones nothing, copies nothing and writes nothing.
//!
//! One generation is repaired rather than served: the shape an earlier build of
//! this step created - the generation a plain directory and `upper` inside it the
//! subvolume - can never be composed, so [`replace_unusable_generation`] deletes
//! that generation's own tree and seeds it again on this boot. No other
//! generation is reachable from there.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::guest_init::components::disk::build;
use crate::guest_init::fs;

/// Written at the upper's root once the generation is seeded. Its presence in the
/// binary is also the host's capability probe for this step, so the name is a
/// contract (`GUEST_UPPER_MARKER` in `runtime/launch/components/persistent_disks/nix_lower.rs`).
pub(in crate::guest_init) const SEEDED_SENTINEL: &str = ".cang-upper-seeded";
/// Where the image generation's own Nix DB is staged for the post-mount install.
/// The host stages the same name into a host-seeded upper, and the guest-side
/// consumer (`install_staged_image_db`) is shared by both.
pub(in crate::guest_init) const IMAGE_DB_DIR: &str = ".cang-image-db";
/// Where the cloned generation's DB is parked for the post-mount merge.
pub(in crate::guest_init) const PREVIOUS_DB_DIR: &str = ".cang-previous-db";
/// The overlay's writable layer, relative to a generation directory.
const UPPER_DIR: &str = "upper";
/// A Nix DB directory, relative to a `/nix` root.
const LIVE_DB_RELATIVE: &str = "var/nix/db";
/// The files a Nix DB is made of: the database, the write-ahead log SQLite may
/// have left beside it, and the schema the database was created with.
const DB_FILES: [&str; 4] = ["db.sqlite", "db.sqlite-wal", "db.sqlite-shm", "schema"];

/// Whether this upper is on the block disk rather than the host's share.
///
/// The host names the path, not the placement, so the placement is read off the
/// path: everything under the build disk's mount point is a filesystem the guest
/// has to prepare itself, and everything else is a directory the host already
/// seeded. That is what decides both whether the guest seeds and whether a
/// failure is fatal ([`crate::guest_init::components::nix::root`]).
pub(in crate::guest_init) fn is_on_build_disk(upperdir: &Path) -> bool {
    upperdir.starts_with(Path::new(build::MOUNT_POINT))
}

/// What one seed did, for the profile log and the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum SeedOutcome {
    /// The generation was already seeded: nothing was created, copied or written.
    AlreadySeeded,
    /// An interrupted seed's upper was finished instead of cloned again.
    Resumed,
    /// A fresh generation was started empty.
    Fresh,
    /// A fresh generation was cloned from a sibling generation.
    Cloned(PathBuf, CloneKind),
}

/// How a sibling generation was cloned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) enum CloneKind {
    /// `btrfs subvolume snapshot`: constant time, no data copied, extents shared.
    Snapshot,
    /// `cp -a`: the sibling generation is not a subvolume to snapshot.
    Copy,
}

/// Seed this generation from a sibling generation and the image's lower.
///
/// `upper` and `work` are the two overlayfs directories the host named; the
/// generation directory is their parent.
pub(in crate::guest_init) fn seed_block_upper(
    upper: &Path,
    work: &Path,
    lower: &Path,
) -> Result<SeedOutcome> {
    seed_block_upper_with(upper, work, lower, &SystemSeedOps)
}

fn seed_block_upper_with(
    upper: &Path,
    work: &Path,
    lower: &Path,
    ops: &impl SeedOps,
) -> Result<SeedOutcome> {
    // Seeding is only ever useful on the disk it is meant for: on an unmounted
    // build-disk path the upper would be a task-rootfs directory that looks like a
    // cache and loses every object at exit, so a missing mount is a failure and
    // not a quiet fallback.
    if !ops.is_mounted(Path::new(build::MOUNT_POINT))? {
        bail!(
            "the /nix upper '{}' is on the build disk but '{}' is not mounted",
            upper.display(),
            build::MOUNT_POINT
        );
    }
    let generation = upper.parent().ok_or_else(|| {
        anyhow!(
            "the /nix upper '{}' has no generation directory to seed",
            upper.display()
        )
    })?;
    // The two overlayfs directories are siblings inside the generation, which is
    // created as one unit: a work dir anywhere else would never be created, and
    // the mount would fail on a missing directory instead of saying why.
    if work.parent() != Some(generation) {
        bail!(
            "the /nix work dir '{}' is not inside the generation directory '{}' of its upper '{}'",
            work.display(),
            generation.display(),
            upper.display()
        );
    }
    replace_unusable_generation(generation, upper, ops)?;

    // The generation directory is only created when something is going into it:
    // the second boot of a seeded generation must touch nothing at all.
    let outcome = match plan_seed(upper, ops)? {
        SeedPlan::AlreadySeeded => return Ok(SeedOutcome::AlreadySeeded),
        SeedPlan::Resumed => {
            // An interrupted seed keeps its partial upper; both overlayfs
            // directories still have to be there for the mount to follow, and the
            // work dir starts empty.
            ops.create_dir_all(upper)?;
            reset_work_dir(work, ops)?;
            SeedOutcome::Resumed
        }
        SeedPlan::Fresh => {
            create_generation(generation, ops)?;
            ops.create_dir_all(upper)?;
            reset_work_dir(work, ops)?;
            SeedOutcome::Fresh
        }
        SeedPlan::Cloned(sibling) => {
            let kind = clone_generation(generation, upper, work, &sibling, ops)?;
            SeedOutcome::Cloned(sibling, kind)
        }
    };

    // Nothing the sibling left behind may describe this generation: its sentinel
    // is what would make an interrupted seed look completed on the next boot and
    // never install this image's DB, and its staging directories are its own.
    clear_inherited_state(upper, ops)?;
    park_previous_db(upper, ops)?;
    stage_image_db(upper, lower, ops)?;
    ops.write_file(&upper.join(SEEDED_SENTINEL), "seeded\n")
        .with_context(|| {
            format!(
                "failed to write the /nix upper sentinel '{}'",
                upper.join(SEEDED_SENTINEL).display()
            )
        })?;
    Ok(outcome)
}

/// What the upper holds, and therefore what the seed has to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SeedPlan {
    /// A completed seed: nothing to do at all.
    AlreadySeeded,
    /// Content without a sentinel - an interrupted seed. Its DB work is finished
    /// but nothing is cloned again, so a partial copy is kept rather than
    /// restarted, and the merge in `repair_store()` is what makes it consistent.
    Resumed,
    /// Empty and a sibling generation can be cloned.
    Cloned(PathBuf),
    /// Empty with nothing to clone from.
    Fresh,
}

fn plan_seed(upper: &Path, ops: &impl SeedOps) -> Result<SeedPlan> {
    if ops.is_file(&upper.join(SEEDED_SENTINEL)) {
        return Ok(SeedPlan::AlreadySeeded);
    }
    if ops.is_dir(upper) && !ops.read_dir(upper)?.is_empty() {
        return Ok(SeedPlan::Resumed);
    }
    Ok(match newest_sibling_generation(upper, ops)? {
        Some(sibling) => SeedPlan::Cloned(sibling),
        None => SeedPlan::Fresh,
    })
}

/// The sibling generation this generation should be cloned from.
///
/// Siblings are the other generation directories under the upper's parent's
/// parent - `<build-disk>/nix-overlay/<generation>/upper` - and the choice is
/// deterministic:
///
/// 1. a sibling with a completion sentinel beats one without, so a half-seeded
///    generation is never preferred over a finished one;
/// 2. then the newest modification time of its `upper`, which is the generation
///    the workspace used last;
/// 3. then the lexicographically smallest directory name, so a tie between two
///    generations is resolved the same way on every boot.
fn newest_sibling_generation(upper: &Path, ops: &impl SeedOps) -> Result<Option<PathBuf>> {
    let generation = upper
        .parent()
        .ok_or_else(|| anyhow!("the /nix upper '{}' has no generation", upper.display()))?;
    let Some(root) = generation.parent() else {
        return Ok(None);
    };
    if !ops.is_dir(root) {
        return Ok(None);
    }
    let mut best: Option<Sibling> = None;
    for entry in ops.read_dir(root)? {
        let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // This generation is never its own source. The comparison is by name
        // because the host builds the path by formatting, and a caller that passed
        // it unnormalised (`.../gen/./upper`) must still not clone itself.
        if entry == generation
            || generation.file_name().and_then(|name| name.to_str()) == Some(name)
        {
            continue;
        }
        // Staging and retired trees sit beside a generation and are not one.
        if name.starts_with('.') {
            continue;
        }
        // "Has content" is still a question about the sibling's upper: a
        // generation that never received a store object is not a cache.
        let candidate = entry.join(UPPER_DIR);
        if !ops.is_dir(&candidate) || ops.read_dir(&candidate)?.is_empty() {
            continue;
        }
        let sibling = Sibling {
            completed: ops.is_file(&candidate.join(SEEDED_SENTINEL)),
            modified: ops.modified(&candidate).unwrap_or(SystemTime::UNIX_EPOCH),
            name: name.to_owned(),
            generation: entry,
        };
        if best.as_ref().is_none_or(|best| sibling.beats(best)) {
            best = Some(sibling);
        }
    }
    Ok(best.map(|sibling| sibling.generation))
}

struct Sibling {
    completed: bool,
    modified: SystemTime,
    name: String,
    generation: PathBuf,
}

impl Sibling {
    fn beats(&self, other: &Self) -> bool {
        if self.completed != other.completed {
            return self.completed;
        }
        if self.modified != other.modified {
            return self.modified > other.modified;
        }
        self.name < other.name
    }
}

/// Delete a generation this step's earlier shape created and cannot use.
///
/// An upper that is itself a btrfs subvolume is the shape that broke every write
/// through the composed overlay: btrfs refuses the cross-subvolume rename
/// overlayfs performs on copy-up with `-EXDEV`, so the generation could never be
/// composed. It is replaced rather than migrated, and the replacement is seeded
/// from a sibling like any other fresh generation (so the cached store objects
/// come back in constant time when a sibling exists).
///
/// Only this generation's own tree is deleted: it is the upper's parent, no name
/// is resolved from outside it, and a sibling generation is never reachable from
/// here.
fn replace_unusable_generation(generation: &Path, upper: &Path, ops: &impl SeedOps) -> Result<()> {
    if !ops.is_dir(generation) || ops.is_subvolume(generation) || !ops.is_subvolume(upper) {
        return Ok(());
    }
    if generation.parent().is_none() || generation.file_name().is_none() {
        bail!(
            "the /nix upper '{}' is a btrfs subvolume inside its generation directory, which overlayfs cannot compose, and '{}' is the filesystem root so it cannot be replaced",
            upper.display(),
            generation.display()
        );
    }
    // Deleting is the one step here that must not be redirected: a generation
    // (or an upper) that is a symlink could resolve into *another* generation's
    // tree, so the repair only ever touches two real directories, one of them
    // named `upper`, and names the shape instead of guessing.
    if !ops.is_plain_dir(generation)
        || !ops.is_plain_dir(upper)
        || upper.file_name() != Some(OsStr::new(UPPER_DIR))
    {
        bail!(
            "the /nix upper '{}' is a btrfs subvolume inside its generation directory, which overlayfs cannot compose, and '{}' is not a generation this step may replace (a symlink, or an upper not named '{}')",
            upper.display(),
            generation.display(),
            UPPER_DIR
        );
    }
    eprintln!(
        "cang-guest-init: the /nix generation '{}' has its upper as a btrfs subvolume, which overlayfs cannot compose; deleting that generation and seeding it again",
        generation.display()
    );
    ops.remove_subvolume(upper).with_context(|| {
        format!(
            "failed to delete the unusable /nix upper subvolume '{}'",
            upper.display()
        )
    })?;
    ops.remove_dir_all(generation).with_context(|| {
        format!(
            "failed to remove the unusable /nix generation '{}'",
            generation.display()
        )
    })?;
    Ok(())
}

/// Create the generation directory, as a btrfs subvolume when the filesystem
/// offers them: the next generation is then a snapshot of it instead of a
/// per-file copy.
fn create_generation(generation: &Path, ops: &impl SeedOps) -> Result<()> {
    if ops.is_dir(generation) {
        // An empty shell - an earlier attempt that stopped before anything was
        // written into it - is replaced so the generation can still be a
        // subvolume. A generation with content is never touched here.
        if ops.is_subvolume(generation) || !ops.read_dir(generation)?.is_empty() {
            return Ok(());
        }
        ops.remove_dir_all(generation)?;
    }
    if let Some(parent) = generation.parent() {
        ops.create_dir_all(parent)?;
    }
    if ops.create_subvolume(generation)? {
        return Ok(());
    }
    eprintln!(
        "cang-guest-init: warning: '{}' could not be made a btrfs subvolume; the next image generation will copy the /nix upper instead of snapshotting it",
        generation.display()
    );
    ops.create_dir_all(generation)
}

/// Clone a sibling generation's `upper` into this generation's.
fn clone_generation(
    generation: &Path,
    upper: &Path,
    work: &Path,
    sibling: &Path,
    ops: &impl SeedOps,
) -> Result<CloneKind> {
    match clone_into(generation, upper, sibling, ops) {
        Ok(kind) => {
            // The clone carries the sibling's sentinel and staging directories,
            // which describe the *sibling's* image generation. They go before the
            // work reset and before the DB work below: a seed that stops after
            // either must be retried, not accepted as completed.
            clear_inherited_state(upper, ops)?;
            // A snapshot carries the sibling's `work` directory and whatever
            // overlayfs left in it, and overlayfs refuses a workdir that is not
            // empty: this generation starts with an empty one.
            reset_work_dir(work, ops)?;
            Ok(kind)
        }
        Err(err) => {
            // A copy that failed halfway may already have brought the sibling's
            // sentinel along; the next boot must retry rather than accept it.
            if let Err(cleanup) = clear_inherited_state(upper, ops) {
                eprintln!(
                    "cang-guest-init: warning: failed to clear the half-done /nix upper '{}': {cleanup:#}",
                    upper.display()
                );
            }
            Err(err)
        }
    }
}

/// Snapshot the sibling generation, or copy its `upper` when it is not a
/// subvolume to snapshot.
fn clone_into(
    generation: &Path,
    upper: &Path,
    sibling: &Path,
    ops: &impl SeedOps,
) -> Result<CloneKind> {
    // A snapshot creates its target, so it can only be used while the target is
    // absent; a generation directory left by an interrupted seed is filled by a
    // copy of the sibling's upper instead.
    let kind = if !ops.is_dir(generation) && ops.is_subvolume(sibling) {
        match ops.snapshot(sibling, generation) {
            Ok(()) => CloneKind::Snapshot,
            Err(err) => {
                eprintln!(
                    "cang-guest-init: warning: btrfs snapshot of '{}' into '{}' failed, copying the tree instead: {err:#}",
                    sibling.display(),
                    generation.display()
                );
                copy_sibling_upper(generation, upper, sibling, ops)?;
                CloneKind::Copy
            }
        }
    } else {
        copy_sibling_upper(generation, upper, sibling, ops)?;
        CloneKind::Copy
    };
    ops.create_dir_all(upper)?;
    Ok(kind)
}

fn copy_sibling_upper(
    generation: &Path,
    upper: &Path,
    sibling: &Path,
    ops: &impl SeedOps,
) -> Result<()> {
    create_generation(generation, ops)?;
    ops.create_dir_all(upper)?;
    let source = sibling.join(UPPER_DIR);
    ops.copy_tree(&source, upper).with_context(|| {
        format!(
            "failed to clone the /nix upper '{}' into '{}'",
            source.display(),
            upper.display()
        )
    })?;
    Ok(())
}

/// Empty the work directory, creating it. Both overlayfs directories must exist
/// before the mount, and the work dir must carry nothing.
fn reset_work_dir(work: &Path, ops: &impl SeedOps) -> Result<()> {
    if ops.is_dir(work) {
        ops.remove_dir_all(work)?;
    }
    ops.create_dir_all(work)
}

/// Park the cloned generation's DB where `repair_store()` merges it from.
///
/// A copy rather than a move, like the host does: the live `var/nix/db` is
/// replaced by the post-mount install anyway, and a copy keeps a retried seed
/// idempotent.
fn park_previous_db(upper: &Path, ops: &impl SeedOps) -> Result<()> {
    let live = upper.join(LIVE_DB_RELATIVE);
    if !ops.is_dir(&live) {
        return Ok(());
    }
    let parked = upper.join(PREVIOUS_DB_DIR);
    ops.create_dir_all(&parked)?;
    for name in DB_FILES {
        let from = live.join(name);
        if !ops.is_file(&from) {
            continue;
        }
        ops.copy_file(&from, &parked.join(name)).with_context(|| {
            format!(
                "failed to park the previous /nix DB '{}' into '{}'",
                from.display(),
                parked.display()
            )
        })?;
    }
    Ok(())
}

/// Stage the image generation's DB where the post-mount install picks it up.
///
/// The lower is the image's own `/nix`, so its DB is the image's; nothing has to
/// travel over the share for this.
fn stage_image_db(upper: &Path, lower: &Path, ops: &impl SeedOps) -> Result<()> {
    let source = lower.join(LIVE_DB_RELATIVE);
    if !ops.is_file(&source.join("db.sqlite")) {
        eprintln!(
            "cang-guest-init: warning: the /nix lower '{}' ships no Nix DB, so the upper keeps the one it has",
            source.display()
        );
        return Ok(());
    }
    let staged = upper.join(IMAGE_DB_DIR);
    ops.create_dir_all(&staged)?;
    for name in DB_FILES {
        let from = source.join(name);
        if !ops.is_file(&from) {
            continue;
        }
        ops.copy_file(&from, &staged.join(name)).with_context(|| {
            format!(
                "failed to stage the image Nix DB '{}' into '{}'",
                from.display(),
                staged.display()
            )
        })?;
    }
    Ok(())
}

/// Drop the completion sentinel and the staged/parked DB directories of whatever
/// generation this upper was cloned from, so an interrupted seed is retried
/// instead of mistaken for a finished one.
fn clear_inherited_state(upper: &Path, ops: &impl SeedOps) -> Result<()> {
    remove_dir_if_present(&upper.join(IMAGE_DB_DIR), ops)?;
    remove_dir_if_present(&upper.join(PREVIOUS_DB_DIR), ops)?;
    ops.remove_file(&upper.join(SEEDED_SENTINEL))
}

fn remove_dir_if_present(path: &Path, ops: &impl SeedOps) -> Result<()> {
    if ops.is_dir(path) {
        ops.remove_dir_all(path)?;
    }
    Ok(())
}

/// The filesystem and command seam, so the seeding decisions are testable without
/// btrfs or a mounted build disk.
trait SeedOps {
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    /// `btrfs subvolume create`: `Ok(false)` when the filesystem or the tool does
    /// not offer it, which is a slow next generation rather than a failure.
    fn create_subvolume(&self, path: &Path) -> Result<bool>;
    fn is_subvolume(&self, path: &Path) -> bool;
    /// `btrfs subvolume delete`: the only way to remove a directory that is a
    /// subvolume, and what a broken-shape generation's upper needs.
    fn remove_subvolume(&self, path: &Path) -> Result<()>;
    fn snapshot(&self, from: &Path, to: &Path) -> Result<()>;
    fn copy_tree(&self, from: &Path, to: &Path) -> Result<()>;
    fn copy_file(&self, from: &Path, to: &Path) -> Result<()>;
    fn is_dir(&self, path: &Path) -> bool;
    /// A directory that is really one, not a symlink to one: the only shape this
    /// step may delete.
    fn is_plain_dir(&self, path: &Path) -> bool;
    fn is_file(&self, path: &Path) -> bool;
    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    fn modified(&self, path: &Path) -> Option<SystemTime>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    fn remove_dir_all(&self, path: &Path) -> Result<()>;
    fn write_file(&self, path: &Path, contents: &str) -> Result<()>;
    fn is_mounted(&self, path: &Path) -> Result<bool>;
}

#[derive(Debug, Clone, Copy)]
struct SystemSeedOps;

impl SeedOps for SystemSeedOps {
    fn create_dir_all(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path)
    }

    fn create_subvolume(&self, path: &Path) -> Result<bool> {
        if crate::guest_init::command::find_on_path("btrfs").is_none() {
            return Ok(false);
        }
        crate::guest_init::command::status_ok("btrfs", &["subvolume", "create", path_str(path)?])
    }

    fn is_subvolume(&self, path: &Path) -> bool {
        // `btrfs subvolume show` is the only way to tell a subvolume from an
        // ordinary directory; without the tool nothing can be snapshotted.
        if crate::guest_init::command::find_on_path("btrfs").is_none() {
            return false;
        }
        let Some(path) = path.to_str() else {
            return false;
        };
        crate::guest_init::command::status_ok("btrfs", &["subvolume", "show", path])
            .unwrap_or(false)
    }

    fn remove_subvolume(&self, path: &Path) -> Result<()> {
        crate::guest_init::command::run("btrfs", &["subvolume", "delete", path_str(path)?])
    }

    fn snapshot(&self, from: &Path, to: &Path) -> Result<()> {
        crate::guest_init::command::run(
            "btrfs",
            &["subvolume", "snapshot", path_str(from)?, path_str(to)?],
        )
    }

    fn copy_tree(&self, from: &Path, to: &Path) -> Result<()> {
        // `cp -a <from>/. <to>` merges into the target instead of nesting the
        // source directory inside it, and `-a` keeps hardlinked store objects
        // hardlinked.
        let source = format!("{}/.", from.display());
        crate::guest_init::command::run("cp", &["-a", &source, path_str(to)?])
    }

    fn copy_file(&self, from: &Path, to: &Path) -> Result<()> {
        std::fs::copy(from, to)
            .map(|_| ())
            .with_context(|| format!("failed to copy '{}' to '{}'", from.display(), to.display()))
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn is_plain_dir(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("failed to list '{}'", path.display()))?
        {
            entries.push(entry?.path());
        }
        Ok(entries)
    }

    fn modified(&self, path: &Path) -> Option<SystemTime> {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("failed to remove '{}'", path.display())),
        }
    }

    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("failed to remove '{}'", path.display())),
        }
    }

    fn write_file(&self, path: &Path, contents: &str) -> Result<()> {
        fs::write_file(path, contents, 0o644)
    }

    fn is_mounted(&self, path: &Path) -> Result<bool> {
        crate::guest_init::command::status_ok("findmnt", &["-rn", path_str(path)?])
    }
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
#[path = "block_upper_tests.rs"]
mod tests;
