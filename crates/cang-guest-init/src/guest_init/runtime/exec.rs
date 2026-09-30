use anyhow::{Context, Result, bail};
use cang_exec_protocol::{Frame, PROTOCOL_VERSION, WaypipeAction, read_frame, write_frame};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::guest_init::components::env::GuestPermissions;
use crate::guest_init::components::home::identity::DevIdentity;
use crate::guest_init::components::waypipe::WaypipeService;
use crate::guest_init::process;
use crate::guest_init::runtime::vsock::VsockListener;

const IO_BUF_SIZE: usize = 16 * 1024;
const POLL_TIMEOUT_MS: i32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) struct ExecConfig {
    pub(in crate::guest_init) port: u32,
    pub(in crate::guest_init) protocol_version: u16,
}

pub(in crate::guest_init) fn start(
    config: ExecConfig,
    identity: DevIdentity,
    permissions: GuestPermissions,
    waypipe: Option<WaypipeService>,
) -> Result<thread::JoinHandle<()>> {
    if config.protocol_version != PROTOCOL_VERSION {
        bail!(
            "cang exec protocol version mismatch: guest supports {PROTOCOL_VERSION}, host requested {}",
            config.protocol_version
        );
    }
    let listener = VsockListener::bind(config.port)?;
    Ok(thread::spawn(move || {
        loop {
            let client = match listener.accept() {
                Ok(client) => client,
                Err(err) => {
                    eprintln!("cang-guest-init: exec listener failed: {err:#}");
                    std::process::exit(1);
                }
            };
            let identity = identity.clone();
            let waypipe = waypipe.clone();
            thread::spawn(move || {
                if let Err(err) = handle_client(
                    client,
                    &identity,
                    permissions,
                    Path::new("/workspace"),
                    waypipe.as_ref(),
                ) {
                    eprintln!("cang-guest-init: exec request failed: {err:#}");
                }
            });
        }
    }))
}

fn handle_client(
    mut client: std::fs::File,
    identity: &DevIdentity,
    permissions: GuestPermissions,
    workdir: &Path,
    waypipe: Option<&WaypipeService>,
) -> Result<()> {
    match read_frame(&mut client)? {
        Some(Frame::Hello { version }) if version == PROTOCOL_VERSION => {
            write_frame(&mut client, &Frame::Ready)?;
        }
        Some(Frame::Hello { version }) => {
            write_frame(
                &mut client,
                &Frame::Error(format!(
                    "cang exec protocol version mismatch: guest supports {PROTOCOL_VERSION}, host requested {version}"
                )),
            )?;
            return Ok(());
        }
        Some(frame) => bail!("expected exec hello, got {frame:?}"),
        None => return Ok(()),
    }
    let (argv, waypipe_action) = match read_frame(&mut client)? {
        Some(Frame::Start { argv, waypipe }) => (argv, waypipe),
        Some(frame) => bail!("expected exec start, got {frame:?}"),
        None => return Ok(()),
    };
    let waypipe_result = match (waypipe_action, waypipe) {
        (WaypipeAction::Disabled, _) => Ok(()),
        (WaypipeAction::Reuse, Some(service)) => service.reuse(),
        (WaypipeAction::Replace, Some(service)) => service.replace(),
        (WaypipeAction::Reuse | WaypipeAction::Replace, None) => Err(anyhow::anyhow!(
            "cang task does not have Waypipe capability"
        )),
    };
    if let Err(err) = waypipe_result {
        write_frame(
            &mut client,
            &Frame::Error(format!("Waypipe activation failed: {err:#}")),
        )?;
        return Ok(());
    }

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let identity = identity.clone();
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if process::is_root() {
                process::apply_dev_credentials(&identity, permissions)?;
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            write_frame(
                &mut client,
                &Frame::Error(format!("failed to start command: {err}")),
            )?;
            return Ok(());
        }
    };
    let pgid = child.id() as libc::pid_t;
    let mut child_stdin = Some(child.stdin.take().expect("piped stdin"));
    let mut child_stdout = child.stdout.take().expect("piped stdout");
    let mut child_stderr = child.stderr.take().expect("piped stderr");
    set_nonblocking(child_stdout.as_raw_fd())?;
    set_nonblocking(child_stderr.as_raw_fd())?;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut buffer = vec![0; IO_BUF_SIZE];

    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for exec command")?
        {
            drain_output(&mut child_stdout, &mut client, true)?;
            drain_output(&mut child_stderr, &mut client, false)?;
            let code = status.code().unwrap_or(128 + status.signal().unwrap_or(1));
            write_frame(&mut client, &Frame::Exit { code })?;
            return Ok(());
        }

        let mut fds = [
            libc::pollfd {
                fd: client.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if stdout_open {
                    child_stdout.as_raw_fd()
                } else {
                    -1
                },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if stderr_open {
                    child_stderr.as_raw_fd()
                } else {
                    -1
                },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_TIMEOUT_MS) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("exec client poll failed");
        }

        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match read_frame(&mut client)? {
                Some(Frame::Stdin(data)) => {
                    if let Some(stdin) = child_stdin.as_mut() {
                        stdin.write_all(&data)?;
                    }
                }
                Some(Frame::StdinEof) => {
                    child_stdin.take();
                }
                Some(Frame::Signal { signal }) => signal_process_group(pgid, signal)?,
                Some(frame) => bail!("unexpected host exec frame: {frame:?}"),
                None => {
                    terminate_and_reap(&mut child, pgid)?;
                    return Ok(());
                }
            }
        }
        if stdout_open && fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            stdout_open = forward_output(&mut child_stdout, &mut client, true, &mut buffer)?;
        }
        if stderr_open && fds[2].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            stderr_open = forward_output(&mut child_stderr, &mut client, false, &mut buffer)?;
        }
    }
}

