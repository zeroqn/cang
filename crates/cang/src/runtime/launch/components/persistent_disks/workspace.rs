//! The workspace disk: `/workspace` as a btrfs filesystem on a block device.
//!
//! `--workspace-disk` replaces the host-directory virtio-fs share with a btrfs
//! tree the guest mounts itself: sources, `target/` and the cargo home all live
//! inside it, so what a build writes never crosses FUSE. The image is created
//! on first use at the requested size, validated on reuse, and a path that holds
//! anything other than btrfs is refused rather than reformatted.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use super::raw_btrfs::{
    RawDiskSpec, RawImageCommandRunner, extend_file, inspect_existing_regular_file,
};
use crate::runtime::launch::config::DiskAttachment;

pub(crate) const ID: &str = crate::runtime::launch::config::WORKSPACE_DISK_ID;
pub(crate) const LABEL: &str = "CANG_WORKSPACE";
/// The guest contract: which disk to mount where, and which cargo home to bind.
pub(crate) const ID_ENV: &str = "CANG_WORKSPACE_DISK_ID";
pub(crate) const LABEL_ENV: &str = "CANG_WORKSPACE_DISK_LABEL";
/// The default creation size, large enough for a debug and a release `target/`
/// beside an unpacked registry, and grown by the same paths a resize uses.
pub(crate) const DEFAULT_SIZE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub(crate) const DEFAULT_SIZE_HINT: &str = "64G";
/// A `RawDiskSpec` for the diagnostics only: the workspace disk has no state-root
/// file name (its path is user-supplied), and its size comes from the flag.
const SPEC: RawDiskSpec = RawDiskSpec {
    file_name: "",
    id: ID,
    label: LABEL,
    size_bytes: DEFAULT_SIZE_BYTES,
    size_hint: DEFAULT_SIZE_HINT,
    diagnostic_name: "cang workspace disk",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceDiskStatus {
    Created,
    Reused,
}

/// How much of the image must be free before auto-grow stops: the tree grows once
/// the free space the guest recorded is under a tenth of the image.
///
/// The guest writes that number into the tree's own bookkeeping
/// (`/workspace/.cang/free-bytes`) right after it mounts the disk, and the host
/// reads it back through one maintenance VM (`maintenance::workspace_grow`) at the
/// next launch or `cang workspace serve`.
pub(crate) const AUTO_GROW_FREE_PERCENT: u64 = 10;
/// What a grow step adds beyond twice the space in use, so one grow covers a
/// build that is about to double its output rather than a grow per compile.
pub(crate) const AUTO_GROW_STEP_SPARE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The headroom rule: the host must have twice the bytes the step would add.
pub(crate) const AUTO_GROW_HOST_HEADROOM_FACTOR: u64 = 2;

/// What the auto-grow decided about one existing image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoGrowDecision {
    /// Enough free space, or nothing worth growing to: leave the image alone.
    Leave,
    /// Grow the host file to this many bytes; the guest expands the btrfs filesystem
    /// to the new device size when it mounts the disk.
    Grow { target_size_bytes: u64 },
    /// The step is wanted but the host cannot pay for it: report and leave the image
    /// alone, because a host disk filled by an image is a worse answer than a full
    /// workspace.
    Refuse {
        target_size_bytes: u64,
        needed_bytes: u64,
        host_free_bytes: u64,
    },
}

/// The auto-grow rule, as a function of what the guest saw and what the host has.
///
/// `free_bytes` is the number the guest wrote into its tree; `size_bytes` is the
/// image's current length, which is the denominator a user's "10 % free" means.
pub(crate) fn plan_auto_grow(
    size_bytes: u64,
    free_bytes: u64,
    host_free_bytes: u64,
) -> AutoGrowDecision {
    if free_bytes.saturating_mul(100) >= size_bytes.saturating_mul(AUTO_GROW_FREE_PERCENT) {
        return AutoGrowDecision::Leave;
    }
    let used_bytes = size_bytes.saturating_sub(free_bytes);
    // Twice what is in use, plus a fixed spare, so the next grow is not one compile
    // away. Always above the current size: `used > 0.9 * size` under the rule above.
    let target_size_bytes = used_bytes
        .saturating_mul(2)
        .saturating_add(AUTO_GROW_STEP_SPARE_BYTES);
    if target_size_bytes <= size_bytes {
        return AutoGrowDecision::Leave;
    }
    let needed_bytes = target_size_bytes - size_bytes;
    let required_host_bytes = needed_bytes.saturating_mul(AUTO_GROW_HOST_HEADROOM_FACTOR);
    if host_free_bytes < required_host_bytes {
        return AutoGrowDecision::Refuse {
            target_size_bytes,
            needed_bytes,
            host_free_bytes,
        };
    }
    AutoGrowDecision::Grow { target_size_bytes }
}

