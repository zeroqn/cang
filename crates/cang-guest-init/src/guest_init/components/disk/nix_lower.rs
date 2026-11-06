//! The read-only `/nix` lower disk: find it and mount it.
//!
//! When the host can build one, the image's `/nix/store` arrives as a read-only
//! btrfs image on virtio-blk instead of over virtio-fs, and the guest composes
//! the `/nix` overlay itself. Note 56 measured why: the lower is read on every
//! command, and `find`/`du` over it are 7.5-43x faster on the block device.
//!
//! A disk that cannot be found or mounted is not fatal: the host-composed overlay
//! is still mounted at `/nix` underneath, and the caller keeps using it.

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

/// Where the read-only lower is mounted in the guest.
pub(in crate::guest_init) const MOUNT_POINT: &str = "/run/cang/nix-lower";
/// libkrun attaches the persistent disks in launch order, so a hint only saves a
/// `blkid -L` lookup; `CANG_NIX_LOWER` is what actually identifies the disk.
const PREFERRED_LOWER_DISKS: &[&str] = &["/dev/vdb", "/dev/vdd", "/dev/vde"];

pub(in crate::guest_init) fn find_disk(label: &str, disk_id: &str) -> Result<PathBuf> {
    crate::guest_init::components::disk::btrfs::find_labeled_disk_with_preferred(
        label,
        disk_id,
        PREFERRED_LOWER_DISKS,
    )
    .with_context(|| format!("read-only /nix lower disk not found (label={label} id={disk_id})"))
}

/// `mount -t btrfs -o ro <disk> <mount point>`: read-only, and the operands last
/// so a stray option cannot turn into a third operand.
pub(in crate::guest_init) fn mount_args<'a>(
    disk: &'a Path,
    mount: &'a Path,
) -> Result<[&'a str; 6]> {
    Ok(["-t", "btrfs", "-o", "ro", path_str(disk)?, path_str(mount)?])
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_arguments_are_read_only_and_operands_last() {
        let args = mount_args(Path::new("/dev/vdb"), Path::new(MOUNT_POINT)).unwrap();

        assert_eq!(args, ["-t", "btrfs", "-o", "ro", "/dev/vdb", MOUNT_POINT]);
    }

    #[test]
    fn the_mount_point_is_under_the_run_directory() {
        assert_eq!(MOUNT_POINT, "/run/cang/nix-lower");
    }
}
