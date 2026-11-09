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
//! What one seed does:
//!
//! 1. `upper` is created as a btrfs subvolume when the filesystem offers them, so
//!    the next generation clones the whole cache in constant time and zero bytes;
//! 2. a fresh generation is cloned from the newest *completed* sibling generation
//!    under the same parent (`btrfs subvolume snapshot`, else `cp -a`), or started
//!    empty when there is none;
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

use anyhow::{Context, Result, anyhow, bail};
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
    /// A fresh generation was cloned from a sibling generation's upper.
    Cloned(PathBuf, CloneKind),
}

/// How a sibling generation's upper was cloned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) enum CloneKind {
    /// `btrfs subvolume snapshot`: constant time, no data copied, extents shared.
    Snapshot,
    /// `cp -a`: the sibling's upper is not a subvolume to snapshot.
    Copy,
}

/// Seed this generation's upper from a sibling generation and the image's lower.
pub(in crate::guest_init) fn seed_block_upper(upper: &Path, lower: &Path) -> Result<SeedOutcome> {
    seed_block_upper_with(upper, lower, &SystemSeedOps)
}

fn seed_block_upper_with(upper: &Path, lower: &Path, ops: &impl SeedOps) -> Result<SeedOutcome> {
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

    // The generation directory is only created when something is going into it:
    // the second boot of a seeded generation must touch nothing at all.
    let outcome = match plan_seed(upper, ops)? {
        SeedPlan::AlreadySeeded => return Ok(SeedOutcome::AlreadySeeded),
        SeedPlan::Resumed => SeedOutcome::Resumed,
        SeedPlan::Fresh => {
            ops.create_dir_all(generation)?;
            create_empty_upper(upper, ops)?;
            SeedOutcome::Fresh
        }
        SeedPlan::Cloned(sibling) => {
            ops.create_dir_all(generation)?;
            let kind = clone_upper(upper, &sibling, ops)?;
            SeedOutcome::Cloned(sibling, kind)
        }
    };

    // A clone carries the sibling's own completion sentinel and whatever it had
    // staged or parked. All of it describes the *sibling's* image generation, so
    // it goes before this generation declares its own DB state - otherwise a seed
    // interrupted after the clone would look completed on the next boot and never
    // install this image's DB.
    remove_dir_if_present(&upper.join(IMAGE_DB_DIR), ops)?;
    remove_dir_if_present(&upper.join(PREVIOUS_DB_DIR), ops)?;
    ops.remove_file(&upper.join(SEEDED_SENTINEL))?;
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
    Ok(match newest_sibling_upper(upper, ops)? {
        Some(sibling) => SeedPlan::Cloned(sibling),
        None => SeedPlan::Fresh,
    })
}

/// The sibling generation whose upper this one should be cloned from.
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
fn newest_sibling_upper(upper: &Path, ops: &impl SeedOps) -> Result<Option<PathBuf>> {
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
        let candidate = entry.join("upper");
        if !ops.is_dir(&candidate) || ops.read_dir(&candidate)?.is_empty() {
            continue;
        }
        let sibling = Sibling {
            completed: ops.is_file(&candidate.join(SEEDED_SENTINEL)),
            modified: ops.modified(&candidate).unwrap_or(SystemTime::UNIX_EPOCH),
            name: name.to_owned(),
            upper: candidate,
        };
        if best.as_ref().is_none_or(|best| sibling.beats(best)) {
            best = Some(sibling);
        }
    }
    Ok(best.map(|sibling| sibling.upper))
}

struct Sibling {
    completed: bool,
    modified: SystemTime,
    name: String,
    upper: PathBuf,
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

/// Create an empty upper, as a subvolume when the filesystem offers them: the next
/// generation is then a snapshot of it instead of a per-file copy.
fn create_empty_upper(upper: &Path, ops: &impl SeedOps) -> Result<()> {
    if ops.is_dir(upper) {
        return Ok(());
    }
    if ops.create_subvolume(upper)? {
        return Ok(());
    }
    eprintln!(
        "cang-guest-init: warning: '{}' could not be made a btrfs subvolume; the next image generation will copy the /nix upper instead of snapshotting it",
        upper.display()
    );
    ops.create_dir_all(upper)
}

fn clone_upper(upper: &Path, sibling: &Path, ops: &impl SeedOps) -> Result<CloneKind> {
    // A snapshot creates its target, so it can only be used while the target is
    // absent; an upper directory left by an interrupted seed is filled by a copy.
    if !ops.is_dir(upper) && ops.is_subvolume(sibling) {
        match ops.snapshot(sibling, upper) {
            Ok(()) => return Ok(CloneKind::Snapshot),
            Err(err) => eprintln!(
                "cang-guest-init: warning: btrfs snapshot of '{}' into '{}' failed, copying the tree instead: {err:#}",
                sibling.display(),
                upper.display()
            ),
        }
    }
    ops.create_dir_all(upper)?;
    ops.copy_tree(sibling, upper).with_context(|| {
        format!(
            "failed to clone the /nix upper '{}' into '{}'",
            sibling.display(),
            upper.display()
        )
    })?;
    Ok(CloneKind::Copy)
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
    fn snapshot(&self, from: &Path, to: &Path) -> Result<()>;
    fn copy_tree(&self, from: &Path, to: &Path) -> Result<()>;
    fn copy_file(&self, from: &Path, to: &Path) -> Result<()>;
    fn is_dir(&self, path: &Path) -> bool;
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
