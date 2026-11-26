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
//! 4. create `cargo/magic-context/`, seed it once from the store the cortexkit
//!    share carries, and bind-mount it over
//!    `~/.local/share/cortexkit/magic-context`, so the agent's database writes
//!    land on the btrfs tree instead of the share. Only that one path moves: the
//!    rest of the share stays mounted where it was.
//!
//! The seed has to happen before the bind: `~/.cargo` is the host's registry
//! until the bind hides it, which is the same copy the build disk made, once
//! per workspace instead of once per task. The same is true of the cortexkit
//! share's `magic-context` directory, which the magic-context bind covers.
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

/// The magic-context store inside the tree, and the path of the mounted
/// cortexkit share it is bound over. The store is a SQLite database with a
/// `-wal` and `-shm` beside it, rewritten on every turn; over the share each of
/// those writes is a FUSE round trip, and the tree is where a build already
/// keeps its `target/`.
pub(in crate::guest_init) const MAGIC_CONTEXT_DIR: &str = "/workspace/cargo/magic-context";
pub(in crate::guest_init) const MAGIC_CONTEXT_TARGET: &str =
    "/home/dev/.local/share/cortexkit/magic-context";
/// The store's database name. Its `-wal`/`-shm` and the `context.db.corrupt-*`
/// dumps magic-context keeps beside it all carry this prefix and are all
/// deliberately not copied: the snapshot replaces the first pair, and the dumps
/// are the host store's own history of one bad database.
const MAGIC_CONTEXT_DB: &str = "context.db";

/// Where the tree records what it had free when it was mounted (relative to the
/// mount point), and the prefix of the line the file holds.
///
/// The file *is* the report: it carries exactly the one line the host parses
/// (`cang-workspace-free-bytes<TAB>free<TAB>total`), so a maintenance VM that
/// only wants the number mounts the disk read-only and hands the file back
/// instead of re-deciding anything. The host uses it between runs, because a
/// guest mount and a mid-run grow are mutually exclusive (virtio-blk fixes the
/// device size when it is probed).
pub(in crate::guest_init) const FREE_BYTES_FILE: &str = ".cang/free-bytes";
pub(in crate::guest_init) const FREE_BYTES_REPORT_PREFIX: &str = "cang-workspace-free-bytes";

/// Where a reading maintenance VM mounts the image. Never `/workspace`: a reader
/// must not be mistaken for the tree a command runs in.
const READ_MOUNT_POINT: &str = "/run/cang/workspace-free-bytes";

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
        // virtio-blk fixes a device's size when it is probed, so an image the host
        // grew between runs has a device bigger than the filesystem inside it.
        // Expanding here is the hand-over `cang disk resize` makes in its
        // maintenance VM, and an already-fitting filesystem makes this a no-op.
        ops.resize_max(mount)
            .context("failed to grow the workspace btrfs filesystem to the disk's size")?;
    }
    // The guest process's cwd was set to `/workspace` before this process started
    // (the init chdirs to the launch's workdir), when the path was still an empty
    // rootfs directory. Mounting the disk covers that directory, and a command
    // that inherited the old cwd would read the covered one: an empty
    // `/workspace` that is not the tree, even though `pwd` prints the same path.
    // Entering the mount point again is what puts the command in the tree.
    ops.enter_mount(mount)
        .context("failed to enter the mounted workspace tree")?;
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

    // The agent's store is bound over the share's own `magic-context` directory
    // rather than in place of the whole cortexkit mount: it is the one path the
    // agent writes to constantly, and everything else the share carries is
    // read-mostly.
    let magic_context = Path::new(MAGIC_CONTEXT_DIR);
    ops.create_dir_all(magic_context)?;
    ops.chown(magic_context, identity.uid, identity.gid)?;
    // The share's directory is the seed's source and the mount point the bind
    // covers. It is created only when missing, so a host that has never run
    // magic-context seeds nothing and the guest starts a fresh store.
    let shared_magic_context = Path::new(MAGIC_CONTEXT_TARGET);
    ops.create_dir_all(shared_magic_context)?;
    if ops.is_empty(magic_context)? {
        ops.seed_magic_context(shared_magic_context, magic_context)?;
        // The snapshot is written by this root process, and magic-context runs
        // as the developer identity.
        ops.chown_tree(magic_context, identity.uid, identity.gid)?;
    }
    ops.bind_mount(magic_context, shared_magic_context)
        .context("failed to bind the tree's magic-context store over the share")?;

    // What this mount found free, for the host's next decision. Written after the
    // seed so it measures the tree the next run will actually start from, and
    // best-effort: a tree that cannot hold this one file is a tree that is full,
    // and failing the launch over the note is worse than a missing number (the
    // host reads "no marker" as "do not grow").
    let _ = ops.record_free_bytes(mount, identity.uid, identity.gid);
    Ok(mount.to_path_buf())
}

