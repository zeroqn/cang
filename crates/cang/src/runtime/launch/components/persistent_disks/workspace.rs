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

use super::raw_btrfs::{RawDiskSpec, RawImageCommandRunner, inspect_existing_regular_file};
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
        if requested_size_bytes.is_some_and(|requested| existing_len < requested) {
            bail!(
                "existing cang workspace disk '{}' is {} bytes, below the requested {} bytes; \
                 grow it with 'truncate -s {} {}' or pass a smaller --workspace-size",
                path.display(),
                existing_len,
                size_bytes,
                size_hint_for(size_bytes),
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
        return Ok(WorkspaceDisk {
            path: path.to_path_buf(),
            size_bytes: existing_len,
            status: WorkspaceDiskStatus::Reused,
        });
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
