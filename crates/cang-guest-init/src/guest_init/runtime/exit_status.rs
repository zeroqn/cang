//! Reporting the payload's exit status to the VMM.
//!
//! cang's libkrun fork intercepts the virtio-fs ioctl `VIRTIO_IOC_EXIT_CODE_REQ`
//! (0x7602) before the request reaches the host filesystem server and stores the
//! argument as the VM's own exit status
//! (`deps/libkrun/src/devices/src/virtio/fs/augment_fs.rs`), which the VMM then
//! prefers over the vCPU's default when it stops
//! (`deps/libkrun/src/libkrun/src/vmm/mod.rs`). Nothing in the guest ever sent
//! it, so the payload's status stayed inside the guest: `cang -- false` exited 0.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// The fork's exit-status ioctl request number.
pub(in crate::guest_init) const VIRTIO_IOC_EXIT_CODE_REQ: libc::Ioctl = 0x7602;

/// The virtio-fs share that carries the ioctl to the host-side server.
const WORKSPACE_MOUNT: &str = "/workspace";
/// The fallback mount, used when the workspace share is not mounted at all.
const ROOT_MOUNT: &str = "/";
const GUEST_DEBUG_ENV: &str = "CANG_GUEST_DEBUG";

/// Report `code` as the VM's exit status through the workspace share.
pub(in crate::guest_init) fn report(code: i32) -> io::Result<()> {
    let file = match File::open(WORKSPACE_MOUNT) {
        Ok(file) => file,
        Err(workspace_error) => {
            // Without the workspace share there is no passthrough filesystem to
            // carry the request: the rootfs is a plain block device. Reporting is
            // best effort, so fall back to the root mount and keep the workspace
            // error for the caller's diagnostics.
            File::open(ROOT_MOUNT).map_err(|_| workspace_error)?
        }
    };
    report_via(&file, code)
}

fn report_via(file: &File, code: i32) -> io::Result<()> {
    // SAFETY: the request is issued on an open directory fd; the fork's
    // virtio-fs server consumes this request number before any host filesystem
    // ioctl is attempted, and the argument is the caller's exit status.
    let rc = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            VIRTIO_IOC_EXIT_CODE_REQ,
            code as libc::c_ulong,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Report `code` and end this process with it.
pub(in crate::guest_init) fn report_and_exit(code: i32) -> ! {
    if let Err(err) = report(code)
        && guest_debug_enabled()
    {
        eprintln!("cang-guest-init: debug: exit status report failed: {err}");
    }
    std::process::exit(code)
}

/// Map a `waitpid` status onto an exit status.
///
/// A signal death becomes `128 + signal`, so a payload the guest killed (SIGTERM
/// from `cang kill`, SIGSEGV from a crash) reports a non-zero status instead of a
/// silent success.
pub(in crate::guest_init) fn wait_status_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        return libc::WEXITSTATUS(status);
    }
    if libc::WIFSIGNALED(status) {
        return 128 + libc::WTERMSIG(status);
    }
    1
}

fn guest_debug_enabled() -> bool {
    std::env::var(GUEST_DEBUG_ENV).ok().as_deref() == Some("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `waitpid` status for a child that exited with `code`.
    fn exited(code: i32) -> i32 {
        code << 8
    }

    /// A `waitpid` status for a child killed by `signal`.
    fn signaled(signal: i32) -> i32 {
        signal
    }

    #[test]
    fn exit_status_request_matches_the_libkrun_fork_ioctl() {
        assert_eq!(VIRTIO_IOC_EXIT_CODE_REQ, 0x7602);
    }

    #[test]
    fn wait_status_code_preserves_a_normal_exit_code() {
        assert_eq!(wait_status_code(exited(0)), 0);
        assert_eq!(wait_status_code(exited(3)), 3);
        assert_eq!(wait_status_code(exited(255)), 255);
    }

    #[test]
    fn wait_status_code_maps_signal_death_to_a_nonzero_status() {
        assert_eq!(
            wait_status_code(signaled(libc::SIGTERM)),
            128 + libc::SIGTERM
        );
        assert_eq!(
            wait_status_code(signaled(libc::SIGSEGV)),
            128 + libc::SIGSEGV
        );
    }

    #[test]
    fn report_is_rejected_by_a_directory_that_is_not_a_virtio_fs_share() {
        let file = File::open(std::env::temp_dir()).expect("open temp dir");
        assert!(report_via(&file, 3).is_err());
    }
}