/// Print the free-space marker the tree's last mount recorded.
///
/// The disk is mounted read-only and unmounted again: nothing here writes to the
/// image, which is why this can run beside a `cang workspace serve` whose lease
/// this maintenance VM does not take.
pub(in crate::guest_init) fn report_free_bytes() -> Result<()> {
    let env = crate::guest_init::components::env::CangEnv::from_process_env()?;
    let contract = env.workspace_disk.as_ref().ok_or_else(|| {
        anyhow!(
            "this maintenance VM was started without a workspace disk contract, so there is no tree to read a free-space marker from"
        )
    })?;
    report_free_bytes_with(contract, &SystemFreeBytesOps)
}

fn report_free_bytes_with(contract: &WorkspaceDiskContract, ops: &impl FreeBytesOps) -> Result<()> {
    let mount = Path::new(READ_MOUNT_POINT);
    ops.create_dir_all(mount)?;
    let disk = ops.find_disk(&contract.disk_label, &contract.disk_id)?;
    ops.mount_btrfs_read_only(&disk, mount)?;
    let report = ops.read_report(&mount.join(FREE_BYTES_FILE));
    let unmount = ops.unmount(mount);
    let report = report?;
    unmount.context("failed to unmount the workspace free-space read mount")?;
    let Some(report) = report else {
        return Ok(());
    };
    println!("{report}");
    Ok(())
}

trait FreeBytesOps {
    fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn mount_btrfs_read_only(&self, disk: &Path, mount: &Path) -> Result<()>;
    /// The marker's one line, or `None` when the tree carries no marker (an image
    /// whose guest-init predates it, or one no guest ever mounted).
    fn read_report(&self, path: &Path) -> Result<Option<String>>;
    fn unmount(&self, mount: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct SystemFreeBytesOps;

impl FreeBytesOps for SystemFreeBytesOps {
    fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf> {
        find_disk(label, disk_id)
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        crate::guest_init::fs::create_dir_all(path)
    }

    fn mount_btrfs_read_only(&self, disk: &Path, mount: &Path) -> Result<()> {
        crate::guest_init::command::run(
            "mount",
            &["-t", "btrfs", "-o", "ro", path_str(disk)?, path_str(mount)?],
        )
        .with_context(|| format!("failed to mount '{}' read-only", disk.display()))
    }

    fn read_report(&self, path: &Path) -> Result<Option<String>> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read '{}'", path.display()));
            }
        };
        Ok(contents
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(FREE_BYTES_REPORT_PREFIX))
            .map(str::to_owned))
    }

    fn unmount(&self, mount: &Path) -> Result<()> {
        crate::guest_init::command::run("umount", &[path_str(mount)?])
    }
}

