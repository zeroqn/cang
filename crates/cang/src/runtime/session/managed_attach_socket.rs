//! Host-side libkrun managed attach socket path allocation.

use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const RUNTIME_DIR_MODE: u32 = 0o700;
pub(crate) const LINUX_UNIX_SOCKET_PATH_LIMIT: usize = 107;

pub(crate) fn allocate(task_id: &str, task_dir: &Path) -> Result<PathBuf> {
    allocate_named(task_id, task_dir, "a")
}

pub(crate) fn allocate_exec(task_id: &str, task_dir: &Path) -> Result<PathBuf> {
    allocate_named(task_id, task_dir, "e")
}

pub(crate) fn allocate_pulse_bridge(task_id: &str, task_dir: &Path) -> Result<PathBuf> {
    allocate_named(task_id, task_dir, "pulse")
}

/// The unix socket libkrun binds for the guest's workspace relay listener.
pub(crate) fn allocate_workspace_relay(task_id: &str, task_dir: &Path) -> Result<PathBuf> {
    allocate_named(task_id, task_dir, "wr")
}

pub(crate) fn allocate_waypipe_data(task_id: &str, task_dir: &Path) -> Result<PathBuf> {
    allocate_named(task_id, task_dir, "wd")
}

pub(crate) fn allocate_waypipe_control(task_id: &str, task_dir: &Path) -> Result<PathBuf> {
    allocate_named(task_id, task_dir, "wc")
}

fn allocate_named(task_id: &str, task_dir: &Path, prefix: &str) -> Result<PathBuf> {
    let uid = current_uid();
    allocate_in_runtime_parent(Path::new("/tmp"), uid, task_id, task_dir, prefix)
}

fn allocate_in_runtime_parent(
    runtime_parent: &Path,
    uid: u32,
    task_id: &str,
    task_dir: &Path,
    prefix: &str,
) -> Result<PathBuf> {
    let runtime_dir = runtime_parent.join(format!("cang-{uid}"));
    ensure_owner_only_runtime_dir(&runtime_dir, uid)?;
    let socket_path = runtime_dir.join(format!(
        "{prefix}-{:016x}.sock",
        task_hash(task_id, task_dir)
    ));
    validate_socket_path_budget(&socket_path)?;
    remove_stale_socket_at_exact_path(&socket_path)?;
    Ok(socket_path)
}

fn ensure_owner_only_runtime_dir(path: &Path, uid: u32) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to create cang runtime dir '{}'", path.display())
            });
        }
    }

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat cang runtime dir '{}'", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "cang runtime dir '{}' must not be a symlink",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "cang runtime dir '{}' exists but is not a directory",
            path.display()
        );
    }
    if metadata.uid() != uid {
        bail!(
            "cang runtime dir '{}' is owned by uid {}, expected {}",
            path.display(),
            metadata.uid(),
            uid
        );
    }

    let mode = metadata.permissions().mode() & 0o777;
    if mode != RUNTIME_DIR_MODE {
        chmod_no_follow(path, RUNTIME_DIR_MODE).with_context(|| {
            format!(
                "failed to repair cang runtime dir '{}' mode to {:o}",
                path.display(),
                RUNTIME_DIR_MODE
            )
        })?;
    }
    Ok(())
}

fn validate_socket_path_budget(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_bytes().len();
    if bytes > LINUX_UNIX_SOCKET_PATH_LIMIT {
        bail!(
            "allocated cang attach socket path '{}' is {bytes} bytes, exceeding Linux Unix socket pathname limit of {LINUX_UNIX_SOCKET_PATH_LIMIT} bytes",
            path.display()
        );
    }
    Ok(())
}

fn remove_stale_socket_at_exact_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to stat cang attach socket '{}'", path.display())
            });
        }
    };
    if !metadata.file_type().is_socket() {
        return Err(anyhow!(
            "cang attach socket path '{}' already exists but is not a Unix socket",
            path.display()
        ));
    }
    fs::remove_file(path).with_context(|| {
        format!(
            "failed to remove stale cang attach socket '{}'",
            path.display()
        )
    })
}

