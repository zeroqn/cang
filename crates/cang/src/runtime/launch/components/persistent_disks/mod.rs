//! Persistent launch disk contributors.
//!
//! A launch contributes the persistent container-store carrier, the `/nix` env,
//! and - when an image could be built - the read-only block lower the guest
//! composes its own `/nix` overlay from (see `nix_lower`).
//!
//! Two disks used to live here. The build disk (`cang-build.raw`) is retired
//! (wayfinder ticket 27): cargo's `target/` is its own `./target` in the tree and
//! the cargo home is bound onto `~/.cargo` from the tree, so nothing needs a
//! 96 GiB image beside the workspace. A launch that finds one reclaims it
//! (`retired_build_disk`), and no launch creates, attaches or exports it again.

use anyhow::Result;
use std::path::Path;

use crate::runtime::launch::config::DiskAttachment;
use raw_btrfs::{HostRawImageCommandRunner, RawBtrfsDisk};

mod containers;
mod nix;
pub(crate) mod nix_lower;
pub(crate) mod raw_btrfs;
pub(crate) mod retired_build_disk;
pub(crate) mod workspace;

pub(crate) fn grow_container_store(
    state_root: &Path,
    target_size_bytes: u64,
) -> Result<RawBtrfsDisk> {
    containers::grow_with_runner(state_root, target_size_bytes, &HostRawImageCommandRunner)
}

pub(crate) fn recreate_container_store(state_root: &Path) -> Result<RawBtrfsDisk> {
    containers::recreate_with_runner(state_root, &HostRawImageCommandRunner)
}

pub(crate) fn container_store_attachment(disk: &RawBtrfsDisk) -> DiskAttachment {
    containers::attachment(disk)
}

pub(crate) fn container_store_raw_disk_env_pairs(disk: &RawBtrfsDisk) -> [(String, String); 4] {
    containers::raw_disk_env_pairs(disk)
}

pub(crate) fn container_store_default_size_bytes() -> u64 {
    containers::default_size_bytes()
}

/// The read-only block lower for the guest's `/nix` overlay.
///
/// `Ok(None)` means the launch kept the host-composed overlay: either it asked for
/// it (`--nix-lower=virtiofs`) or the block lower could not be served under
/// `--nix-lower=auto`. `--nix-lower=blk` is strict and turns that into an error.
pub(crate) fn prepare_nix_lower_block(
    intent: &crate::runtime::launch::config::HostNixOverlay,
    guest_init: &Path,
    want_block_lower: bool,
) -> Result<Option<nix_lower::NixLowerBlock>> {
    nix_lower::prepare_block(intent, guest_init, want_block_lower)
}

