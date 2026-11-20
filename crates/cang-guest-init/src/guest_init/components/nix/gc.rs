//! `cang-guest-init internal nix gc`: reclaim a disk from `/nix` upper generations
//! the workspace no longer needs.
//!
//! The writable `/nix` upper lives on a guest-mounted disk - the build disk's btrfs
//! image for a host-directory launch, or the workspace tree for a
//! `--workspace-disk` one - neither of which the host can mount rootless, so
//! reclaiming it needs a guest. The rule is the seed's own: the generation the
//! workspace used last is the cache, and every other completed generation is dead
//! weight. `--all` drops the cache as well, so the next launch starts from an empty
//! upper.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::guest_init::command;
use crate::guest_init::components::disk::build;
use crate::guest_init::components::env::CangEnv;
use crate::guest_init::components::nix::block_upper::{SEEDED_SENTINEL, UPPER_DIR};

/// Where the generations live inside the build disk: `<mount>/nix-overlay/<digest>`.
const BUILD_DISK_GENERATIONS_DIR: &str = "nix-overlay";

/// The layout one gc run collects: the disk to mount and the directory that holds
/// its generations. The host names both, because only it knows whether this
/// workspace's `/nix` upper is the build disk's or the workspace tree's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct GcLayout {
    /// Where the disk is mounted inside this maintenance VM.
    pub(in crate::guest_init) mount: PathBuf,
    /// The generations directory, either absolute (already inside a mounted
    /// filesystem) or relative to `mount`.
    pub(in crate::guest_init) generations_dir: PathBuf,
    /// The btrfs label and the cang disk id to find: the build disk's for a
    /// host-directory launch, the workspace image's for `--workspace-disk`.
    pub(in crate::guest_init) disk_label: String,
    pub(in crate::guest_init) disk_id: String,
}

/// A generation directory on the build disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Generation {
    name: String,
    path: PathBuf,
    completed: bool,
    modified: SystemTime,
    bytes: u64,
}

pub(in crate::guest_init) fn run(
    all: bool,
    dry_run: bool,
    layout: Option<&GcLayout>,
) -> Result<()> {
    let env = CangEnv::from_process_env()?;
    let layout = layout.cloned().unwrap_or_else(|| GcLayout {
        mount: PathBuf::from(build::MOUNT_POINT),
        generations_dir: PathBuf::from(BUILD_DISK_GENERATIONS_DIR),
        disk_label: env.build_disk_label.clone(),
        disk_id: env.build_disk_id.clone(),
    });
    run_with_ops(all, dry_run, &layout, &SystemGcOps)
}

fn run_with_ops(all: bool, dry_run: bool, layout: &GcLayout, ops: &impl GcOps) -> Result<()> {
    let mount = layout.mount.as_path();
    let disk = ops.find_disk(&layout.disk_label, &layout.disk_id)?;
    ops.create_dir_all(mount)?;
    let was_mounted = ops.is_mounted(mount)?;
    if !was_mounted {
        ops.mount_btrfs(&disk, mount)
            .with_context(|| format!("failed to mount the disk at '{}'", mount.display()))?;
    }
    let collected = collect(layout, all, dry_run, ops);
    if !was_mounted {
        let unmounted = ops
            .unmount(mount)
            .with_context(|| format!("failed to unmount the disk at '{}'", mount.display()));
        // A failed collection is the more useful error; a failed unmount still
        // matters, because the maintenance VM is about to exit with its status.
        return match (collected, unmounted) {
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err),
            (Ok(()), Ok(())) => Ok(()),
        };
    }
    collected
}

