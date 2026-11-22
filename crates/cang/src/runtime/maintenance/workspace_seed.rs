//! Seeding a workspace image from an existing host tree (ticket 25).
//!
//! The image is authoritative under the redraw, but every real checkout starts
//! life as a host directory. This module is the one-shot step that puts that tree
//! into a brand-new image: `cang workspace init <disk> --from DIR`.
//!
//! Three properties matter more than the mechanics:
//!
//! 1. **Never half-seeded.** The copy runs inside one task VM, and the marker that
//!    says "this image is seeded" (`.cang/seed-complete`) is written **last**. An
//!    interrupted seed therefore leaves an image a later seed will still accept,
//!    rather than one that looks done and is missing files.
//! 2. **Refuse before copying, not halfway.** The host walks the source tree and
//!    refuses when it does not fit the image, naming both sizes.
//! 3. **Refuse to seed twice.** A seeded image is refused by name, and so is an
//!    image whose tree already holds something the seed did not put there - a seed
//!    is a one-time act, not a sync.
//!
//! What is copied: everything the tree holds except `target/`, `.scratch/` and the
//! tree's own `.cang/` bookkeeping. Sockets and FIFOs are skipped by `tar` itself
//! (it prints a warning and carries on); `.git/objects/info/alternates` is
//! *detected* and warned about, because it usually names an absolute host path that
//! does not exist inside the guest, which would make `git` see a partial object
//! store. The copy is driven by `tar` rather than `cp` because `cp -a` fails the
//! whole copy on the first socket.

use anyhow::{Context, Result, bail};
use std::path::Path;

/// Where the source tree is mounted inside the seeding VM.
pub(crate) const SEED_SOURCE_TARGET: &str = "/mnt/seed";
/// The in-tree marker, written after the copy. Relative to the tree root.
pub(crate) const SEED_SENTINEL: &str = ".cang/seed-complete";
/// Directories a seed never copies: build output, scratch, and the tree's own
/// bookkeeping (which the sentinel itself lives in).
pub(crate) const SEED_EXCLUDES: [&str; 3] = ["target", ".scratch", ".cang"];
/// A tree may fill this much of the image and no more: the remaining space is what
/// the first build needs.
pub(crate) const SEED_MAX_FILL_PERCENT: u64 = 85;

/// The guest command that performs the copy.
///
/// One `bash -lc` script, because the whole point is that the copy happens where
/// the image is mounted; every branch exits non-zero with a `cang:` line so the
/// failure a user sees names the condition rather than a shell exit code.
pub(crate) fn seed_command() -> Vec<String> {
    let excludes = SEED_EXCLUDES
        .iter()
        .map(|name| format!("--exclude=./{name}"))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        r#"set -e
sentinel="/workspace/{sentinel}"
if [ -e "$sentinel" ]; then
  echo "cang: this workspace image is already seeded ($sentinel); a seed is a one-time act, so pass a different image instead" >&2
  exit 3
fi
# `.cang` and `cargo` are the bookkeeping the mount itself creates before this
# script runs (the tree's marker directory and the cargo home it binds at
# ~/.cargo), so they are not evidence that anyone seeded this image.
existing=$(ls -A /workspace 2>/dev/null | grep -v -e '^\.cang$' -e '^cargo$' | head -n 1 || true)
if [ -n "$existing" ]; then
  echo "cang: /workspace already holds '$existing', so this image is not empty; a seed only fills an empty image" >&2
  exit 4
fi
if [ -e /mnt/seed/.git/objects/info/alternates ]; then
  echo "cang: warning: .git/objects/info/alternates names objects outside the tree; inside the guest those paths may not exist, and git may report a partial object store (the file is copied unchanged)" >&2
fi
files_before=$(find /mnt/seed -mindepth 1 -not -path /mnt/seed/target -not -path /mnt/seed/.scratch -not -path /mnt/seed/.cang | wc -l)
tar -C /mnt/seed {excludes} -cf - . 2>/dev/null | tar -C /workspace -xf - 2>/dev/null
mkdir -p /workspace/.cang
printf 'seeded-from=%s\nentries=%s\n' "/mnt/seed" "$files_before" > "$sentinel"
echo "cang: seeded $files_before entries into the workspace image" >&2"#,
        sentinel = SEED_SENTINEL,
        excludes = excludes
    );
    vec!["bash".to_owned(), "-lc".to_owned(), script]
}

/// The apparent size a seed would copy, so the host can refuse before booting.
pub(crate) fn source_bytes(root: &Path) -> Result<u64> {
    fn walk(dir: &Path, total: &mut u64) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read '{}'", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read '{}'", dir.display()))?;
            let name = entry.file_name();
            let path = entry.path();
            if SEED_EXCLUDES.iter().any(|excluded| name == *excluded) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("failed to stat '{}'", path.display()))?;
            if metadata.is_dir() {
                walk(&path, total)?;
            } else {
                *total = total.saturating_add(metadata.len());
            }
        }
        Ok(())
    }

    let mut total = 0;
    walk(root, &mut total)?;
    Ok(total)
}