fn forward_output(
    reader: &mut impl Read,
    client: &mut impl Write,
    stdout: bool,
    buffer: &mut [u8],
) -> Result<bool> {
    match reader.read(buffer) {
        Ok(0) => Ok(false),
        Ok(read) => {
            let frame = if stdout {
                Frame::Stdout(buffer[..read].to_vec())
            } else {
                Frame::Stderr(buffer[..read].to_vec())
            };
            write_frame(client, &frame)?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(err) => Err(err).context("failed to read exec child output"),
    }
}

fn drain_output(reader: &mut impl Read, client: &mut impl Write, stdout: bool) -> Result<()> {
    let mut buffer = vec![0; IO_BUF_SIZE];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                let frame = if stdout {
                    Frame::Stdout(buffer[..read].to_vec())
                } else {
                    Frame::Stderr(buffer[..read].to_vec())
                };
                write_frame(client, &frame)?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(err) => return Err(err).context("failed to drain exec child output"),
        }
    }
}

fn signal_process_group(pgid: libc::pid_t, signal: i32) -> Result<()> {
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).context("failed to signal exec process group")
}

fn terminate_and_reap(child: &mut std::process::Child, pgid: libc::pid_t) -> Result<()> {
    signal_process_group(pgid, libc::SIGTERM)?;
    for _ in 0..25 {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    signal_process_group(pgid, libc::SIGKILL)?;
    child.wait()?;
    Ok(())
}

fn set_nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to make exec child output nonblocking");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    fn identity() -> DevIdentity {
        DevIdentity::new(
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
            PathBuf::from("/bin/sh"),
        )
    }

    #[test]
    fn exec_rejects_waypipe_reuse_without_task_capability() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let client = unsafe { std::fs::File::from_raw_fd(client.into_raw_fd()) };
        let temp = tempfile::tempdir().unwrap();
        let thread = thread::spawn(move || {
            handle_client(client, &identity(), Default::default(), temp.path(), None)
        });

        write_frame(
            &mut server,
            &Frame::Hello {
                version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Ready));
        write_frame(
            &mut server,
            &Frame::Start {
                argv: vec!["/bin/true".into()],
                waypipe: WaypipeAction::Reuse,
            },
        )
        .unwrap();

        let frame = read_frame(&mut server).unwrap().unwrap();
        assert!(
            matches!(frame, Frame::Error(message) if message.contains("does not have Waypipe capability"))
        );
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn exec_reports_startup_error_without_ending_connection_abruptly() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let client = unsafe { std::fs::File::from_raw_fd(client.into_raw_fd()) };
        let temp = tempfile::tempdir().unwrap();
        let thread = thread::spawn(move || {
            handle_client(client, &identity(), Default::default(), temp.path(), None)
        });

        write_frame(
            &mut server,
            &Frame::Hello {
                version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Ready));
        write_frame(
            &mut server,
            &Frame::Start {
                argv: vec!["/definitely/missing-cang-exec-command".into()],
                waypipe: WaypipeAction::Disabled,
            },
        )
        .unwrap();
        let frame = read_frame(&mut server).unwrap().unwrap();
        assert!(matches!(frame, Frame::Error(message) if message.contains("failed to start")));
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn exec_preserves_stdout_stderr_and_exit_status() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let client = unsafe { std::fs::File::from_raw_fd(client.into_raw_fd()) };
        let temp = tempfile::tempdir().unwrap();
        let thread = thread::spawn(move || {
            handle_client(client, &identity(), Default::default(), temp.path(), None)
        });

        write_frame(
            &mut server,
            &Frame::Hello {
                version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Ready));
        write_frame(
            &mut server,
            &Frame::Start {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    // `read` keeps the command alive until the `StdinEof` below
                    // arrives: without it the command can exit - and the session
                    // close the connection - before the harness gets to write the
                    // frame, which turns a scheduling delay into a spurious EPIPE.
                    "read line; printf out; printf err >&2; exit 23".into(),
                ],
                waypipe: WaypipeAction::Disabled,
            },
        )
        .unwrap();
        write_frame(&mut server, &Frame::StdinEof).unwrap();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = loop {
            match read_frame(&mut server).unwrap().unwrap() {
                Frame::Stdout(data) => stdout.extend(data),
                Frame::Stderr(data) => stderr.extend(data),
                Frame::Exit { code } => break code,
                frame => panic!("unexpected exec frame: {frame:?}"),
            }
        };
        assert_eq!(stdout, b"out");
        assert_eq!(stderr, b"err");
        assert_eq!(exit, 23);
        thread.join().unwrap().unwrap();
    }
}
