//! The workspace disk: `/workspace` as the btrfs tree the launch named.
//!
//! With `--workspace-disk` the host contributes no `/workspace` bind mount, so
//! `/workspace` is an empty rootfs directory until this runs. In order:
//!
//! 1. mount the labelled btrfs disk at `/workspace`;
//! 2. hand the tree to the developer identity;
//! 3. create the `cargo/` directory inside it, seed it from the registry the
//!    launch mounted at `~/.cargo` on the first task of a workspace, and bind-mount
//!    it at `~/.cargo`, so cargo's own default home is the registry on the disk.
//!
//! The seed has to happen before the bind: `~/.cargo` is the host's registry
//! until the bind hides it, which is the same copy the build disk made, once
//! per workspace instead of once per task.
//!
//! A failure here is fatal, unlike the build disk's: a guest whose `/workspace`
//! is an empty directory would run the command against nothing, and every
//! measurement would be of a workspace that does not exist.

use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

use crate::guest_init::components::env::WorkspaceDiskContract;
use crate::guest_init::components::home::identity::DevIdentity;

/// Where the disk is mounted.
pub(in crate::guest_init) const MOUNT_POINT: &str = "/workspace";
/// The cargo home inside the tree, and where the guest binds it.
pub(in crate::guest_init) const CARGO_HOME_DIR: &str = "/workspace/cargo";
pub(in crate::guest_init) const CARGO_HOME_TARGET: &str = "/home/dev/.cargo";
/// The registry the launch mounted; the seed source, read before the bind hides
/// it. (`~/.cargo` is the same path, which is why the order matters.)
const MOUNTED_CARGO_HOME: &str = "/home/dev/.cargo";

pub(in crate::guest_init) fn find_disk(label: &str, disk_id: &str) -> Result<PathBuf> {
    crate::guest_init::components::disk::btrfs::find_labeled_disk_with_preferred(
        label,
        disk_id,
        &[],
    )
    .with_context(|| format!("workspace btrfs disk not found (label={label} id={disk_id})"))
}

pub(in crate::guest_init) fn ensure_ready(
    contract: &WorkspaceDiskContract,
    identity: &DevIdentity,
) -> Result<PathBuf> {
    ensure_ready_with(contract, identity, &SystemWorkspaceDiskOps)
}

fn ensure_ready_with(
    contract: &WorkspaceDiskContract,
    identity: &DevIdentity,
    ops: &impl WorkspaceDiskOps,
) -> Result<PathBuf> {
    let mount = Path::new(MOUNT_POINT);
    ops.create_dir_all(mount)?;
    let disk = ops.find_disk(&contract.disk_label, &contract.disk_id)?;
    if !ops.is_mounted(mount)? {
        ops.mount_btrfs(&disk, mount)
            .context("failed to mount the workspace btrfs disk")?;
    }
    ops.chown(mount, identity.uid, identity.gid)?;

    let cargo_home = Path::new(CARGO_HOME_DIR);
    ops.create_dir_all(cargo_home)?;
    ops.chown(cargo_home, identity.uid, identity.gid)?;

    if ops.is_empty(cargo_home)? {
        ops.seed_cargo_home(Path::new(MOUNTED_CARGO_HOME), cargo_home)?;
        ops.chown(cargo_home, identity.uid, identity.gid)?;
    }

    // The launch mounts the host's registry at `~/.cargo` and this replaces it: the
    // host mount is unmounted rather than covered, so exactly one filesystem is
    // behind the path and nothing reads the share by mistake afterwards.
    let target = Path::new(CARGO_HOME_TARGET);
    if ops.is_mounted(target)? {
        ops.unmount(target)
            .context("failed to unmount the launch's ~/.cargo registry mount")?;
    }
    ops.create_dir_all(target)?;
    ops.bind_mount(cargo_home, target)
        .context("failed to bind the workspace cargo home at ~/.cargo")?;
    Ok(mount.to_path_buf())
}

