//! libfuse's `fuse_mount()` handshake, reproduced: a socket pair, the helper
//! inheriting one end as fd 3 with `_FUSE_COMMFD=3`, `fusermount3` exec'ing
//! setuid and performing the mount, and the `/dev/fuse` descriptor arriving back
//! over `SCM_RIGHTS`.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::os::unix::io::RawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// The environment variable libfuse's mount helper convention uses.
pub const FUSE_COMMFD_ENV: &str = "_FUSE_COMMFD";
/// The descriptor number the helper expects to find the socket on.
pub const FUSE_COMMFD: RawFd = 3;

/// An open `/dev/fuse` connection owned by this process.
#[derive(Debug)]
pub struct FuseMount {
    fd: RawFd,
}

impl FuseMount {
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Take the descriptor out, transferring ownership to the caller.
    pub fn into_fd(self) -> RawFd {
        let fd = self.fd;
        std::mem::forget(self);
        fd
    }
}

impl Drop for FuseMount {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.fd) };
    }
}

/// Mount `mountpoint` through `fusermount3` and return the FUSE connection.
///
/// `options` is the `-o` argument verbatim (`fsname=cang-workspace` for cang's
/// workspace mount, plus nothing else: no `default_permissions` - the kernel
/// would then check mode bits against the *host* uid - and no `auto_unmount`,
/// because the contract wants a stale mount to stay).
pub fn mount(program: &OsString, mountpoint: &Path, options: &str) -> Result<FuseMount> {
    if !mountpoint.is_dir() {
        bail!(
            "workspace relay mount point '{}' is not a directory",
            mountpoint.display()
        );
    }
    let (parent_end, child_end) = socketpair()?;
    let mut command = helper_command(program, mountpoint, options, child_end);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            close_fd(parent_end);
            close_fd(child_end);
            return Err(err).with_context(|| format!("failed to run {program:?}"));
        }
    };
    // The parent keeps only its own end; the child has the other one and dies
    // with it after a successful handshake.
    close_fd(child_end);

    let fd = recv_fd(parent_end);
    close_fd(parent_end);
    let status = child.wait().context("failed to reap fusermount3")?;
    let fd = fd.with_context(|| {
        format!(
            "fusermount3 did not hand back a /dev/fuse descriptor ({status}); \
             expected {}={FUSE_COMMFD} to be inherited",
            FUSE_COMMFD_ENV
        )
    })?;
    if !status.success() {
        close_fd(fd);
        bail!("fusermount3 exited with {status} after handing back a descriptor");
    }
    Ok(FuseMount { fd })
}

/// The helper invocation, kept separate so its shape is testable without a
/// setuid helper.
pub fn helper_command(
    program: &OsString,
    mountpoint: &Path,
    options: &str,
    child_end: RawFd,
) -> Command {
    let mut command = Command::new(program);
    command
        .arg("-o")
        .arg(options)
        .arg("--")
        .arg(mountpoint)
        .env(FUSE_COMMFD_ENV, FUSE_COMMFD.to_string())
        .stdin(std::process::Stdio::null());
    // SAFETY: this pre-exec hook only calls dup2/close on an inherited
    // descriptor, both async-signal-safe, and does not allocate.
    unsafe {
        command.pre_exec(move || {
            if child_end != FUSE_COMMFD {
                if libc::dup2(child_end, FUSE_COMMFD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(child_end);
            }
            // The descriptor must survive exec: fusermount3 writes the fd back
            // on it after the mount succeeds.
            let flags = libc::fcntl(FUSE_COMMFD, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(FUSE_COMMFD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            Ok(())
        });
    }
    command
}

/// Receive one descriptor over a unix socket (`SCM_RIGHTS`).
pub fn recv_fd(sock: RawFd) -> Result<RawFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: byte.len(),
    };
    let mut control = [0u8; 64];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    loop {
        let received = unsafe { libc::recvmsg(sock, &mut message, 0) };
        if received < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err).context("failed to receive the /dev/fuse descriptor");
        }
        if received == 0 {
            bail!("fusermount3 closed the handshake socket without sending a descriptor");
        }
        break;
    }
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if header.is_null() {
        bail!("fusermount3 sent no control message; expected SCM_RIGHTS");
    }
    let header_ref = unsafe { &*header };
    if header_ref.cmsg_level != libc::SOL_SOCKET || header_ref.cmsg_type != libc::SCM_RIGHTS {
        bail!(
            "fusermount3 control message was level {} type {}, expected SOL_SOCKET/SCM_RIGHTS",
            header_ref.cmsg_level,
            header_ref.cmsg_type
        );
    }
    let mut fd: RawFd = -1;
    unsafe {
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(header),
            (&mut fd as *mut RawFd).cast(),
            std::mem::size_of::<RawFd>(),
        );
    }
    if fd < 0 {
        bail!("fusermount3 sent an invalid descriptor ({fd})");
    }
    Ok(fd)
}

