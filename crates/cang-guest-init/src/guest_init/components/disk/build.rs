//! The build disk: mount it, seed the cargo home, and point cargo at both.
//!
//! `CARGO_TARGET_DIR` and `CARGO_HOME` arrive from the host pointing inside
//! [`MOUNT_POINT`], but the host cannot mount anything - until this runs, those
//! paths are just directories on the task rootfs, and a build would fill them and
//! lose everything at exit. So, in order:
//!
//! 1. mount the labelled btrfs disk over the mount point;
//! 2. hand it to the developer identity;
//! 3. create `target/` and `cargo/`;
//! 4. on the first task of a workspace, seed `cargo/` from the virtio-fs cargo
//!    home the image ships (`~/.cargo`), so the first build does not spend its
//!    time re-downloading a registry that is already in the guest - one copy per
//!    workspace, then the disk keeps it.
//!
//! A disk that cannot be mounted is not fatal, but it must not leave the two
//! variables pointing at rootfs directories that look like caches and are thrown
//! away with the task: a failed mount removes them, and the build falls back to
//! the share.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::guest_init::components::home::identity::DevIdentity;

/// Where the disk is mounted; also the prefix of the two cargo paths.
pub(in crate::guest_init) const MOUNT_POINT: &str = "/home/dev/.cache/cang-build";
pub(in crate::guest_init) const CARGO_TARGET_DIR: &str = "/home/dev/.cache/cang-build/target";
pub(in crate::guest_init) const CARGO_HOME: &str = "/home/dev/.cache/cang-build/cargo";
pub(in crate::guest_init) const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";
pub(in crate::guest_init) const CARGO_HOME_ENV: &str = "CARGO_HOME";
/// The virtio-fs cargo home every image ships; the seed source.
const IMAGE_CARGO_HOME: &str = "/home/dev/.cargo";
const PREFERRED_BUILD_DISK: &str = "/dev/vdc";

pub(in crate::guest_init) fn find_disk(label: &str, disk_id: &str) -> Result<PathBuf> {
    crate::guest_init::components::disk::btrfs::find_labeled_disk_with_preferred(
        label,
        disk_id,
        &[PREFERRED_BUILD_DISK],
    )
    .with_context(|| format!("build btrfs disk not found (label={label} id={disk_id})"))
}

/// Mounts the build disk, prepares both cargo directories and, on the first task
/// of a workspace, seeds the cargo home from the image's.
///
/// On failure both `CARGO_TARGET_DIR` and `CARGO_HOME` are removed, so the build
/// lands on the share instead of in a directory that disappears with the task.
pub(in crate::guest_init) fn ensure_ready(
    label: &str,
    disk_id: &str,
    identity: &DevIdentity,
) -> Result<PathBuf> {
    ensure_ready_with(label, disk_id, identity, &SystemBuildDiskOps)
}

fn ensure_ready_with(
    label: &str,
    disk_id: &str,
    identity: &DevIdentity,
    ops: &impl BuildDiskOps,
) -> Result<PathBuf> {
    match prepare_with_ops(label, disk_id, identity, ops) {
        Ok(mount) => Ok(mount),
        Err(err) => {
            unsafe {
                std::env::remove_var(CARGO_TARGET_DIR_ENV);
                std::env::remove_var(CARGO_HOME_ENV);
            }
            Err(err)
        }
    }
}

fn prepare_with_ops(
    label: &str,
    disk_id: &str,
    identity: &DevIdentity,
    ops: &impl BuildDiskOps,
) -> Result<PathBuf> {
    let mount = Path::new(MOUNT_POINT);
    ops.create_dir_all(mount)?;
    let disk = ops.find_disk(label, disk_id)?;
    if !ops.is_mounted(mount)? {
        match ops.mount_btrfs(&disk, mount) {
            Ok(()) => {}
            Err(_) if ops.is_mounted(mount).unwrap_or(false) => {}
            Err(err) => return Err(err).context("failed to mount the build btrfs disk"),
        }
    }
    for dir in [Path::new(CARGO_TARGET_DIR), Path::new(CARGO_HOME)] {
        ops.create_dir_all(dir)?;
        ops.chown(dir, identity.uid, identity.gid)?;
    }
    // The mount root itself is created by root before the mount.
    ops.chown(mount, identity.uid, identity.gid)?;
    if ops.is_empty(Path::new(CARGO_HOME))? {
        ops.seed_cargo_home(Path::new(IMAGE_CARGO_HOME), Path::new(CARGO_HOME))?;
        ops.chown(Path::new(CARGO_HOME), identity.uid, identity.gid)?;
    }
    Ok(mount.to_path_buf())
}

