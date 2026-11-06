//! Persistent launch disk contributors.
//!
//! Host-overlay `/nix` mode contributes `/nix` env plus the selected persistent
//! container-store carrier, and - when an image could be built - the read-only
//! block lower the guest composes its own `/nix` overlay from (see `nix_lower`).

use anyhow::Result;
use std::path::Path;

use crate::runtime::launch::config::DiskAttachment;
use raw_btrfs::{HostRawImageCommandRunner, RawBtrfsDisk};

mod build;
mod containers;
mod nix;
pub(crate) mod nix_lower;
pub(crate) mod raw_btrfs;

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

/// The read-only block lower for the guest's `/nix` overlay, or `None` when the
/// launch keeps the host-composed overlay (no image could be built, no store to
/// serve, or `CANG_NIX_LOWER_BLOCK=0`).
pub(crate) fn prepare_nix_lower_block(
    intent: &crate::runtime::launch::config::HostNixOverlay,
) -> Option<nix_lower::NixLowerBlock> {
    nix_lower::prepare_block(intent)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistentDisks {
    container_store: RawBtrfsDisk,
    build_target: RawBtrfsDisk,
}

impl PersistentDisks {
    pub(crate) fn attachments(&self) -> Vec<DiskAttachment> {
        vec![
            containers::attachment(&self.container_store),
            build::attachment(&self.build_target),
        ]
    }

    pub(crate) fn env_pairs(&self) -> Vec<(String, String)> {
        let mut env = nix::host_overlay_env_pairs().to_vec();
        env.extend(containers::raw_disk_env_pairs(&self.container_store));
        env.extend(build::env_pairs(&self.build_target));
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
    let container_store = containers::prepare_with_runner(state_root, runner)?;
    let build_target = build::prepare_with_runner(state_root, runner)?;
    Ok(PersistentDisks {
        container_store,
        build_target,
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
        let target_disk = disks.build_target;
        assert_eq!(target_disk.path, temp.path().join(build::FILE_NAME));
        assert_eq!(target_disk.id, build::ID);
        assert_eq!(target_disk.label, build::LABEL);
        // One mkfs per disk: the container store and the build target.
        assert_eq!(runner.mkfs_call_count(), 2);
    }

    #[test]
    fn reuses_existing_valid_disks_without_formatting() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (name, size) in [
            (containers::FILE_NAME, containers::SIZE_BYTES),
            (build::FILE_NAME, build::SIZE_BYTES),
        ] {
            let file = File::create(temp.path().join(name)).expect("disk file");
            file.set_len(size).expect("disk size");
        }
        let runner = FakeRunner::with_probes(&["btrfs", "btrfs"]);

        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        assert_eq!(disks.container_store.status, RawBtrfsDiskStatus::Reused);
        assert_eq!(disks.build_target.status, RawBtrfsDiskStatus::Reused);
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
    fn persistent_disk_owners_preserve_attachments() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = FakeRunner::default();
        let disks = prepare_with_runner(temp.path(), &runner).expect("disks should prepare");

        let attachments = disks.attachments();

        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].id, containers::ID);
        assert_eq!(attachments[0].path, temp.path().join(containers::FILE_NAME));
        assert!(!attachments[0].read_only);
        assert_eq!(attachments[1].id, build::ID);
        assert_eq!(attachments[1].path, temp.path().join(build::FILE_NAME));
        assert!(!attachments[1].read_only);
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
                ("CANG_NIX_HOST_OVERLAY".to_owned(), "1".to_owned()),
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
                (
                    "CARGO_TARGET_DIR".to_owned(),
                    "/home/dev/.cache/cang-build/target".to_owned(),
                ),
                (
                    "CARGO_HOME".to_owned(),
                    "/home/dev/.cache/cang-build/cargo".to_owned(),
                ),
                ("CANG_BUILD_DISK_ID".to_owned(), "cang-build".to_owned()),
                ("CANG_BUILD_DISK_LABEL".to_owned(), "CANG_BUILD".to_owned(),),
            ]
        );
        assert!(!env.iter().any(|(key, _)| key == "CANG_NIX_DISK_ID"));
        assert!(!env.iter().any(|(key, _)| key == "CANG_NIX_DISK_LABEL"));
    }
}
