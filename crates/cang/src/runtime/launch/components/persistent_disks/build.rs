//! Persistent build disk: the cargo cache and the build output, on virtio-blk.
//!
//! Two of a rust build's working sets are a poor match for the virtio-fs
//! workspace share, and both are measured:
//!
//! - **`target/`** - thousands of small files. Three concurrent builds of a
//!   23-crate workspace at `JOBS=4`: **1152-1164 s with `target/` on the share,
//!   285-288 s on this disk**.
//! - **`~/.cargo`** - the unpacked registry the compiler reads from for every
//!   dependency. cang shares it over virtio-fs (`CARGO_TAG` -> `/home/dev/.cargo`),
//!   so those reads are the `READ` and `LOOKUP` classes of every build's request
//!   histogram; on this disk they are local block reads.
//!
//! One disk carries both, per workspace: `state_root` is the workspace's own state
//! directory, so a workspace keeps its build warm across tasks and no two
//! workspaces ever share a filesystem (btrfs cannot be mounted read-write by two
//! guests, and cang already refuses a second task on one workspace).
//!
//! A btrfs *subvolume* cannot be attached as a block device - virtio-blk needs a
//! file with its own block address space - so this is a raw image with a btrfs
//! filesystem inside, which is what gives the guest an independent mount.

use anyhow::{Context, Result};
use std::path::Path;

use crate::runtime::launch::components::persistent_disks::raw_btrfs::{
    self, RawBtrfsDisk, RawDiskSpec,
};
use crate::runtime::launch::config::DiskAttachment;

pub(super) const FILE_NAME: &str = "cang-build.raw";
pub(super) const ID: &str = "cang-build";
pub(super) const LABEL: &str = "CANG_BUILD";
pub(in crate::runtime::launch) const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";
pub(in crate::runtime::launch) const CARGO_HOME_ENV: &str = "CARGO_HOME";
/// Where the guest mounts the disk; both cargo paths live under it, so a test
/// checks they do.
#[cfg(test)]
const MOUNT_POINT: &str = "/home/dev/.cache/cang-build";
/// The build output directory, inside the mount.
pub(in crate::runtime::launch) const TARGET_DIR: &str = "/home/dev/.cache/cang-build/target";
/// The cargo home (registry, git checkouts), inside the mount.
pub(in crate::runtime::launch) const CARGO_HOME: &str = "/home/dev/.cache/cang-build/cargo";
/// Room for an unpacked registry (about 1.5 GiB), a debug `target/` (11 GiB on
/// the reproducing tree) and a release one beside it.
pub(super) const SIZE_BYTES: u64 = 96 * 1024 * 1024 * 1024;

const SPEC: RawDiskSpec = RawDiskSpec {
    file_name: FILE_NAME,
    id: ID,
    label: LABEL,
    size_bytes: SIZE_BYTES,
    size_hint: "96G",
    diagnostic_name: "cang persistent build disk",
};

pub(super) fn prepare_with_runner(
    state_root: &Path,
    runner: &impl raw_btrfs::RawImageCommandRunner,
) -> Result<RawBtrfsDisk> {
    raw_btrfs::prepare_with_runner(state_root, &SPEC, runner)
        .context("failed to prepare cang persistent build disk")
}

pub(super) fn attachment(disk: &RawBtrfsDisk) -> DiskAttachment {
    DiskAttachment {
        id: disk.id.clone(),
        path: disk.path.clone(),
        read_only: false,
    }
}

/// The guest env that routes both cargo working sets onto the disk, plus the
/// label and id the guest uses to find the device.
pub(super) fn env_pairs(disk: &RawBtrfsDisk) -> [(String, String); 4] {
    [
        (CARGO_TARGET_DIR_ENV.to_owned(), TARGET_DIR.to_owned()),
        (CARGO_HOME_ENV.to_owned(), CARGO_HOME.to_owned()),
        ("CANG_BUILD_DISK_ID".to_owned(), disk.id.clone()),
        ("CANG_BUILD_DISK_LABEL".to_owned(), disk.label.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::raw_btrfs::test_support::FakeRunner;
    use super::*;

    fn test_disk() -> RawBtrfsDisk {
        RawBtrfsDisk {
            path: Path::new("/state/cang-build.raw").to_path_buf(),
            id: ID.to_owned(),
            label: LABEL.to_owned(),
            size_bytes: SIZE_BYTES,
            status: raw_btrfs::RawBtrfsDiskStatus::Reused,
        }
    }

    #[test]
    fn prepares_a_workspace_scoped_build_disk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::default();
        let disk = prepare_with_runner(temp.path(), &runner).expect("prepare");

        assert_eq!(disk.id, ID);
        assert_eq!(disk.label, LABEL);
        assert_eq!(disk.path, temp.path().join(FILE_NAME));
        assert_eq!(disk.size_bytes, SIZE_BYTES);
    }

    /// Both measured working sets must land on the disk: the output directory and
    /// the registry the compiler reads from.
    #[test]
    fn env_points_both_cargo_working_sets_at_the_disk() {
        let env: std::collections::HashMap<_, _> = env_pairs(&test_disk()).into_iter().collect();
        assert_eq!(env[CARGO_TARGET_DIR_ENV], TARGET_DIR);
        assert_eq!(env[CARGO_HOME_ENV], CARGO_HOME);
        assert_eq!(env["CANG_BUILD_DISK_ID"], ID);
        assert_eq!(env["CANG_BUILD_DISK_LABEL"], LABEL);
        // Both paths live inside the mount point the guest mounts the disk on.
        for value in [&env[CARGO_TARGET_DIR_ENV], &env[CARGO_HOME_ENV]] {
            assert!(
                value.starts_with(&format!("{MOUNT_POINT}/")),
                "{value} must be under {MOUNT_POINT}"
            );
        }
    }

    #[test]
    fn attachment_is_writable_and_named_after_the_disk() {
        let attachment = attachment(&test_disk());
        assert!(!attachment.read_only, "a build disk must be writable");
        assert_eq!(attachment.id, ID);
        assert_eq!(attachment.path, Path::new("/state/cang-build.raw"));
    }
}