trait WorkspaceDiskOps {
    fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn is_mounted(&self, path: &Path) -> Result<bool>;
    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()>;
    fn bind_mount(&self, source: &Path, target: &Path) -> Result<()>;
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> Result<()>;
    fn unmount(&self, path: &Path) -> Result<()>;
    fn is_empty(&self, path: &Path) -> Result<bool>;
    fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct SystemWorkspaceDiskOps;

impl WorkspaceDiskOps for SystemWorkspaceDiskOps {
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
        crate::guest_init::command::run(
            "mount",
            &["-t", "btrfs", path_str(disk)?, path_str(mount)?],
        )
    }

    fn bind_mount(&self, source: &Path, target: &Path) -> Result<()> {
        crate::guest_init::command::run("mount", &["--bind", path_str(source)?, path_str(target)?])
    }

    fn chown(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
        crate::guest_init::fs::chown(path, uid, gid)
    }

    fn unmount(&self, path: &Path) -> Result<()> {
        crate::guest_init::command::run("umount", &[path_str(path)?])
    }

    fn is_empty(&self, path: &Path) -> Result<bool> {
        let mut entries = std::fs::read_dir(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(entries.next().is_none())
    }

    /// `cp -a` rather than a reflink: the source is a virtio-fs share, so there
    /// are no extents to share; this is a one-time registry copy per workspace.
    fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()> {
        if !from.is_dir() {
            return Ok(());
        }
        crate::guest_init::command::run("cp", &["-a", "-T", path_str(from)?, path_str(to)?])
    }
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn identity() -> DevIdentity {
        DevIdentity {
            uid: 1000,
            gid: 1001,
            home: PathBuf::from("/home/dev"),
            shell: PathBuf::from("/bin/sh"),
        }
    }

    fn contract() -> WorkspaceDiskContract {
        WorkspaceDiskContract {
            disk_id: "cang-workspace".to_owned(),
            disk_label: "CANG_WORKSPACE".to_owned(),
        }
    }

    #[derive(Default)]
    struct FakeWorkspaceDiskOps {
        mounted: RefCell<Vec<String>>,
        disk: PathBuf,
        operations: RefCell<Vec<String>>,
        mount_error: RefCell<Option<String>>,
        cargo_home_empty: bool,
    }

    impl FakeWorkspaceDiskOps {
        fn new() -> Self {
            Self {
                disk: PathBuf::from("/dev/vdb"),
                cargo_home_empty: true,
                ..Self::default()
            }
        }
    }

    impl WorkspaceDiskOps for FakeWorkspaceDiskOps {
        fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf> {
            self.operations
                .borrow_mut()
                .push(format!("find:{label}:{disk_id}"));
            Ok(self.disk.clone())
        }

        fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("mkdir:{}", path.display()));
            Ok(())
        }

        fn is_mounted(&self, path: &Path) -> Result<bool> {
            let key = path.display().to_string();
            self.operations.borrow_mut().push(format!("findmnt:{key}"));
            Ok(self.mounted.borrow().iter().any(|entry| entry == &key))
        }

        fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()> {
            if let Some(message) = self.mount_error.borrow_mut().take() {
                return Err(anyhow!(message));
            }
            self.operations.borrow_mut().push(format!(
                "mount:{}:{}",
                disk.display(),
                mount.display()
            ));
            self.mounted.borrow_mut().push(mount.display().to_string());
            Ok(())
        }

        fn bind_mount(&self, source: &Path, target: &Path) -> Result<()> {
            self.operations.borrow_mut().push(format!(
                "bind:{}:{}",
                source.display(),
                target.display()
            ));
            self.mounted.borrow_mut().push(target.display().to_string());
            Ok(())
        }

