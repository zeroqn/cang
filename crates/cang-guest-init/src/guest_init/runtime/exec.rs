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
use crate::guest_init::components::memory;
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
        // The listener only accepts and hands off; it never forks, so raising its
        // priority cannot leak into a user command.
        super::priority::prioritise_current_thread(
            "exec-listener",
            super::priority::INTERACTIVE_NICE,
        );
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
                super::priority::prioritise_current_thread(
                    "exec-client",
                    super::priority::INTERACTIVE_NICE,
                );
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

/// Reads the request that follows the hello frame.
///
/// Returns `None` when the client asked for something this listener answers
/// itself - a memory reclaim, whose reply has already gone out - or hung up.
fn read_request(client: &mut std::fs::File) -> Result<Option<(Vec<String>, WaypipeAction)>> {
    match read_frame(client)? {
        Some(Frame::Start { argv, waypipe }) => Ok(Some((argv, waypipe))),
        Some(Frame::ReclaimMemory) => {
            // The listener thread runs as the guest's root identity: the
            // command children drop credentials in `pre_exec`, this thread never
            // does, and releasing page cache needs root.
            let reply = match memory::reclaim_clean_page_cache() {
                Ok(report) => Frame::ReclaimedMemory {
                    freed_kib: report.freed_kib,
                },
                Err(err) => Frame::Error(format!("guest memory reclaim failed: {err:#}")),
            };
            write_frame(client, &reply)?;
            Ok(None)
        }
        Some(frame) => bail!("expected exec start, got {frame:?}"),
        None => Ok(None),
    }
}

/// Turns the forked child into the exec'd workload: its own process group, an
/// ordinary nice level, a killable OOM score, and the caller's credentials.
///
/// The OOM score is the load-bearing one. This listener runs on a thread of the
/// managed-session supervisor, which sits at `oom_score_adj` -1000 so it survives
/// guest memory pressure; the `fork` inherits that, and an unkillable command is
/// worse than a killed one - the kernel skips it, takes whatever else is at 0 (in
/// the measured arm, the session's idle payload), and then panics with "System is
/// deadlocked on memory" for want of any other victim (`oom_kill.c:1159-1166`).
/// So the command starts at the same score the session's own PTY child gets
/// (`runtime/session.rs`'s `allow_workload_kill`).
fn prepare_exec_child(
    identity: &DevIdentity,
    permissions: GuestPermissions,
    bounds: process::WorkloadBounds,
) -> std::io::Result<()> {
    // Both halves of "this process is the workload", from the bounds the parent
    // resolved: raw writes only, nothing that allocates between `fork` and `exec`.
    bounds.apply()?;
    // SAFETY: the caller runs this between `fork` and `exec`, where these
    // async-signal-safe calls are what the child is allowed to do at all.
    unsafe {
        if libc::setpgid(0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The client thread runs at the interactive nice level; the command it
        // starts is ordinary work and must not outrank the guest's own build.
        if libc::setpriority(libc::PRIO_PROCESS, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    if process::is_root() {
        process::apply_dev_credentials(identity, permissions)?;
    }
    Ok(())
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
    let Some((argv, waypipe_action)) = read_request(&mut client)? else {
        return Ok(());
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
    // Resolved here, in the parent: reading the environment and `sysconf` are not
    // async-signal-safe, and the child may only apply a decision.
    let bounds = process::WorkloadBounds::prepare();
    unsafe {
        command.pre_exec(move || prepare_exec_child(&identity, permissions, bounds));
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

    /// The inherited score is what made the defect: the listener's thread group
    /// sits at -1000, so a forked command was unkillable and the guest lost its
    /// OOM victim. Raising our own score first makes the reset observable - the
    /// prepared child reports 0, the control child keeps what it inherited.
    ///
    /// No privilege is needed for either write: the kernel only refuses a value
    /// below the one this process was last given `CAP_SYS_RESOURCE` at
    /// (`fs/proc/base.c:1130-1136`), which for an unprivileged process is
    /// `OOM_SCORE_ADJ_MIN`. The window where this test binary is +1000 lasts two
    /// child spawns and is restored before any assertion runs.
    #[test]
    fn exec_child_does_not_inherit_a_protected_oom_score() {
        let dir = tempfile::tempdir().unwrap();
        let inherited_path = dir.path().join("inherited");
        let reset_path = dir.path().join("reset");

        std::fs::write("/proc/self/oom_score_adj", "1000\n").unwrap();

        let control = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "cat /proc/self/oom_score_adj > {}",
                inherited_path.display()
            ))
            .status();

        let identity = identity();
        let permissions = Default::default();
        let mut prepared = Command::new("/bin/sh");
        prepared.arg("-c").arg(format!(
            "cat /proc/self/oom_score_adj > {}",
            reset_path.display()
        ));
        let bounds = process::WorkloadBounds::prepare();
        unsafe {
            prepared.pre_exec(move || prepare_exec_child(&identity, permissions, bounds));
        }
        let prepared_status = prepared.status();

        let own = std::fs::read_to_string("/proc/self/oom_score_adj").unwrap();
        std::fs::write("/proc/self/oom_score_adj", "0\n").unwrap();

        assert!(control.unwrap().success());
        assert!(prepared_status.unwrap().success());
        assert_eq!(own.trim(), "1000", "the test process keeps its own score");
        assert_eq!(
            std::fs::read_to_string(&inherited_path).unwrap().trim(),
            "1000",
            "a child that is not prepared keeps the inherited score"
        );
        assert_eq!(
            std::fs::read_to_string(&reset_path).unwrap().trim(),
            "0",
            "the exec child starts killable, like the session's PTY child"
        );
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