/// The one line the tree records: `free` and `total` bytes of the filesystem the
/// tree is mounted from.
fn free_bytes_line(path: &Path) -> Result<String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow!("{} is not a usable path", path.display()))?;
    // SAFETY: `statvfs` writes into a buffer this call owns and reads only `c_path`.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Err(anyhow!(
            "failed to read free space for '{}': {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let free = stats.f_bavail as u64 * stats.f_frsize as u64;
    let total = stats.f_blocks as u64 * stats.f_frsize as u64;
    Ok(format!("{FREE_BYTES_REPORT_PREFIX}\t{free}\t{total}"))
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
    /// Seed the tree's magic-context store from the copy the share carries.
    fn seed_magic_context(&self, from: &Path, to: &Path) -> Result<()>;
    /// Hand a seeded tree to the developer identity.
    fn chown_tree(&self, path: &Path, uid: u32, gid: u32) -> Result<()>;
    /// Expand the filesystem to the disk's current device size.
    fn resize_max(&self, mount: &Path) -> Result<()>;
    /// Make this process's cwd the mounted tree, so the command it `exec`s (or
    /// forks) runs in the tree rather than in the directory the mount covers.
    fn enter_mount(&self, mount: &Path) -> Result<()>;
    /// Record what the mounted tree has free, for the host's next auto-grow.
    fn record_free_bytes(&self, mount: &Path, uid: u32, gid: u32) -> Result<()>;
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

    /// The tree's copy of the share's store.
    ///
    /// `context.db` goes through SQLite's own backup API rather than a copy: the
    /// host's agent may hold that database open and be writing to it while this
    /// runs, and a file copy of a live database together with its `-wal` moves a
    /// transaction that is still in flight. `.backup` reads through the store's
    /// locking and writes one consolidated database, so the sidecars are not
    /// copied at all (see `MAGIC_CONTEXT_DB`).
    fn seed_magic_context(&self, from: &Path, to: &Path) -> Result<()> {
        // The store is the agent's private history: the share's copy is a 0700
        // directory with a 0600 database, and the tree's copy keeps that rather
        // than whatever a directory created under `cargo/` gets from this
        // process's umask.
        crate::guest_init::fs::chmod(to, 0o700)?;
        let database = from.join(MAGIC_CONTEXT_DB);
        if database.is_file() {
            let snapshot = to.join(MAGIC_CONTEXT_DB);
            crate::guest_init::command::run(
                "sqlite3",
                &[
                    path_str(&database)?,
                    &format!(".backup {}", path_str(&snapshot)?),
                ],
            )
            .with_context(|| format!("failed to snapshot '{}' for the tree", database.display()))?;
            crate::guest_init::fs::chmod(&snapshot, 0o600)?;
        }
        for entry in std::fs::read_dir(from)
            .with_context(|| format!("failed to read '{}'", from.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read an entry of '{}'", from.display()))?;
            let name = entry.file_name();
            if is_magic_context_database_file(&name.to_string_lossy()) {
                continue;
            }
            let source = entry.path();
            let target = to.join(&name);
            crate::guest_init::command::run(
                "cp",
                &["-a", "-T", path_str(&source)?, path_str(&target)?],
            )
            .with_context(|| format!("failed to copy '{}' into the tree", source.display()))?;
        }
        Ok(())
    }

    fn chown_tree(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
        crate::guest_init::fs::chown_tree_skipping_symlinks(path, uid, gid)
    }

    fn resize_max(&self, mount: &Path) -> Result<()> {
        crate::guest_init::command::run("btrfs", &["filesystem", "resize", "max", path_str(mount)?])
    }

    fn enter_mount(&self, mount: &Path) -> Result<()> {
        Ok(std::env::set_current_dir(mount)?)
    }

    fn record_free_bytes(&self, mount: &Path, uid: u32, gid: u32) -> Result<()> {
        let dir = mount.join(".cang");
        crate::guest_init::fs::create_dir_all(&dir)?;
        crate::guest_init::fs::chown(&dir, uid, gid)?;
        let file = mount.join(FREE_BYTES_FILE);
        std::fs::write(&file, free_bytes_line(mount)?)
            .with_context(|| format!("failed to record free space in '{}'", dir.display()))?;
        // The tree belongs to the developer identity, and so does its own
        // bookkeeping: a marker only root can replace would make `cang exec`'s
        // own reading of the number a dead end.
        crate::guest_init::fs::chown(&file, uid, gid)
    }
}