        fn chown(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("chown:{}:{uid}:{gid}", path.display()));
            Ok(())
        }

        fn unmount(&self, path: &Path) -> Result<()> {
            let key = path.display().to_string();
            self.operations.borrow_mut().push(format!("umount:{key}"));
            self.mounted.borrow_mut().retain(|entry| entry != &key);
            Ok(())
        }

        fn is_empty(&self, path: &Path) -> Result<bool> {
            self.operations
                .borrow_mut()
                .push(format!("is_empty:{}", path.display()));
            Ok(self.cargo_home_empty)
        }

        fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("seed:{}:{}", from.display(), to.display()));
            Ok(())
        }
    }

    #[test]
    fn mounts_the_tree_and_binds_the_cargo_home() {
        let ops = FakeWorkspaceDiskOps::new();
        let mount = ensure_ready_with(&contract(), &identity(), &ops).expect("should mount");

        assert_eq!(mount, PathBuf::from(MOUNT_POINT));
        assert_eq!(
            ops.operations.borrow().as_slice(),
            [
                "mkdir:/workspace",
                "find:CANG_WORKSPACE:cang-workspace",
                "findmnt:/workspace",
                "mount:/dev/vdb:/workspace",
                "chown:/workspace:1000:1001",
                "mkdir:/workspace/cargo",
                "chown:/workspace/cargo:1000:1001",
                "is_empty:/workspace/cargo",
                "seed:/home/dev/.cargo:/workspace/cargo",
                "chown:/workspace/cargo:1000:1001",
                "findmnt:/home/dev/.cargo",
                "mkdir:/home/dev/.cargo",
                "bind:/workspace/cargo:/home/dev/.cargo",
            ]
        );
    }

    /// The launch's registry mount is unmounted, not covered: one filesystem sits
    /// behind `~/.cargo`, and it is the tree's.
    #[test]
    fn the_launch_registry_mount_is_unmounted_before_the_bind() {
        let ops = FakeWorkspaceDiskOps::new();
        ops.mounted.borrow_mut().push("/home/dev/.cargo".to_owned());
        ensure_ready_with(&contract(), &identity(), &ops).expect("should mount");

        let operations = ops.operations.borrow();
        let unmount = operations
            .iter()
            .position(|op| op == "umount:/home/dev/.cargo")
            .expect("the launch mount is unmounted");
        let bind = operations
            .iter()
            .position(|op| op == "bind:/workspace/cargo:/home/dev/.cargo")
            .expect("the tree's cargo is bound");
        assert!(unmount < bind, "unmount first, then bind");
        assert_eq!(
            ops.mounted.borrow().as_slice(),
            ["/workspace", "/home/dev/.cargo"],
            "exactly one mount remains at the path, and it is the tree's"
        );
    }

    #[test]
    fn a_populated_registry_on_the_disk_is_not_seeded_again() {
        let mut ops = FakeWorkspaceDiskOps::new();
        ops.cargo_home_empty = false;
        ensure_ready_with(&contract(), &identity(), &ops).expect("should mount");

        assert!(
            !ops.operations
                .borrow()
                .iter()
                .any(|op| op.starts_with("seed:"))
        );
    }

    #[test]
    fn does_not_remount_what_is_already_mounted() {
        let ops = FakeWorkspaceDiskOps::new();
        ops.mounted.borrow_mut().push("/workspace".to_owned());
        ensure_ready_with(&contract(), &identity(), &ops).expect("should reuse");

        let operations = ops.operations.borrow();
        assert!(!operations.iter().any(|op| op.starts_with("mount:")));
        assert!(
            operations
                .iter()
                .any(|op| op == "bind:/workspace/cargo:/home/dev/.cargo")
        );
    }

    #[test]
    fn a_failed_mount_is_fatal() {
        let ops = FakeWorkspaceDiskOps::new();
        ops.mount_error
            .borrow_mut()
            .replace("wrong fs type".to_owned());

        let err = ensure_ready_with(&contract(), &identity(), &ops).expect_err("must be fatal");

        assert!(format!("{err:#}").contains("wrong fs type"));
        assert!(
            !ops.operations
                .borrow()
                .iter()
                .any(|op| op.starts_with("bind:"))
        );
    }
}