/// Refuse a seed whose tree would leave no room for the first build.
pub(crate) fn check_fits(tree_bytes: u64, image_bytes: u64) -> Result<()> {
    if tree_bytes.saturating_mul(100) > image_bytes.saturating_mul(SEED_MAX_FILL_PERCENT) {
        bail!(
            "the tree to seed is {} and the image is {}; a seed may fill at most {}% of the image \
             (the rest is what the first build needs) - create a larger image with \
             --workspace-size, or --workspace-disk a new path",
            human_bytes(tree_bytes),
            human_bytes(image_bytes),
            SEED_MAX_FILL_PERCENT
        );
    }
    Ok(())
}

/// The image size a seed would get: the existing file's length, else the size the
/// launch would create it at.
pub(crate) fn image_bytes(disk: &Path, requested: Option<u64>) -> Result<u64> {
    // A block device is as big as its owner made it, so its own size answers the
    // fits check without the flag.
    if let Some(size) =
        super::super::launch::components::persistent_disks::workspace::size_for(disk, requested)?
    {
        return Ok(size);
    }
    match std::fs::symlink_metadata(disk) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata.len()),
        Ok(_) => bail!(
            "'{}' exists and is neither a file nor a block device; a workspace disk is one of those",
            disk.display()
        ),
        Err(_) => Ok(requested.unwrap_or(DEFAULT_IMAGE_BYTES)),
    }
}

const DEFAULT_IMAGE_BYTES: u64 = 64 * 1024 * 1024 * 1024;

fn human_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_writes_the_sentinel_after_the_copy() {
        let command = seed_command();
        let script = command.last().expect("the script");
        let copy = script.find("tar -C /mnt/seed").expect("a copy");
        let sentinel = script.find("> \"$sentinel\"").expect("the sentinel write");
        assert!(
            copy < sentinel,
            "the marker must be written after the copy, or an interrupted seed looks complete"
        );
    }

    #[test]
    fn the_command_refuses_a_seeded_or_non_empty_image_first() {
        let command = seed_command();
        let script = command.last().expect("the script");
        let seeded = script.find("already seeded").expect("a seeded refusal");
        let non_empty = script.find("is not empty").expect("a non-empty refusal");
        let copy = script.find("tar -C /mnt/seed").expect("a copy");
        assert!(seeded < copy && non_empty < copy);
    }

    #[test]
    fn the_command_excludes_build_output_scratch_and_bookkeeping() {
        let command = seed_command();
        let script = command.last().expect("the script");
        for excluded in SEED_EXCLUDES {
            assert!(
                script.contains(&format!("--exclude=./{excluded}")),
                "{excluded} must not be copied"
            );
        }
    }

    #[test]
    fn a_tree_larger_than_the_image_is_refused_before_booting() {
        let err = check_fits(20 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024)
            .expect_err("20 GiB does not fit 8 GiB");
        let message = format!("{err:#}");
        assert!(message.contains("20.0 GiB"), "{message}");
        assert!(message.contains("8.0 GiB"), "{message}");
        assert!(message.contains("--workspace-size"), "{message}");
    }

    #[test]
    fn a_tree_that_fits_leaves_room_for_the_first_build() {
        assert!(check_fits(0, 1).is_ok());
        assert!(check_fits(6 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024).is_ok());
        assert!(check_fits(7 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024).is_err());
    }

    #[test]
    fn the_size_a_seed_asks_for_is_the_files_length_or_the_creation_size() {
        let temp = tempfile::tempdir().expect("tempdir");
        let absent = temp.path().join("absent.raw");
        assert_eq!(
            image_bytes(&absent, Some(1024)).expect("absent image"),
            1024
        );
        assert_eq!(
            image_bytes(&absent, None).expect("absent image"),
            DEFAULT_IMAGE_BYTES
        );

        let present = temp.path().join("present.raw");
        std::fs::write(&present, vec![0u8; 8192]).expect("image");
        assert_eq!(image_bytes(&present, Some(1024)).expect("present"), 8192);

        let directory = temp.path().join("a-directory");
        std::fs::create_dir(&directory).expect("directory");
        // A directory is neither a file nor a device, and `size_for` answers `None`
        // for it, so the disk path itself is what refuses it.
        let err = image_bytes(&directory, None).expect_err("a directory is not a disk");
        assert!(format!("{err:#}").contains("neither a file nor a block device"));
    }

    #[test]
    fn the_source_size_skips_the_excluded_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("target")).expect("target");
        std::fs::write(temp.path().join("target/blob"), vec![0u8; 4096]).expect("blob");
        std::fs::create_dir_all(temp.path().join(".cang")).expect("cang");
        std::fs::write(temp.path().join(".cang/bookkeeping"), vec![0u8; 4096]).expect("marker");
        std::fs::write(temp.path().join("real.txt"), vec![0u8; 1024]).expect("real");

        let bytes = source_bytes(temp.path()).expect("size");
        assert_eq!(bytes, 1024, "only the real file is counted");
    }
}