/// Grow an existing workspace image to at least `target_size_bytes`, accepting one
/// that is already there.
///
/// Only the host half runs here: the sparse file is extended, and the guest expands
/// the btrfs filesystem to the new device size when it next mounts the disk. A
/// missing image is refused - the launch creates images, a resize never does.
pub(crate) fn grow_existing_at_least_with_runner(
    path: &Path,
    target_size_bytes: u64,
    runner: &impl RawImageCommandRunner,
) -> Result<WorkspaceDisk> {
    let Some(current_len) = inspect_existing_regular_file(path, &SPEC)? else {
        bail!(
            "no cang workspace disk at '{}': a launch creates the image, and `cang disk resize --workspace-disk` only grows one that exists - launch cang with that --workspace-disk path first",
            path.display()
        );
    };
    if target_size_bytes < current_len {
        bail!(
            "requested workspace disk size {} bytes must not be smaller than its current size {} bytes for '{}'; cang workspace images only grow",
            target_size_bytes,
            current_len,
            path.display()
        );
    }
    // The workspace's own validation, not the state-root disks': there is no
    // creation-size floor to enforce here.
    validate_existing(path, current_len, None, runner)?;
    if target_size_bytes > current_len {
        extend_file(path, target_size_bytes, &SPEC)?;
    }
    Ok(WorkspaceDisk {
        path: path.to_path_buf(),
        size_bytes: target_size_bytes.max(current_len),
        status: WorkspaceDiskStatus::Reused,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceDisk {
    pub(crate) path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) status: WorkspaceDiskStatus,
}

/// Create the workspace image at `path` when it is absent, or validate the one
/// already there.
///
/// `requested_size_bytes` is the *creation* size, and only a lower bound on reuse
/// when the caller asked for one explicitly: an image first created at 32 GiB
/// stays usable without having to pass the flag again every launch.
pub(crate) fn prepare_with_runner(
    path: &Path,
    requested_size_bytes: Option<u64>,
    runner: &impl RawImageCommandRunner,
) -> Result<WorkspaceDisk> {
    if let Some(0) = requested_size_bytes {
        bail!("cang workspace disk size must be greater than zero");
    }
    let size_bytes = requested_size_bytes.unwrap_or(DEFAULT_SIZE_BYTES);

    if let Some(existing_len) = inspect_existing_regular_file(path, &SPEC)? {
        return validate_existing(path, existing_len, requested_size_bytes, runner);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    let file = File::create_new(path)
        .with_context(|| format!("failed to create cang workspace disk '{}'", path.display()))?;
    file.set_len(size_bytes).with_context(|| {
        format!(
            "failed to set cang workspace disk '{}' to {} bytes",
            path.display(),
            size_bytes
        )
    })?;
    drop(file);

    if let Err(err) = runner.mkfs_btrfs(path, LABEL, &SPEC) {
        let _ = fs::remove_file(path);
        return Err(err).with_context(|| {
            format!(
                "failed to format new cang workspace disk '{}' as btrfs",
                path.display()
            )
        });
    }

    Ok(WorkspaceDisk {
        path: path.to_path_buf(),
        size_bytes,
        status: WorkspaceDiskStatus::Created,
    })
}

/// Validate an image that must already exist, for a command that must never create
/// or reformat one (`cang nix gc --workspace-disk`).
pub(crate) fn validate_existing_with_runner(
    path: &Path,
    runner: &impl RawImageCommandRunner,
) -> Result<WorkspaceDisk> {
    let Some(existing_len) = inspect_existing_regular_file(path, &SPEC)? else {
        bail!(
            "no cang workspace disk at '{}': --workspace-disk names an image a launch created, and cang nix gc does not create one",
            path.display()
        );
    };
    validate_existing(path, existing_len, None, runner)
}

fn validate_existing(
    path: &Path,
    existing_len: u64,
    requested_size_bytes: Option<u64>,
    runner: &impl RawImageCommandRunner,
) -> Result<WorkspaceDisk> {
    if requested_size_bytes.is_some_and(|requested| existing_len < requested) {
        bail!(
            "existing cang workspace disk '{}' is {} bytes, below the requested {} bytes; \
             grow it with 'truncate -s {} {}' or pass a smaller --workspace-size",
            path.display(),
            existing_len,
            requested_size_bytes.unwrap_or(existing_len),
            size_hint_for(requested_size_bytes.unwrap_or(existing_len)),
            path.display()
        );
    }
    let fs_type = runner.probe_fs_type(path, &SPEC)?;
    if fs_type != "btrfs" {
        bail!(
            "existing cang workspace disk '{}' has filesystem type '{}', expected btrfs; \
             refusing to reformat it - point --workspace-disk at another file or delete it",
            path.display(),
            if fs_type.is_empty() {
                "unknown"
            } else {
                &fs_type
            }
        );
    }
    Ok(WorkspaceDisk {
        path: path.to_path_buf(),
        size_bytes: existing_len,
        status: WorkspaceDiskStatus::Reused,
    })
}

pub(crate) fn attachment(disk: &WorkspaceDisk) -> DiskAttachment {
    DiskAttachment {
        id: ID.to_owned(),
        path: disk.path.clone(),
        read_only: false,
    }
}

pub(crate) fn env_pairs(_disk: &WorkspaceDisk) -> [(String, String); 2] {
    [
        (ID_ENV.to_owned(), ID.to_owned()),
        (LABEL_ENV.to_owned(), LABEL.to_owned()),
    ]
}

fn size_hint_for(size_bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if size_bytes.is_multiple_of(GIB) {
        format!("{}G", size_bytes / GIB)
    } else {
        format!("{size_bytes}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::launch::components::persistent_disks::raw_btrfs::test_support::FakeRunner;
    use std::fs;

    #[test]
    fn creates_a_new_image_and_formats_it_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        let runner = FakeRunner::default();

        let disk =
            prepare_with_runner(&path, Some(1024), &runner).expect("image should be created");

        assert_eq!(disk.status, WorkspaceDiskStatus::Created);
        assert_eq!(disk.size_bytes, 1024);
        assert_eq!(fs::metadata(&path).expect("metadata").len(), 1024);
        assert_eq!(runner.mkfs_call_count(), 1);
        let calls = runner.mkfs_calls.borrow();
        assert_eq!(calls[0].1, LABEL);
        assert_eq!(calls[0].2, SPEC.diagnostic_name);
    }

    #[test]
    fn reuses_an_existing_btrfs_image_without_formatting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        fs::write(&path, vec![0u8; 4096]).expect("image");
        let runner = FakeRunner::with_probe("btrfs");

        let disk = prepare_with_runner(&path, Some(1024), &runner).expect("image should be reused");

        assert_eq!(disk.status, WorkspaceDiskStatus::Reused);
        assert_eq!(disk.size_bytes, 4096);
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn an_existing_image_below_the_default_is_usable_without_the_size_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        fs::write(&path, vec![0u8; 4096]).expect("image");
        let runner = FakeRunner::with_probe("btrfs");

        let disk = prepare_with_runner(&path, None, &runner).expect("image should be reused");

        assert_eq!(disk.status, WorkspaceDiskStatus::Reused);
        assert_eq!(disk.size_bytes, 4096);
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn refuses_an_existing_image_that_is_not_btrfs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        fs::write(&path, vec![0u8; 4096]).expect("image");
        let runner = FakeRunner::with_probe("ext4");

        let err =
            prepare_with_runner(&path, Some(1024), &runner).expect_err("ext4 must be refused");

        let message = format!("{err:#}");
        assert!(message.contains("expected btrfs"), "{message}");
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn refuses_an_image_smaller_than_the_requested_size() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        fs::write(&path, vec![0u8; 1024]).expect("image");
        let runner = FakeRunner::with_probe("btrfs");

        let err = prepare_with_runner(&path, Some(4096), &runner).expect_err("too small");

        assert!(format!("{err:#}").contains("below the requested"));
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn refuses_a_directory_and_a_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("work.raw");
        fs::create_dir(&dir).expect("directory");
        let err =
            prepare_with_runner(&dir, Some(1024), &FakeRunner::default()).expect_err("directory");
        assert!(format!("{err:#}").contains("not a regular file"));

        let target = temp.path().join("real.raw");
        fs::write(&target, vec![0u8; 4096]).expect("target");
        let link = temp.path().join("link.raw");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let err =
            prepare_with_runner(&link, Some(1024), &FakeRunner::default()).expect_err("symlink");
        assert!(format!("{err:#}").contains("symlink"));
    }

    #[test]
    fn a_failed_format_removes_the_new_image() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        let runner = FakeRunner::default();
        runner.push_mkfs_error("mkfs.btrfs is not installed");

        let err = prepare_with_runner(&path, Some(1024), &runner).expect_err("format should fail");

        assert!(format!("{err:#}").contains("mkfs.btrfs is not installed"));
        assert!(!path.exists());
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    fn sized_image(dir: &Path, name: &str, len: u64) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, vec![0u8; 4096]).expect("image");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("image open");
        file.set_len(len).expect("image size");
        path
    }

    #[test]
    fn grow_extends_an_existing_image_without_reformatting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = sized_image(temp.path(), "work.raw", 8 * GIB);
        let runner = FakeRunner::with_probe("btrfs");

        let disk = grow_existing_at_least_with_runner(&path, 32 * GIB, &runner)
            .expect("grow should happen");

        assert_eq!(disk.size_bytes, 32 * GIB);
        assert_eq!(disk.status, WorkspaceDiskStatus::Reused);
        assert_eq!(fs::metadata(&path).expect("metadata").len(), 32 * GIB);
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn grow_accepts_an_image_already_at_the_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = sized_image(temp.path(), "work.raw", 32 * GIB);
        let runner = FakeRunner::with_probe("btrfs");

        let disk = grow_existing_at_least_with_runner(&path, 32 * GIB, &runner)
            .expect("an already-reached size is a retry, not an error");

        assert_eq!(disk.size_bytes, 32 * GIB);
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn grow_refuses_a_smaller_target_and_a_missing_image() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = sized_image(temp.path(), "work.raw", 8 * GIB);
        let runner = FakeRunner::with_probe("btrfs");

        let err = grow_existing_at_least_with_runner(&path, 4 * GIB, &runner)
            .expect_err("shrink must fail");
        assert!(format!("{err:#}").contains("only grow"), "{err:#}");

        let missing = temp.path().join("absent.raw");
        let err = grow_existing_at_least_with_runner(&missing, 8 * GIB, &runner)
            .expect_err("a missing image must not be created");
        assert!(
            format!("{err:#}").contains("no cang workspace disk at"),
            "{err:#}"
        );
        assert!(!missing.exists(), "a resize never creates the image");
    }

    #[test]
    fn grow_refuses_an_image_that_is_not_btrfs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = sized_image(temp.path(), "work.raw", 8 * GIB);
        let runner = FakeRunner::with_probe("ext4");

        let err = grow_existing_at_least_with_runner(&path, 16 * GIB, &runner)
            .expect_err("ext4 must be refused");

        assert!(format!("{err:#}").contains("expected btrfs"), "{err:#}");
        assert_eq!(fs::metadata(&path).expect("metadata").len(), 8 * GIB);
    }

    /// Under a tenth free, the step is twice what is in use plus a fixed spare, so
    /// the next grow is not one compile away.
    #[test]
    fn the_rule_grows_to_twice_the_used_space_plus_a_spare() {
        let size = 64 * GIB;
        let free = 2 * GIB; // 3.1 % free
        let used = size - free;

        assert_eq!(
            plan_auto_grow(size, free, u64::MAX),
            AutoGrowDecision::Grow {
                target_size_bytes: used * 2 + AUTO_GROW_STEP_SPARE_BYTES
            }
        );
    }

    #[test]
    fn the_rule_leaves_an_image_with_a_tenth_or_more_free() {
        // A size divisible by ten, so "exactly a tenth" is exact in u64 bytes
        // (64 GiB / 10 truncates to a value just under a tenth, which the rule
        // correctly reads as `Grow`).
        let size = 60 * GIB;

        assert_eq!(
            plan_auto_grow(size, size / 10, u64::MAX),
            AutoGrowDecision::Leave,
            "exactly a tenth free is enough"
        );
        assert_eq!(
            plan_auto_grow(size, 0, u64::MAX),
            AutoGrowDecision::Grow {
                target_size_bytes: size * 2 + AUTO_GROW_STEP_SPARE_BYTES
            }
        );
        assert_eq!(plan_auto_grow(0, 0, u64::MAX), AutoGrowDecision::Leave);
    }

    /// The host pays twice the step or the image does not grow: filling the host's
    /// own disk to rescue a workspace is not a trade cang makes silently.
    #[test]
    fn the_rule_refuses_when_the_host_cannot_pay_twice_the_step() {
        let size = 64 * GIB;
        let free = GIB;
        let target = (size - free) * 2 + AUTO_GROW_STEP_SPARE_BYTES;
        let needed = target - size;

        assert_eq!(
            plan_auto_grow(size, free, needed * 2 - 1),
            AutoGrowDecision::Refuse {
                target_size_bytes: target,
                needed_bytes: needed,
                host_free_bytes: needed * 2 - 1,
            }
        );
        assert_eq!(
            plan_auto_grow(size, free, needed * 2),
            AutoGrowDecision::Grow {
                target_size_bytes: target
            }
        );
    }

    #[test]
    fn env_and_attachment_name_the_disk() {
        let disk = WorkspaceDisk {
            path: PathBuf::from("/tmp/work.raw"),
            size_bytes: 1024,
            status: WorkspaceDiskStatus::Created,
        };
        let attachment = attachment(&disk);
        assert_eq!(attachment.id, ID);
        assert_eq!(attachment.path, disk.path);
        assert!(!attachment.read_only);
        let env = env_pairs(&disk);
        assert_eq!(env[0], (ID_ENV.to_owned(), ID.to_owned()));
        assert_eq!(env[1], (LABEL_ENV.to_owned(), LABEL.to_owned()));
    }
}
