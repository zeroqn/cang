//! The retired build disk: `cang-build.raw`, reclaimed once by a launch.
//!
//! Until ticket 27 a launch created a 96 GiB sparse btrfs image per workspace in
//! the state root and attached it as virtio-blk, exporting `CARGO_TARGET_DIR`
//! and `CARGO_HOME` for it. Cargo's output is now the tree's own `./target` and
//! the cargo home is bound from the tree onto `~/.cargo`, so the disk has no
//! tenant left: cang no longer creates, attaches or exports it, and a launch that
//! finds one deletes it and says what it held.
//!
//! The deletion is deliberately narrow, because it is the one cang action that
//! removes a user's file without being asked for by name:
//!
//! - only `<state root>/cang-build.raw`, the exact name cang created itself -
//!   never a path a flag named, never anything else in the state root;
//! - only when an exclusive, non-blocking `flock` on the image is available, which
//!   is the same evidence a resize needs that no guest has it attached (a busy
//!   image is left alone, silently);
//! - only a regular file: a symlink or a directory in that name is left untouched,
//!   because following either would delete something outside the state root.

use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub(crate) const FILE_NAME: &str = "cang-build.raw";

/// What a reclaim removed, so the caller can print exactly one line about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reclaimed {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
}

/// Reclaim the retired build disk, if this workspace has one and nothing holds it.
///
/// `Ok(None)` means there was nothing to do: no image, an image that is not a
/// regular file, or an image another process holds. Only the image itself is
/// removed; the state root's other contents are never touched.
pub(crate) fn reclaim(state_root: &Path) -> Result<Option<Reclaimed>> {
    let path = state_root.join(FILE_NAME);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect '{}'", path.display()));
        }
    };
    if !metadata.file_type().is_file() {
        return Ok(None);
    }

    let file = File::open(&path)
        .with_context(|| format!("failed to open retired build disk '{}'", path.display()))?;
    // `LOCK_EX` is the attachment's rule (a guest holding the image is the case
    // this must not interrupt); `LOCK_NB` is why a busy image is silent rather
    // than a launch that refuses to start.
    // SAFETY: `file` is open for the duration of the call and the fd is valid.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return Ok(None);
    }
    let bytes = metadata.len();
    // Read the length before the delete: after it there is nothing to measure.
    fs::remove_file(&path)
        .with_context(|| format!("failed to reclaim retired build disk '{}'", path.display()))?;
    drop(file);
    Ok(Some(Reclaimed { path, bytes }))
}

/// The one line a reclaim prints.
pub(crate) fn report(reclaimed: &Reclaimed) -> String {
    format!(
        "cang: reclaimed {} from '{}' - a warm target/, the cargo home and the retired /nix \
         upper lived there; they now live in the workspace image's tree, so cargo's ./target \
         and the tree's cargo home replace it and cang no longer creates or attaches a build disk",
        human_bytes(reclaimed.bytes),
        reclaimed.path.display()
    )
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1024_u64.pow(4)),
        ("GiB", 1024_u64.pow(3)),
        ("MiB", 1024_u64.pow(2)),
        ("KiB", 1024),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn state_root(name: &str) -> PathBuf {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(name);
        fs::create_dir_all(&root).expect("state root");
        // Leaked on purpose: `Reclaimed` reports a path, and the tests only need
        // the directory to outlive this call.
        std::mem::forget(temp);
        root
    }

    #[test]
    fn a_launch_reclaims_the_image_and_nothing_else() {
        let root = state_root("history");
        let disk = root.join(FILE_NAME);
        fs::write(&disk, vec![0u8; 4096]).expect("disk");
        fs::write(root.join("other.state"), b"keep").expect("other state");

        let reclaimed = reclaim(&root).expect("reclaim").expect("image existed");

        assert_eq!(reclaimed.bytes, 4096);
        assert_eq!(reclaimed.path, disk);
        assert!(!disk.exists());
        assert!(root.join("other.state").exists(), "only the image goes");
    }

    #[test]
    fn a_workspace_without_the_image_does_nothing() {
        let root = state_root("empty");

        assert_eq!(reclaim(&root).expect("reclaim"), None);
    }

    /// A held image is left alone and silently: the holder is a guest that is
    /// still using it, and a second process must not take it away.
    #[test]
    fn a_held_image_is_not_deleted() {
        let root = state_root("held");
        let disk = root.join(FILE_NAME);
        fs::write(&disk, vec![0u8; 1024]).expect("disk");
        let holder = File::open(&disk).expect("holder");
        // SAFETY: an fd this test owns.
        let locked = unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert!(locked, "the test takes the lock");

        assert_eq!(reclaim(&root).expect("reclaim"), None);
        assert!(disk.exists(), "a held image survives");
        drop(holder);
    }

    /// A symlink in the image's name is not a build disk cang created, and
    /// following it would delete a file outside the state root.
    #[test]
    fn a_symlink_is_neither_followed_nor_deleted() {
        let root = state_root("symlink");
        let outside = root.parent().unwrap().join("outside.raw");
        fs::write(&outside, vec![0u8; 4096]).expect("outside file");
        symlink(&outside, root.join(FILE_NAME)).expect("symlink");

        assert_eq!(reclaim(&root).expect("reclaim"), None);
        assert!(outside.exists(), "the symlink target survives");
        assert!(
            root.join(FILE_NAME).symlink_metadata().is_ok(),
            "and so does the link"
        );
    }

    #[test]
    fn the_line_names_the_bytes_what_it_held_and_where_that_lives_now() {
        let report = report(&Reclaimed {
            path: PathBuf::from("/state/workspace-1/cang-build.raw"),
            bytes: 96 * 1024 * 1024 * 1024,
        });

        assert!(
            report.starts_with("cang: reclaimed 96.0 GiB from "),
            "{report}"
        );
        assert!(
            report.contains("/state/workspace-1/cang-build.raw"),
            "{report}"
        );
        assert!(report.contains("warm target/"), "{report}");
        assert!(report.contains("cargo home"), "{report}");
        assert!(report.contains("retired /nix upper"), "{report}");
        assert!(report.contains("workspace image's tree"), "{report}");
        assert_eq!(report.lines().count(), 1, "one line, as reported");
    }

    #[test]
    fn human_bytes_reports_the_scale_a_user_recognizes() {
        assert_eq!(human_bytes(96 * 1024 * 1024 * 1024), "96.0 GiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(12), "12 bytes");
    }
}