fn socketpair() -> Result<(RawFd, RawFd)> {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to create the fusermount3 handshake socket pair");
    }
    Ok((fds[0], fds[1]))
}

fn close_fd(fd: RawFd) {
    if fd >= 0 {
        let _ = unsafe { libc::close(fd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn send_fd(sock: RawFd, fd: RawFd) {
        let mut byte = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: byte.len(),
        };
        let mut control = [0u8; 64];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as _;
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
            std::ptr::copy_nonoverlapping(
                (&fd as *const RawFd).cast::<u8>(),
                libc::CMSG_DATA(header),
                std::mem::size_of::<RawFd>(),
            );
        }
        let sent = unsafe { libc::sendmsg(sock, &message, 0) };
        assert!(
            sent >= 0,
            "sendmsg failed: {}",
            std::io::Error::last_os_error()
        );
    }

    /// The receive half of the handshake, against a real `SCM_RIGHTS` sender.
    #[test]
    fn a_descriptor_arrives_over_scm_rights() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("payload");
        std::fs::write(&path, b"relay").expect("file");
        let file = std::fs::File::open(&path).expect("open");
        let (a, b) = socketpair().expect("socketpair");

        send_fd(a, std::os::unix::io::AsRawFd::as_raw_fd(&file));
        let received = recv_fd(b).expect("descriptor should arrive");
        close_fd(a);
        close_fd(b);

        let mut stat_sent: libc::stat = unsafe { std::mem::zeroed() };
        let mut stat_received: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::fstat(std::os::unix::io::AsRawFd::as_raw_fd(&file), &mut stat_sent) },
            0
        );
        assert_eq!(unsafe { libc::fstat(received, &mut stat_received) }, 0);
        // Same inode: the descriptor we received is the file we sent.
        assert_eq!(stat_sent.st_ino, stat_received.st_ino);
        let metadata = std::fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.ino(), stat_received.st_ino);
        close_fd(received);
    }

    #[test]
    fn a_closed_handshake_socket_is_a_named_error() {
        let (a, b) = socketpair().expect("socketpair");
        close_fd(a);
        let err = recv_fd(b).expect_err("a closed socket must fail");
        close_fd(b);
        assert!(
            format!("{err:#}").contains("closed the handshake socket"),
            "{err:#}"
        );
    }

    #[test]
    fn the_helper_command_carries_the_libfuse_convention() {
        let command = helper_command(
            &OsString::from("/run/wrappers/bin/fusermount3"),
            Path::new("/tmp/relay-mnt"),
            "fsname=cang-workspace",
            7,
        );
        let program = command.get_program().to_string_lossy().to_string();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(program, "/run/wrappers/bin/fusermount3");
        assert_eq!(
            args,
            ["-o", "fsname=cang-workspace", "--", "/tmp/relay-mnt"]
        );
        let commfd = command
            .get_envs()
            .find(|(key, _)| key.to_string_lossy() == FUSE_COMMFD_ENV)
            .map(|(_, value)| value.map(|value| value.to_string_lossy().to_string()));
        assert_eq!(commfd, Some(Some("3".to_owned())));
    }

    /// A real mount requires a setuid helper, so the mount itself is exercised
    /// live (and by the pump's own readiness check), not here; what is testable
    /// without one is the refusal for a mount point that cannot be mounted.
    #[test]
    fn a_file_is_not_a_mountable_point() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").expect("file");
        let err = mount(
            &OsString::from("fusermount3"),
            &file,
            "fsname=cang-workspace",
        )
        .expect_err("a file must be refused");
        assert!(format!("{err:#}").contains("not a directory"), "{err:#}");
    }
}
