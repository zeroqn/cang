//! Cap the request size of the guest's virtio-blk disks.
//!
//! btrfs 7.x merges reads into multi-extent, multi-folio bios, and a merged bio
//! can carry a device LBA taken from one extent while its payload pages belong to
//! another (`btrfs_bio_is_contig`, `submit_extent_folio`, large folios). The guest
//! then reads the wrong device range, the pages fail their data checksum, and the
//! reader gets `EIO` - or `SIGBUS` if the page is mapped - on a file whose bytes
//! are fine on disk. Measured on this fork's 7.3.1 guest kernel: 210 failing
//! sectors over 12 cold 2.4 GB sweeps over the writable btrfs disks, and 40 KiB
//! windows of zeroes (crc32c of a zero page) over the read-only `/nix` lower.
//! 6.12 never merges that way and never fails.
//!
//! Nothing can be fixed from the guest, but the merge can be kept out of the
//! dangerous shape: `max_sectors_kb=64` (16 pages per request) never produced a
//! failing sector in 9 sweeps, keeps the checksums on - so a genuinely bad byte
//! still surfaces - and costs about 1.5 % on a cold 2.4 GB read, because it caps
//! request size and not the segment count, leaving the queue free to overlap.
//!
//! Drop this when the guest kernel stops merging reads that way.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The cap, in KiB: 16 pages per request, small enough that a merged bio cannot
/// span several extents.
pub(in crate::guest_init) const MAX_SECTORS_KB: &str = "64";
/// Every virtio-blk disk shows up as `vd*` under `/sys/block`.
const DEVICE_PREFIX: &str = "vd";
const QUEUE_DIR: &str = "/sys/block";
const ATTRIBUTE: &str = "queue/max_sectors_kb";

pub(in crate::guest_init) fn cap_request_size() -> Result<()> {
    let mut capped = 0usize;
    for device in block_devices()? {
        match apply(&device) {
            Ok(true) => capped += 1,
            Ok(false) => {}
            // A disk that cannot be capped is not fatal: the launch keeps its
            // disks, and the caller logs what this could not cover.
            Err(err) => eprintln!(
                "cang-guest-init: warning: failed to cap the request size of '{}': {err:#}",
                device.display()
            ),
        }
    }
    eprintln!(
        "cang-guest-init: capped the request size of {capped} virtio-blk disk(s) to {MAX_SECTORS_KB} KiB"
    );
    Ok(())
}

fn block_devices() -> Result<Vec<PathBuf>> {
    let mut devices = Vec::new();
    for entry in
        std::fs::read_dir(QUEUE_DIR).with_context(|| format!("failed to list '{QUEUE_DIR}'"))?
    {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(DEVICE_PREFIX) {
            devices.push(entry.path());
        }
    }
    devices.sort();
    Ok(devices)
}

/// Lower the cap if the disk allows more. Returns whether the file was written.
fn apply(device: &Path) -> Result<bool> {
    let attribute = device.join(ATTRIBUTE);
    let current = match std::fs::read_to_string(&attribute) {
        Ok(current) => current,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read '{}'", attribute.display()));
        }
    };
    match cap_decision(current.trim()) {
        Some(value) => {
            std::fs::write(&attribute, value).with_context(|| {
                format!("failed to write '{value}' to '{}'", attribute.display())
            })?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `Some(value)` when the current cap is higher than [`MAX_SECTORS_KB`].
fn cap_decision(current: &str) -> Option<&'static str> {
    let current: u64 = current.parse().ok()?;
    (current > MAX_SECTORS_KB.parse().unwrap_or(u64::MAX)).then_some(MAX_SECTORS_KB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_larger_cap_is_lowered_to_the_limit() {
        assert_eq!(cap_decision("4096"), Some(MAX_SECTORS_KB));
        assert_eq!(cap_decision("65"), Some(MAX_SECTORS_KB));
    }

    #[test]
    fn a_cap_at_or_below_the_limit_is_left_alone() {
        assert_eq!(cap_decision("64"), None);
        assert_eq!(cap_decision("32"), None);
    }

    #[test]
    fn an_unreadable_value_does_not_override_a_disk() {
        assert_eq!(cap_decision(""), None);
        assert_eq!(cap_decision("n/a"), None);
    }

    #[test]
    fn the_limit_keeps_verification_and_fits_a_folio() {
        // The cap must not shrink below one 256 KiB folio's worth of pages, or
        // reads would be cut into pieces the merge cannot even start from; and it
        // must stay above one page.
        let kib: u64 = MAX_SECTORS_KB.parse().expect("a number");
        assert!((4..=256).contains(&kib), "{kib} KiB");
    }
}
