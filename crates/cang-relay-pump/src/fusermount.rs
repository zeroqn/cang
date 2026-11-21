//! Resolving and running libfuse's `fusermount3`, which performs the privileged
//! mount for an unprivileged user.
//!
//! cang never mounts as root: `fusermount3` is the setuid helper that does the
//! `mount(2)` and hands back a `/dev/fuse` descriptor. On a NixOS host that
//! helper is the `security.wrappers` one at `/run/wrappers/bin/fusermount3`; a
//! distribution host has `/usr/bin/fusermount3` on `PATH`; `CANG_FUSERMOUNT3`
//! overrides both, which is how a test or a measurement arm pins a specific
//! build.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const FUSERMOUNT_ENV: &str = "CANG_FUSERMOUNT3";
const NIXOS_WRAPPER: &str = "/run/wrappers/bin/fusermount3";
const PROGRAM_NAME: &str = "fusermount3";

/// The `fusermount3` this process should use.
///
/// Precedence: `CANG_FUSERMOUNT3`, then the NixOS setuid wrapper (if it
/// exists), then the bare program name for `PATH` lookup.
pub fn program() -> OsString {
    program_from(
        std::env::var_os(FUSERMOUNT_ENV),
        Path::new(NIXOS_WRAPPER).is_file(),
    )
}

fn program_from(override_value: Option<OsString>, nixos_wrapper_present: bool) -> OsString {
    if let Some(value) = override_value.filter(|value| !value.as_os_str().is_empty()) {
        return value;
    }
    if nixos_wrapper_present {
        return OsString::from(NIXOS_WRAPPER);
    }
    OsString::from(PROGRAM_NAME)
}

/// Unmount `mountpoint` with `fusermount3 -u`.
///
/// This is the *only* path that unmounts: cang runs it when its own process is
/// done with the mount (ticket 23 decision 4(a) - a serving guest's death keeps
/// the mount and answers `EIO`; it never unmounts it).
pub fn unmount(program: &OsString, mountpoint: &Path) -> Result<()> {
    let output = Command::new(program)
        .arg("-u")
        .arg(mountpoint)
        .output()
        .with_context(|| format!("failed to run {:?} -u", program))?;
    if !output.status.success() {
        anyhow::bail!(
            "{:?} -u {} failed ({}): {}",
            program,
            mountpoint.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Is `mountpoint` currently a FUSE mount?
///
/// Readiness and teardown both ask this instead of trusting a log line: the
/// kernel's own answer is `statfs(2)`'s `f_type` being `FUSE_SUPER_MAGIC`
/// (`0x65735546`).
pub fn is_fuse_mount(mountpoint: &Path) -> bool {
    fuse_mount_type(mountpoint) == Some(FUSE_SUPER_MAGIC)
}

fn fuse_mount_type(mountpoint: &Path) -> Option<i64> {
    let path = std::ffi::CString::new(mountpoint.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    Some(stat.f_type)
}

/// The value `statfs(2)` reports for a FUSE superblock.
pub const FUSE_SUPER_MAGIC: i64 = 0x6573_5546;

/// The path a `fusermount3` lookup would use, for the named error lines.
pub fn describe(program: &OsString) -> PathBuf {
    PathBuf::from(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_override_wins_over_the_nixos_wrapper() {
        assert_eq!(
            program_from(Some(OsString::from("/custom/fusermount3")), true),
            OsString::from("/custom/fusermount3")
        );
    }

    #[test]
    fn the_nixos_wrapper_is_preferred_over_a_path_lookup() {
        assert_eq!(
            program_from(None, true),
            OsString::from("/run/wrappers/bin/fusermount3")
        );
        assert_eq!(program_from(None, false), OsString::from("fusermount3"));
    }

    #[test]
    fn an_empty_override_is_ignored() {
        assert_eq!(
            program_from(Some(OsString::new()), false),
            OsString::from("fusermount3")
        );
    }

    #[test]
    fn a_plain_directory_is_not_a_fuse_mount() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_fuse_mount(dir.path()));
        // And an absent path is not one either, rather than an error to handle.
        assert!(!is_fuse_mount(Path::new("/nonexistent/cang/relay")));
    }

    #[test]
    fn unmounting_a_plain_directory_fails_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No fusermount3 in the test sandbox: the failure must be a named error,
        // not a panic or a silent success.
        let err = unmount(
            &OsString::from("cang-fusermount3-that-does-not-exist"),
            dir.path(),
        )
        .expect_err("a missing helper must fail");
        assert!(format!("{err:#}").contains("failed to run"), "{err:#}");
    }
}