/// Reclaim a retired build disk a previous cang left in this workspace.
pub(crate) fn reclaim_retired_build_disk(
    state_root: &Path,
) -> Result<Option<retired_build_disk::Reclaimed>> {
    retired_build_disk::reclaim(state_root)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistentDisks {
    container_store: RawBtrfsDisk,
}

impl PersistentDisks {
    pub(crate) fn attachments(&self) -> Vec<DiskAttachment> {
        vec![containers::attachment(&self.container_store)]
    }

    pub(crate) fn env_pairs(&self) -> Vec<(String, String)> {
        let mut env = nix::overlay_env_pairs().to_vec();
        env.extend(containers::raw_disk_env_pairs(&self.container_store));
        env
    }
}

pub(crate) trait PersistentDiskPreparer {
    fn prepare(&self, state_root: &Path) -> Result<PersistentDisks>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HostPersistentDiskPreparer;

impl PersistentDiskPreparer for HostPersistentDiskPreparer {
    fn prepare(&self, state_root: &Path) -> Result<PersistentDisks> {
        prepare_with_runner(state_root, &HostRawImageCommandRunner)
    }
}

fn prepare_with_runner(
    state_root: &Path,
    runner: &impl raw_btrfs::RawImageCommandRunner,
) -> Result<PersistentDisks> {
    Ok(PersistentDisks {
        container_store: containers::prepare_with_runner(state_root, runner)?,
    })
}

#[cfg(test)]
mod tests {
    use super::raw_btrfs::{RawBtrfsDiskStatus, test_support::FakeRunner};
    use super::*;
    use std::fs::File;

    #[test]
    fn prepares_workspace_scoped_container_store_raw_btrfs_disk_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::default();

        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        assert!(!temp.path().join(nix::FILE_NAME).exists());
        let container_disk = disks.container_store;
        assert_eq!(container_disk.path, temp.path().join(containers::FILE_NAME));
        assert_eq!(container_disk.id, containers::ID);
        assert_eq!(container_disk.label, containers::LABEL);
        // One mkfs, one disk: the build disk is gone (ticket 27).
        assert_eq!(runner.mkfs_call_count(), 1);
    }

    #[test]
    fn reuses_existing_valid_disks_without_formatting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = File::create(temp.path().join(containers::FILE_NAME)).expect("disk file");
        file.set_len(containers::SIZE_BYTES).expect("disk size");
        let runner = FakeRunner::with_probes(&["btrfs"]);

        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        assert_eq!(disks.container_store.status, RawBtrfsDiskStatus::Reused);
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn disk_validation_failure_is_classified_as_cang_persistent_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = File::create(temp.path().join(containers::FILE_NAME)).expect("disk file");
        file.set_len(containers::SIZE_BYTES).expect("disk size");
        let runner = FakeRunner::default();
        runner.push_probe_error("blkid exploded");

        let err = prepare_with_runner(temp.path(), &runner).expect_err("disk prep should fail");

        assert!(format!("{err:#}").contains("failed to prepare cang persistent container-store"));
        assert!(format!("{err:#}").contains("blkid exploded"));
    }

    #[test]
    fn a_launch_prepares_no_build_disk_and_exports_no_cargo_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::default();

        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        assert!(!temp.path().join(retired_build_disk::FILE_NAME).exists());
        let attachments = disks.attachments();
        let attachment_ids: Vec<&str> = attachments
            .iter()
            .map(|attachment| attachment.id.as_str())
            .collect();
        assert_eq!(attachment_ids, vec![containers::ID]);
        let env = disks.env_pairs();
        assert!(
            !env.iter()
                .any(|(key, _)| key == "CARGO_TARGET_DIR" || key == "CARGO_HOME")
        );
        assert_eq!(runner.mkfs_call_count(), 1);
    }

    #[test]
    fn persistent_disk_owners_preserve_attachments() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::default();
        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        let attachments = disks.attachments();

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, containers::ID);
        assert_eq!(attachments[0].path, temp.path().join(containers::FILE_NAME));
        assert!(!attachments[0].read_only);
    }

    #[test]
    fn persistent_disk_owners_preserve_guest_env() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::default();
        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        let env = disks.env_pairs();

        assert_eq!(
            env,
            vec![
                ("CANG_NIX_OVERLAY".to_owned(), "1".to_owned()),
                ("CANG_CONTAINERS_STORAGE".to_owned(), "1".to_owned()),
                ("CANG_CONTAINERS_STORE".to_owned(), "raw-disk".to_owned()),
                (
                    "CANG_CONTAINERS_DISK_ID".to_owned(),
                    "cang-containers".to_owned(),
                ),
                (
                    "CANG_CONTAINERS_DISK_LABEL".to_owned(),
                    "CANG_CONTAINERS".to_owned(),
                ),
            ]
        );
        assert!(!env.iter().any(|(key, _)| key == "CANG_NIX_DISK_ID"));
        assert!(!env.iter().any(|(key, _)| key == "CANG_NIX_DISK_LABEL"));
        assert!(!env.iter().any(|(key, _)| key == "CANG_BUILD_DISK_ID"));
    }
}
