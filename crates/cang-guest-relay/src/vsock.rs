//! The in-guest `AF_VSOCK` listener the host pump dials through libkrun's vsock
//! unix port (`listen = true` is the reverse-share direction: the host initiates
//! the connection, the guest only ever `bind`s/`accept`s).

use anyhow::{Context, Result};
use std::fs::File;
use std::os::fd::{FromRawFd, RawFd};

pub struct VsockListener {
    fd: RawFd,
}

impl VsockListener {
    pub fn bind(port: u32) -> Result<Self> {
        if port == 0 {
            anyhow::bail!("workspace relay port must be nonzero");
        }
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to create the workspace relay's AF_VSOCK socket");
        }
        let listener = Self { fd };
        let address = libc::sockaddr_vm {
            svm_family: libc::AF_VSOCK as libc::sa_family_t,
            svm_reserved1: 0,
            svm_port: port,
            svm_cid: libc::VMADDR_CID_ANY,
            svm_zero: [0; 4],
        };
        let rc = unsafe {
            libc::bind(
                listener.fd,
                (&address as *const libc::sockaddr_vm).cast::<libc::sockaddr>(),
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("failed to bind the workspace relay on vsock port {port}")
            });
        }
        if unsafe { libc::listen(listener.fd, 8) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to listen on the workspace relay's AF_VSOCK socket");
        }
        Ok(listener)
    }

    /// Block until the host pump connects, then hand back the stream.
    ///
    /// A signal interrupts the wait (`EINTR`); the caller retries, so a shell
    /// that stops the relay does not end it.
    pub fn accept(&self) -> Result<File> {
        loop {
            let fd = unsafe { libc::accept(self.fd, std::ptr::null_mut(), std::ptr::null_mut()) };
            if fd >= 0 {
                return Ok(unsafe { File::from_raw_fd(fd) });
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err).context("failed to accept a workspace relay connection");
        }
    }
}

impl Drop for VsockListener {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.fd) };
    }
}