fn chmod_no_follow(path: &Path, mode: u32) -> std::io::Result<()> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: path is a valid NUL-terminated C string; AT_SYMLINK_NOFOLLOW keeps
    // permission repair from following a replaced symlink in the shared tmp tree.
    let rc = unsafe {
        libc::fchmodat(
            libc::AT_FDCWD,
            path.as_ptr(),
            mode as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn task_hash(task_id: &str, task_dir: &Path) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in task_id
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain(task_dir.as_os_str().as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use tempfile::tempdir;

    #[test]
    fn allocates_short_socket_path_under_owner_runtime_dir() {
        let temp = tempdir().unwrap();
        let deep_task_dir = temp
            .path()
            .join("x".repeat(180))
            .join("tasks")
            .join("task-a");

        let socket =
            allocate_in_runtime_parent(temp.path(), current_uid(), "task-a", &deep_task_dir, "a")
                .expect("allocate socket path");

        assert_eq!(
            socket.parent().unwrap(),
            temp.path().join(format!("cang-{}", current_uid()))
        );
        assert!(socket.file_name().unwrap().as_bytes().starts_with(b"a-"));
        assert!(socket.as_os_str().as_bytes().len() <= LINUX_UNIX_SOCKET_PATH_LIMIT);
        let mode = fs::symlink_metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, RUNTIME_DIR_MODE);
    }

    #[test]
    fn the_workspace_relay_socket_has_its_own_prefix() {
        let temp = tempdir().unwrap();
        let socket = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "wr",
        )
        .expect("allocate relay socket path");
        assert!(
            socket.file_name().unwrap().as_bytes().starts_with(b"wr-"),
            "{}",
            socket.display()
        );
    }

    #[test]
    fn rejects_runtime_dir_symlink() {
        let temp = tempdir().unwrap();
        let runtime_dir = temp.path().join(format!("cang-{}", current_uid()));
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &runtime_dir).unwrap();

        let err = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("must not be a symlink"));
    }

    #[test]
    fn rejects_runtime_dir_non_directory() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join(format!("cang-{}", current_uid())),
            b"not a dir",
        )
        .unwrap();

        let err = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("is not a directory"));
    }

    #[test]
    fn repairs_owner_runtime_dir_mode() {
        let temp = tempdir().unwrap();
        let runtime_dir = temp.path().join(format!("cang-{}", current_uid()));
        fs::create_dir(&runtime_dir).unwrap();
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o755)).unwrap();

        allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .expect("owner-safe mode repair");

        let mode = fs::symlink_metadata(runtime_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, RUNTIME_DIR_MODE);
    }

    #[test]
    fn removes_only_exact_stale_socket_path() {
        let temp = tempdir().unwrap();
        let runtime_dir = temp.path().join(format!("cang-{}", current_uid()));
        fs::create_dir(&runtime_dir).unwrap();
        let stale = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .expect("initial allocate");
        let unrelated = runtime_dir.join("unrelated.sock");
        let stale_listener = UnixListener::bind(&stale).unwrap();
        let unrelated_listener = UnixListener::bind(&unrelated).unwrap();

        drop(stale_listener);
        let allocated = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .expect("reallocate");

        assert_eq!(allocated, stale);
        assert!(!stale.exists());
        assert!(unrelated.exists());
        drop(unrelated_listener);
    }

    #[test]
    fn rejects_existing_non_socket_leaf() {
        let temp = tempdir().unwrap();
        let socket = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .expect("initial allocate");
        fs::write(&socket, b"not a socket").unwrap();

        let err = allocate_in_runtime_parent(
            temp.path(),
            current_uid(),
            "task-a",
            Path::new("/task"),
            "a",
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("already exists but is not a Unix socket"));
    }
}