/// Everything derived from the store's database file: the database itself, its
/// `-wal`/`-shm`, and the `context.db.corrupt-*` dumps magic-context keeps.
fn is_magic_context_database_file(name: &str) -> bool {
    name.starts_with(MAGIC_CONTEXT_DB)
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
        magic_context_empty: bool,
    }

    impl FakeWorkspaceDiskOps {
        fn new() -> Self {
            Self {
                disk: PathBuf::from("/dev/vdb"),
                cargo_home_empty: true,
                magic_context_empty: true,
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
            if path == Path::new(MAGIC_CONTEXT_DIR) {
                Ok(self.magic_context_empty)
            } else {
                Ok(self.cargo_home_empty)
            }
        }

        fn seed_cargo_home(&self, from: &Path, to: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("seed:{}:{}", from.display(), to.display()));
            Ok(())
        }

        fn seed_magic_context(&self, from: &Path, to: &Path) -> Result<()> {
            self.operations.borrow_mut().push(format!(
                "seed-store:{}:{}",
                from.display(),
                to.display()
            ));
            Ok(())
        }

        fn chown_tree(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("chown-tree:{}:{uid}:{gid}", path.display()));
            Ok(())
        }

        fn resize_max(&self, mount: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("resize-max:{}", mount.display()));
            Ok(())
        }

        fn enter_mount(&self, mount: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("chdir:{}", mount.display()));
            Ok(())
        }

        fn record_free_bytes(&self, mount: &Path, _uid: u32, _gid: u32) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("free-bytes:{}", mount.display()));
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
                "resize-max:/workspace",
                "chdir:/workspace",
                "chown:/workspace:1000:1001",
                "mkdir:/workspace/cargo",
                "chown:/workspace/cargo:1000:1001",
                "is_empty:/workspace/cargo",
                "seed:/home/dev/.cargo:/workspace/cargo",
                "chown:/workspace/cargo:1000:1001",
                "findmnt:/home/dev/.cargo",
                "mkdir:/home/dev/.cargo",
                "bind:/workspace/cargo:/home/dev/.cargo",
                "mkdir:/workspace/cargo/magic-context",
                "chown:/workspace/cargo/magic-context:1000:1001",
                "mkdir:/home/dev/.local/share/cortexkit/magic-context",
                "is_empty:/workspace/cargo/magic-context",
                "seed-store:/home/dev/.local/share/cortexkit/magic-context:/workspace/cargo/magic-context",
                "chown-tree:/workspace/cargo/magic-context:1000:1001",
                "bind:/workspace/cargo/magic-context:/home/dev/.local/share/cortexkit/magic-context",
                "free-bytes:/workspace",
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
            [
                "/workspace",
                "/home/dev/.cargo",
                "/home/dev/.local/share/cortexkit/magic-context",
            ],
            "exactly one mount remains at each path, and both are the tree's"
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

    /// The store moves to the tree, and the share keeps serving everything else
    /// (the cortexkit mount itself is not replaced, only this path under it).
    #[test]
    fn the_magic_context_store_is_bound_over_the_share_path() {
        let ops = FakeWorkspaceDiskOps::new();
        ensure_ready_with(&contract(), &identity(), &ops).expect("should mount");

        let operations = ops.operations.borrow();
        let cargo_bind = operations
            .iter()
            .position(|op| op == "bind:/workspace/cargo:/home/dev/.cargo")
            .expect("the cargo home is bound");
        let create = operations
            .iter()
            .position(|op| op == "mkdir:/workspace/cargo/magic-context")
            .expect("the store directory is inside the tree");
        let bind = operations
            .iter()
            .position(|op| {
                op == "bind:/workspace/cargo/magic-context:/home/dev/.local/share/cortexkit/magic-context"
            })
            .expect("the store is bound over the share's copy");
        assert!(cargo_bind < create, "the tree comes first: {operations:?}");
        assert!(create < bind, "the store exists before it is bound");
    }

    #[test]
    fn a_populated_magic_context_store_is_not_seeded_again() {
        let mut ops = FakeWorkspaceDiskOps::new();
        ops.magic_context_empty = false;
        ensure_ready_with(&contract(), &identity(), &ops).expect("should mount");

        let operations = ops.operations.borrow();
        assert!(!operations.iter().any(|op| op.starts_with("seed-store:")));
        assert!(!operations.iter().any(|op| op.starts_with("chown-tree:")));
        assert!(operations.iter().any(|op| {
            op == "bind:/workspace/cargo/magic-context:/home/dev/.local/share/cortexkit/magic-context"
        }));
    }

    /// The snapshot replaces the database and its write-ahead log, so those are
    /// the only store entries never copied - and the corrupt dumps beside them
    /// are the host store's, not the tree's.
    #[test]
    fn the_store_s_database_sidecars_and_dumps_are_not_copied() {
        for name in [
            "context.db",
            "context.db-wal",
            "context.db-shm",
            "context.db.corrupt-backup-20261124T164253330Z",
            "context.db.corrupt-original-20261124T164437051Z",
        ] {
            assert!(is_magic_context_database_file(name), "{name}");
        }
        for name in [
            "models",
            "lkg-recovery-fences",
            "pi-served-array-digests",
            "pi-served-tag-numbers",
            "project-identities",
            "last_announced_version",
        ] {
            assert!(!is_magic_context_database_file(name), "{name}");
        }
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

    /// The mount covers the directory the guest's cwd was set to before this
    /// process started, so entering the tree is a step of its own - and it happens
    /// whether this mount did the mounting or found it already mounted.
    #[test]
    fn the_command_enters_the_mounted_tree_rather_than_the_covered_directory() {
        for already_mounted in [false, true] {
            let ops = FakeWorkspaceDiskOps::new();
            if already_mounted {
                ops.mounted.borrow_mut().push(MOUNT_POINT.to_owned());
            }
            ensure_ready_with(&contract(), &identity(), &ops).expect("should mount");

            let operations = ops.operations.borrow();
            assert_eq!(
                operations
                    .iter()
                    .filter(|op| op.as_str() == "chdir:/workspace")
                    .count(),
                1,
                "{operations:?}"
            );
            let enter = operations
                .iter()
                .position(|op| op == "chdir:/workspace")
                .expect("the tree is entered");
            if let Some(mount) = operations
                .iter()
                .position(|op| op == "mount:/dev/vdb:/workspace")
            {
                assert!(mount < enter, "the mount comes first: {operations:?}");
            }
        }
    }

    /// The marker is one line the host parses, and it carries the tree's real
    /// free and total bytes.
    #[test]
    fn the_free_space_line_carries_the_filesystem_s_free_and_total() {
        let temp = tempfile::tempdir().expect("tempdir");
        let line = free_bytes_line(temp.path()).expect("statvfs");

        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 3, "{line}");
        assert_eq!(fields[0], FREE_BYTES_REPORT_PREFIX);
        let free: u64 = fields[1].parse().expect("free bytes");
        let total: u64 = fields[2].parse().expect("total bytes");
        assert!(total > 0, "{line}");
        assert!(free <= total, "{line}");
    }

    /// A reading maintenance VM mounts the disk, hands back the marker it finds,
    /// and leaves the image alone (read-only, unmounted again).
    #[test]
    fn the_report_reader_mounts_read_only_and_prints_the_marker() {
        let ops = FakeFreeBytesOps::new(Some([FREE_BYTES_REPORT_PREFIX, "512", "8192"].join("\t")));

        report_free_bytes_with(&contract(), &ops).expect("the marker is read");

        assert_eq!(
            ops.operations.borrow().as_slice(),
            [
                "mkdir:/run/cang/workspace-free-bytes",
                "find:CANG_WORKSPACE:cang-workspace",
                "mount-ro:/dev/vdb:/run/cang/workspace-free-bytes",
                "read:/run/cang/workspace-free-bytes/.cang/free-bytes",
                "umount:/run/cang/workspace-free-bytes",
            ]
        );
    }

    /// No marker is not an error: an image whose guest-init predates the note (or
    /// one no guest ever mounted) reports nothing, and the host reads that as
    /// "do not grow".
    #[test]
    fn a_tree_without_a_marker_reports_nothing() {
        let ops = FakeFreeBytesOps::new(None);

        report_free_bytes_with(&contract(), &ops).expect("no marker is fine");

        assert!(
            ops.operations
                .borrow()
                .iter()
                .any(|op| op == "umount:/run/cang/workspace-free-bytes"),
            "the read mount is unmounted either way"
        );
    }

    struct FakeFreeBytesOps {
        marker: Option<String>,
        operations: RefCell<Vec<String>>,
    }

    impl FakeFreeBytesOps {
        fn new(marker: Option<String>) -> Self {
            Self {
                marker,
                operations: RefCell::new(Vec::new()),
            }
        }
    }

    impl FreeBytesOps for FakeFreeBytesOps {
        fn find_disk(&self, label: &str, disk_id: &str) -> Result<PathBuf> {
            self.operations
                .borrow_mut()
                .push(format!("find:{label}:{disk_id}"));
            Ok(PathBuf::from("/dev/vdb"))
        }

        fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("mkdir:{}", path.display()));
            Ok(())
        }

        fn mount_btrfs_read_only(&self, disk: &Path, mount: &Path) -> Result<()> {
            self.operations.borrow_mut().push(format!(
                "mount-ro:{}:{}",
                disk.display(),
                mount.display()
            ));
            Ok(())
        }

        fn read_report(&self, path: &Path) -> Result<Option<String>> {
            self.operations
                .borrow_mut()
                .push(format!("read:{}", path.display()));
            Ok(self.marker.clone())
        }

        fn unmount(&self, mount: &Path) -> Result<()> {
            self.operations
                .borrow_mut()
                .push(format!("umount:{}", mount.display()));
            Ok(())
        }
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