fn collect(layout: &GcLayout, all: bool, dry_run: bool, ops: &impl GcOps) -> Result<()> {
    let generations = ops.generations(layout)?;
    if generations.is_empty() {
        println!("cang-guest-init: /nix upper gc: nothing to collect");
        return Ok(());
    }
    // The one the workspace used last is the cache; the rest are dead weight. With
    // `--all` there is no keep-set at all.
    let keep = if all {
        None
    } else {
        generations
            .iter()
            .find(|generation| generation.completed)
            .map(|generation| generation.name.clone())
    };

    let mut freed = 0u64;
    let mut collected = 0usize;
    for generation in &generations {
        if keep.as_deref() == Some(generation.name.as_str()) {
            println!(
                "cang-guest-init: /nix upper gc: keeping '{}' ({} bytes)",
                generation.name, generation.bytes
            );
            continue;
        }
        if dry_run {
            println!(
                "cang-guest-init: /nix upper gc: would collect '{}' ({} bytes)",
                generation.name, generation.bytes
            );
        } else {
            ops.remove(&generation.path).with_context(|| {
                format!(
                    "failed to delete the /nix upper generation '{}'",
                    generation.path.display()
                )
            })?;
            println!(
                "cang-guest-init: /nix upper gc: collected '{}' ({} bytes)",
                generation.name, generation.bytes
            );
        }
        freed += generation.bytes;
        collected += 1;
    }

    let verb = if dry_run { "would free" } else { "freed" };
    println!(
        "cang-guest-init: /nix upper gc: {verb} {freed} bytes across {collected} generation(s)"
    );
    Ok(())
}

trait GcOps {
    fn find_disk(&self, label: &str, id: &str) -> Result<PathBuf>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn is_mounted(&self, path: &Path) -> Result<bool>;
    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()>;
    fn unmount(&self, mount: &Path) -> Result<()>;
    fn generations(&self, layout: &GcLayout) -> Result<Vec<Generation>>;
    fn remove(&self, path: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct SystemGcOps;

impl GcOps for SystemGcOps {
    fn find_disk(&self, label: &str, id: &str) -> Result<PathBuf> {
        build::find_disk(label, id)
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        crate::guest_init::fs::create_dir_all(path)
    }

    fn is_mounted(&self, path: &Path) -> Result<bool> {
        command::status_ok("findmnt", &["-rn", path_str(path)?])
    }

    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()> {
        command::run("mount", &["-t", "btrfs", path_str(disk)?, path_str(mount)?])
    }

    fn unmount(&self, mount: &Path) -> Result<()> {
        command::run("umount", &[path_str(mount)?])
    }

    /// Every real generation directory, ordered the way the seed picks the one to
    /// clone from: completed first, then newest, then by name.
    fn generations(&self, layout: &GcLayout) -> Result<Vec<Generation>> {
        // An absolute generations dir is already inside a mounted filesystem (the
        // workspace tree, mounted by the guest before this ran); a relative one is
        // joined onto the mount this run performs itself.
        let root = if layout.generations_dir.is_absolute() {
            layout.generations_dir.clone()
        } else {
            layout.mount.join(&layout.generations_dir)
        };
        let mut generations = Vec::new();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(generations),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to list '{}'", root.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Staging trees are dot-names, and a symlink could aim this delete at
            // another tree; only a plain directory is a generation.
            if name.starts_with('.') || !is_plain_dir(&path) {
                continue;
            }
            let completed = path.join(UPPER_DIR).join(SEEDED_SENTINEL).is_file();
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let bytes = directory_bytes(&path)?;
            generations.push(Generation {
                name,
                path,
                completed,
                modified,
                bytes,
            });
        }
        generations.sort_by(|left, right| {
            right
                .completed
                .cmp(&left.completed)
                .then_with(|| right.modified.cmp(&left.modified))
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(generations)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        if command::status_ok("btrfs", &["subvolume", "show", path_str(path)?]).unwrap_or(false) {
            command::run("btrfs", &["subvolume", "delete", path_str(path)?])
        } else {
            match std::fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => {
                    Err(err).with_context(|| format!("failed to remove '{}'", path.display()))
                }
            }
        }
    }
}

fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

/// Size on disk of one generation, from `du -sb`: the number the report quotes.
fn directory_bytes(path: &Path) -> Result<u64> {
    let output = command::output_trimmed("du", &["-sb", path_str(path)?])?
        .ok_or_else(|| anyhow::anyhow!("du reported no size for '{}'", path.display()))?;
    let first = output
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("du reported no size for '{}'", path.display()))?;
    first
        .parse::<u64>()
        .with_context(|| format!("du reported an unparsable size '{first}'"))
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path '{}' is not valid UTF-8", path.display()))
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
