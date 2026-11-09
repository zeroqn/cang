//! The keep-id user namespace a maintenance VM runs in.
//!
//! Preparing the root (bind mounts), mounting a disk and starting `libkrun` are all
//! privileged operations that a rootless caller can only perform inside a user
//! namespace. A task VM's worker gets one from the session supervisor; a maintenance
//! command is an ordinary CLI invocation, so it wraps its own VM step instead -
//! `unshare(1)` with this user's uid mapped to 0, a private mount namespace, and the
//! subordinate ID ranges mapped too, because the rootfs snapshot the guest boots from
//! is owned by IDs from those ranges.
//!
//! Only the VM step is wrapped. The rootfs materialization in front of it is a
//! rootless Buildah snapshot, which sets up a namespace of its own and cannot run
//! inside cang's (Buildah would look for *root*'s subordinate ranges and find none).

use anyhow::Result;
use std::process::Command;

use crate::runtime::session::supervisor::identity::{KeepIdLauncher, UNSHARE_PROGRAM};

/// Wrap `command` so it runs inside the keep-id user namespace, unless this process
/// is already root or already in a namespace of its own.
pub(super) fn wrap(command: &mut Command) -> Result<()> {
    if current_uid() == 0 || in_user_namespace() {
        return Ok(());
    }
    let launcher = KeepIdLauncher::from_current_system()?;
    let program = command.get_program().to_owned();
    let args: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
    let mut wrapped = Command::new(UNSHARE_PROGRAM);
    wrapped
        .args(launcher.namespace_args())
        .arg(program)
        .args(args);
    *command = wrapped;
    Ok(())
}

/// True when this process runs in a user namespace of its own, which is the case
/// exactly when its user namespace is not pid 1's.
///
/// `/proc/self/uid_map` cannot answer this: the initial namespace of a host maps
/// `0 0 4294967295`, one line, just as a keep-id namespace maps one line. The
/// namespace's own inode can, and two paths into one namespace share it.
fn in_user_namespace() -> bool {
    same_namespace("/proc/self/ns/user", "/proc/1/ns/user") == Some(false)
}

fn same_namespace(left: &str, right: &str) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let left = std::fs::metadata(left).ok()?;
    let right = std::fs::metadata(right).ok()?;
    Some(left.ino() == right.ino() && left.dev() == right.dev())
}

fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_namespace_test_compares_namespace_identity_not_its_contents() {
        // Two paths into one namespace are that namespace...
        assert_eq!(
            super::same_namespace("/proc/self/ns/user", "/proc/self/ns/user"),
            Some(true)
        );
        // ...and another namespace is another, whatever its map says.
        assert_eq!(
            super::same_namespace("/proc/self/ns/user", "/proc/self/ns/mnt"),
            Some(false)
        );
        assert_eq!(
            super::same_namespace("/proc/does-not-exist", "/proc/self/ns/user"),
            None
        );
    }
}