trait BuildDiskOps {
    fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn is_mounted(&self, path: &Path) -> Result<bool>;
    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()>;
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> Result<()>;
    fn is_empty(&self, path: &Path) -> Result<bool>;
    fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct SystemBuildDiskOps;

impl BuildDiskOps for SystemBuildDiskOps {
    fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf> {
        find_disk(label, disk_id)
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        crate::guest_init::fs::create_dir_all(path)
    }

    fn is_mounted(&self, path: &Path) -> Result<bool> {
        crate::guest_init::command::status_ok("findmnt", &["-rn", path_str(path)?])
    }

    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()> {
        let args = mount_btrfs_args(disk, mount)?;
        crate::guest_init::command::run("mount", &args)
    }

    fn chown(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
        crate::guest_init::fs::chown(path, uid, gid)
    }

    fn is_empty(&self, path: &Path) -> Result<bool> {
        let mut entries = std::fs::read_dir(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(entries.next().is_none())
    }

    /// Copies the image's cargo home onto the disk.
    ///
    /// `cp -a` rather than a reflink: the source is a virtio-fs share, so there
    /// are no shared extents to exploit; this is a one-time read of about 1.5 GiB
    /// per workspace, and afterwards every build reads the registry from the disk.
    fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()> {
        if !from.is_dir() {
            return Ok(());
        }
        crate::guest_init::command::run("cp", &["-a", "-T", path_str(from)?, path_str(to)?])
    }
}

fn mount_btrfs_args<'a>(disk: &'a Path, mount: &'a Path) -> Result<[&'a str; 4]> {
    Ok(["-t", "btrfs", path_str(disk)?, path_str(mount)?])
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_init::components::home::identity::DevIdentity;
    use std::cell::RefCell;

    fn identity() -> DevIdentity {
        DevIdentity {
            uid: 1000,
            gid: 1001,
            home: PathBuf::from("/home/dev"),
            shell: PathBuf::from("/bin/sh"),
        }
    }

    /// A first task of a workspace: mount, create both directories, seed the
    /// registry from the image so the first build is not a re-download.
    #[test]
    fn mounts_creates_and_seeds_on_the_first_task() {
        let ops = FakeBuildDiskOps::new(false, true);
        let mount = ensure_ready_with("cang-build", "disk-id", &identity(), &ops).unwrap();

        assert_eq!(mount, PathBuf::from(MOUNT_POINT));
        assert_eq!(
            ops.operations.borrow().as_slice(),
            [
                "mkdir:/home/dev/.cache/cang-build",
                "find:cang-build:disk-id",
                "findmnt:/home/dev/.cache/cang-build",
                "mount:/dev/vdc:/home/dev/.cache/cang-build",
                "mkdir:/home/dev/.cache/cang-build/target",
                "chown:/home/dev/.cache/cang-build/target:1000:1001",
                "mkdir:/home/dev/.cache/cang-build/cargo",
                "chown:/home/dev/.cache/cang-build/cargo:1000:1001",
                "chown:/home/dev/.cache/cang-build:1000:1001",
                "empty:/home/dev/.cache/cang-build/cargo",
                "seed:/home/dev/.cargo:/home/dev/.cache/cang-build/cargo",
                "chown:/home/dev/.cache/cang-build/cargo:1000:1001",
            ]
        );
    }

    /// A later task of the same workspace: the disk is already mounted and the
    /// cargo home already populated, so nothing is copied and nothing is remounted.
    #[test]
    fn a_warm_workspace_is_left_alone() {
        let ops = FakeBuildDiskOps::new(true, false);
        ensure_ready_with("cang-build", "disk-id", &identity(), &ops).unwrap();

        assert_eq!(
            ops.operations.borrow().as_slice(),
            [
                "mkdir:/home/dev/.cache/cang-build",
                "find:cang-build:disk-id",
                "findmnt:/home/dev/.cache/cang-build",
                "mkdir:/home/dev/.cache/cang-build/target",
                "chown:/home/dev/.cache/cang-build/target:1000:1001",
                "mkdir:/home/dev/.cache/cang-build/cargo",
                "chown:/home/dev/.cache/cang-build/cargo:1000:1001",
                "chown:/home/dev/.cache/cang-build:1000:1001",
                "empty:/home/dev/.cache/cang-build/cargo",
            ]
        );
    }

    /// A failed mount must not leave either variable pointing at a rootfs
    /// directory that looks like a cache and is thrown away with the task.
    #[test]
    fn a_failed_mount_removes_both_cargo_variables() {
        unsafe {
            std::env::set_var(CARGO_TARGET_DIR_ENV, CARGO_TARGET_DIR);
            std::env::set_var(CARGO_HOME_ENV, CARGO_HOME);
        }
        let ops = FakeBuildDiskOps::failing_mount();
        assert!(ensure_ready_with("cang-build", "disk-id", &identity(), &ops).is_err());
        assert!(std::env::var_os(CARGO_TARGET_DIR_ENV).is_none());
        assert!(std::env::var_os(CARGO_HOME_ENV).is_none());
    }

    /// `mount -t btrfs <disk> <mount>`: operands last, so a stray option cannot
    /// become a third operand and make the mount fail with a message about a
    /// missing device.
    #[test]
    fn mount_arguments_put_the_operands_last() {
        let args = mount_btrfs_args(Path::new("/dev/vdc"), Path::new(MOUNT_POINT)).unwrap();
        assert_eq!(args, ["-t", "btrfs", "/dev/vdc", MOUNT_POINT]);
    }

    struct FakeBuildDiskOps {
        mounted: bool,
        cargo_home_empty: bool,
        fail_mount: bool,
        operations: RefCell<Vec<String>>,
    }

    impl FakeBuildDiskOps {
        fn new(mounted: bool, cargo_home_empty: bool) -> Self {
            Self {
                mounted,
                cargo_home_empty,
                fail_mount: false,
                operations: RefCell::new(Vec::new()),
            }
        }

        fn failing_mount() -> Self {
            Self {
                mounted: false,
                cargo_home_empty: true,
                fail_mount: true,
                operations: RefCell::new(Vec::new()),
            }
        }

        fn record(&self, operation: impl Into<String>) {
            self.operations.borrow_mut().push(operation.into());
        }
    }

    impl BuildDiskOps for FakeBuildDiskOps {
        fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf> {
            self.record(format!("find:{label}:{disk_id}"));
            Ok(PathBuf::from("/dev/vdc"))
        }

        fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.record(format!("mkdir:{}", path.display()));
            Ok(())
        }

        fn is_mounted(&self, path: &Path) -> Result<bool> {
            self.record(format!("findmnt:{}", path.display()));
            Ok(self.mounted)
        }

        fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()> {
            self.record(format!("mount:{}:{}", disk.display(), mount.display()));
            if self.fail_mount {
                anyhow::bail!("mount failed");
            }
            Ok(())
        }

        fn chown(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
            self.record(format!("chown:{}:{uid}:{gid}", path.display()));
            Ok(())
        }

        fn is_empty(&self, path: &Path) -> Result<bool> {
            self.record(format!("empty:{}", path.display()));
            Ok(self.cargo_home_empty)
        }

        fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()> {
            self.record(format!("seed:{}:{}", from.display(), to.display()));
            Ok(())
        }
    }
}
