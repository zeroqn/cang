use anyhow::{Context, Result, anyhow, bail};
use cang_attach_protocol::{
    DetachFilter, Frame, PROTOCOL_VERSION, read_frame,
    terminal_trace::{trace_data_from_env, trace_event_from_env},
    write_frame,
};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::runtime::session::attach_profile::HostAttachProfiler;

use crate::runtime::session::task_control::{
    ActiveTaskStatus, ProcessInspector, list_records, resolve_task_selector,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY: Duration = Duration::from_millis(50);
const READY_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const READY_CONNECT_RETRY: Duration = Duration::from_millis(10);
const IO_BUF_SIZE: usize = 16 * 1024;
const ATTACH_IO_POLL_MS: i32 = 100;
const DAEMON_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(10);
const DAEMON_OUTPUT_IDLE_TIMEOUT: Duration = Duration::from_millis(300);
const FRAME_HEADER_LEN: usize = 5;
const MAX_ATTACH_PAYLOAD_LEN: usize = 16 * 1024 * 1024;
const HOST_OUTPUT_BATCH_MAX_FRAMES: usize = 16;
const HOST_OUTPUT_BATCH_MAX_BYTES: usize = IO_BUF_SIZE;
/// The PTY's end-of-input byte: a canonical-mode tty turns it into a read() that
/// returns 0, which is how a guest command sees the end of the host's stdin.
const PTY_END_OF_INPUT_BYTE: u8 = 0x04;
const FOCUS_GAINED_INPUT: &[u8] = b"\x1b[I";
const FOCUS_LOST_INPUT: &[u8] = b"\x1b[O";
const FOCUS_REPORT_ENABLE_OUTPUT: &[u8] = b"\x1b[?1004h";
const FOCUS_REPORT_GUARD_WINDOW: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Default)]
pub(crate) struct AttachInputPolicy {
    suppress_focus_input: bool,
    focus_report_guard: Option<Arc<Mutex<FocusReportGuard>>>,
    stdin_eof: StdinEofPolicy,
}

/// What the host does when its own stdin reaches EOF.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum StdinEofPolicy {
    /// Treat it as a request to leave the session attached no longer. This is
    /// right for an interactive shell session, whose stdin is the host terminal.
    #[default]
    Detach,
    /// Keep the session and end its input instead. This is what a process
    /// launcher needs: `cang -- <command>` is routinely started with stdin on
    /// /dev/null or a closed pipe, and detaching there threw away the command's
    /// output and its exit status while the command itself kept running.
    EndInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdinEofAction {
    Detach,
    SendEndOfInput,
    Nothing,
}

impl AttachInputPolicy {
    pub(crate) fn new(suppress_focus_input: bool, focus_report_guard: bool) -> Self {
        Self {
            suppress_focus_input,
            focus_report_guard: focus_report_guard
                .then(|| Arc::new(Mutex::new(FocusReportGuard::default()))),
            stdin_eof: StdinEofPolicy::default(),
        }
    }

    /// Keep the attached session when the host's stdin ends, translating the
    /// EOF into the guest PTY's end-of-input byte instead of a detach request.
    pub(crate) fn end_session_input_on_stdin_eof(mut self) -> Self {
        self.stdin_eof = StdinEofPolicy::EndInput;
        self
    }

    fn stdin_eof_action(&self, stdin_is_tty: bool) -> StdinEofAction {
        match self.stdin_eof {
            StdinEofPolicy::Detach => StdinEofAction::Detach,
            // A host terminal that returns EOF is a closed terminal, not a
            // finished input stream, so nothing is invented for it.
            StdinEofPolicy::EndInput if stdin_is_tty => StdinEofAction::Nothing,
            StdinEofPolicy::EndInput => StdinEofAction::SendEndOfInput,
        }
    }

    fn filter_host_input(&self, input: Vec<u8>) -> Vec<u8> {
        self.filter_host_input_at(input, Instant::now())
    }

    fn filter_host_input_at(&self, input: Vec<u8>, now: Instant) -> Vec<u8> {
        if self.suppress_focus_input {
            return strip_focus_input(input.as_slice());
        }
        let Some(guard) = &self.focus_report_guard else {
            return input;
        };
        match guard.lock() {
            Ok(mut guard) => guard.filter_host_input_at(input, now),
            Err(_) => input,
        }
    }

    fn observe_guest_output(&self, output: &[u8]) {
        self.observe_guest_output_at(output, Instant::now());
    }

    fn observe_guest_output_at(&self, output: &[u8], now: Instant) {
        let Some(guard) = &self.focus_report_guard else {
            return;
        };
        if let Ok(mut guard) = guard.lock() {
            guard.observe_guest_output_at(output, now);
        }
    }
}

#[derive(Debug, Default)]
struct FocusReportGuard {
    active_until: Option<Instant>,
    output_suffix: Vec<u8>,
}

impl FocusReportGuard {
    fn observe_guest_output_at(&mut self, output: &[u8], now: Instant) {
        let mut search = Vec::with_capacity(self.output_suffix.len() + output.len());
        search.extend_from_slice(&self.output_suffix);
        search.extend_from_slice(output);
        if contains_subslice(&search, FOCUS_REPORT_ENABLE_OUTPUT) {
            self.active_until = Some(now + FOCUS_REPORT_GUARD_WINDOW);
        }
        self.update_output_suffix(&search);
    }

    fn filter_host_input_at(&mut self, input: Vec<u8>, now: Instant) -> Vec<u8> {
        if !self.is_active_at(now) {
            return input;
        }
        self.filter_guarded_host_input(input.as_slice())
    }

    fn filter_guarded_host_input(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len());
        let mut remaining = input;
        while !remaining.is_empty() {
            if remaining.starts_with(FOCUS_GAINED_INPUT) || remaining.starts_with(FOCUS_LOST_INPUT)
            {
                remaining = &remaining[FOCUS_GAINED_INPUT.len()..];
            } else {
                output.extend_from_slice(remaining);
                self.active_until = None;
                return output;
            }
        }
        output
    }

    fn is_active_at(&mut self, now: Instant) -> bool {
        match self.active_until {
            Some(active_until) if now < active_until => true,
            Some(_) => {
                self.active_until = None;
                false
            }
            None => false,
        }
    }

    fn update_output_suffix(&mut self, bytes: &[u8]) {
        let suffix_len = FOCUS_REPORT_ENABLE_OUTPUT.len().saturating_sub(1);
        let start = bytes.len().saturating_sub(suffix_len);
        self.output_suffix.clear();
        self.output_suffix.extend_from_slice(&bytes[start..]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachOutcome {
    Detached,
    Exited(i32),
}

impl AttachOutcome {
    pub(crate) fn message(self) -> String {
        match self {
            Self::Detached => "cang: detached\n".to_owned(),
            Self::Exited(code) => format!("cang: session exited with status {code}\n"),
        }
    }
}

pub(crate) type ExitObserver<'a> = Option<&'a dyn Fn(i32) -> Result<()>>;

#[derive(Clone, Copy)]
struct AttachProxyOptions<'a> {
    input_policy: &'a AttachInputPolicy,
    exit_observer: ExitObserver<'a>,
}

fn observe_exit(observer: ExitObserver<'_>, code: i32) -> Result<()> {
    if let Some(observer) = observer {
        observer(code).context("failed to record managed guest exit at exit-frame observation")?;
    }
    Ok(())
}

pub(crate) fn attach_to_task(
    app_dir: &Path,
    task_selector: &str,
    inspector: &impl ProcessInspector,
) -> Result<String> {
    let records = list_records(app_dir)?;
    let record = resolve_task_selector(&records, task_selector)?;
    match inspector.status(&record.process) {
        ActiveTaskStatus::Running => {}
        status => bail!(
            "cang task '{}' is not attachable because it is {}",
            record.task_id,
            status.as_str()
        ),
    }
    let managed = record.managed.as_ref().ok_or_else(|| {
        anyhow!(
            "cang task '{}' is not a managed attachable session",
            record.task_id
        )
    })?;
    let outcome = attach_to_socket(&managed.attach_socket)?;
    Ok(outcome.message())
}

pub(crate) fn attach_to_socket(socket_path: &Path) -> Result<AttachOutcome> {
    let stream = connect_with_retry(socket_path, ConnectPolicy::reconnect())?;
    attach_stream(stream)
}

#[cfg(test)]
pub(crate) fn attach_to_ready_socket_with_input_policy(
    socket_path: &Path,
    daemon: bool,
    input_policy: AttachInputPolicy,
) -> Result<AttachOutcome> {
    attach_to_ready_socket_with_input_policy_and_exit_observer(
        socket_path,
        daemon,
        input_policy,
        None,
    )
}

pub(crate) fn attach_to_ready_socket_with_input_policy_and_exit_observer(
    socket_path: &Path,
    daemon: bool,
    input_policy: AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    attach_to_ready_socket_with_policy(
        socket_path,
        ConnectPolicy::post_ready(),
        daemon,
        input_policy,
        exit_observer,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConnectPolicy {
    timeout: Duration,
    retry: Duration,
}

impl ConnectPolicy {
    fn reconnect() -> Self {
        Self {
            timeout: CONNECT_TIMEOUT,
            retry: CONNECT_RETRY,
        }
    }

    fn post_ready() -> Self {
        Self {
            timeout: READY_CONNECT_TIMEOUT,
            retry: READY_CONNECT_RETRY,
        }
    }
}

fn connect_with_retry(socket_path: &Path, policy: ConnectPolicy) -> Result<UnixStream> {
    let deadline = Instant::now() + policy.timeout;
    let mut last_error = None;
    while Instant::now() <= deadline {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_error = Some(err);
                thread::sleep(policy.retry);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout")))
        .with_context(|| {
            format!(
                "failed to connect to cang attach socket '{}'",
                socket_path.display()
            )
        })
}

fn attach_to_ready_socket_with_policy(
    socket_path: &Path,
    policy: ConnectPolicy,
    daemon: bool,
    input_policy: AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    let deadline = Instant::now() + policy.timeout;
    let mut last_error = None;
    while Instant::now() <= deadline {
        match UnixStream::connect(socket_path) {
            Ok(mut stream) => match read_initial_hello(&mut stream)? {
                InitialHello::Ready if daemon => {
                    return attach_stream_after_hello_daemon(stream, input_policy, exit_observer);
                }
                InitialHello::Ready => {
                    return attach_stream_after_hello(stream, input_policy, exit_observer);
                }
                InitialHello::ClosedBeforeHandshake => {
                    last_error = Some(anyhow!("cang attach socket closed before handshake"));
                }
                InitialHello::Busy => {
                    last_error = Some(anyhow!("cang task already has an attached client"));
                }
            },
            Err(err) => {
                last_error = Some(anyhow!(err));
            }
        }
        thread::sleep(policy.retry);
    }
    Err(last_error.unwrap_or_else(|| anyhow!("timeout"))).with_context(|| {
        format!(
            "failed to complete initial cang attach handshake with ready socket '{}'",
            socket_path.display()
        )
    })
}

fn attach_stream(mut stream: UnixStream) -> Result<AttachOutcome> {
    match read_initial_hello(&mut stream)? {
        InitialHello::Ready => {
            attach_stream_after_hello(stream, AttachInputPolicy::default(), None)
        }
        InitialHello::ClosedBeforeHandshake => bail!("cang attach socket closed before handshake"),
        InitialHello::Busy => bail!("cang task already has an attached client"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialHello {
    Ready,
    ClosedBeforeHandshake,
    Busy,
}

fn read_initial_hello(stream: &mut UnixStream) -> Result<InitialHello> {
    match read_frame(stream)? {
        Some(Frame::Hello { version }) if version == PROTOCOL_VERSION => Ok(InitialHello::Ready),
        Some(Frame::Hello { version }) => bail!(
            "cang attach protocol version mismatch: host supports {PROTOCOL_VERSION}, guest reported {version}"
        ),
        Some(Frame::Busy) => Ok(InitialHello::Busy),
        Some(Frame::Error(message)) => bail!("cang attach failed: {message}"),
        Some(frame) => bail!("unexpected first cang attach frame: {frame:?}"),
        None => Ok(InitialHello::ClosedBeforeHandshake),
    }
}

fn attach_stream_after_hello(
    mut stream: UnixStream,
    input_policy: AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    let initial_size = terminal_size(libc::STDIN_FILENO);
    let mut raw = RawTerminalMode::enter(libc::STDIN_FILENO)?;
    write_initial_attach_frames(&mut stream, initial_size)?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let active = Arc::new(AtomicBool::new(true));
    let stdin_writer = writer.clone();
    let stdin_active = active.clone();
    let stdin_input_policy = input_policy.clone();
    let stdin_thread =
        thread::spawn(move || proxy_stdin(stdin_writer, stdin_active, stdin_input_policy));

    let outcome = proxy_remote(stream, &input_policy, exit_observer);
    if let Some(raw) = raw.as_mut() {
        raw.restore_now();
    }
    active.store(false, Ordering::Release);
    let _ = stdin_thread.join();
    outcome
}

fn attach_stream_after_hello_daemon(
    stream: UnixStream,
    input_policy: AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    attach_stream_after_hello_daemon_with_tty_check(stream, input_policy, exit_observer, || {
        daemon_bootstrap_tty_available(libc::STDIN_FILENO, libc::STDOUT_FILENO)
    })
}

fn attach_stream_after_hello_daemon_with_tty_check(
    mut stream: UnixStream,
    input_policy: AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
    tty_available: impl FnOnce() -> bool,
) -> Result<AttachOutcome> {
    ensure_daemon_bootstrap_tty(tty_available)?;
    let initial_size = terminal_size(libc::STDIN_FILENO);
    let mut raw = RawTerminalMode::enter(libc::STDIN_FILENO)?;
    write_initial_attach_frames(&mut stream, initial_size)?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let active = Arc::new(AtomicBool::new(true));
    let stdin_writer = writer.clone();
    let stdin_active = active.clone();
    let stdin_input_policy = input_policy.clone();
    let stdin_thread =
        thread::spawn(move || proxy_stdin(stdin_writer, stdin_active, stdin_input_policy));

    let mut stdout = std::io::stdout().lock();
    let outcome = proxy_remote_until_daemon_idle(
        &mut stream,
        &writer,
        &mut stdout,
        DAEMON_BOOTSTRAP_TIMEOUT,
        DAEMON_OUTPUT_IDLE_TIMEOUT,
        &input_policy,
        exit_observer,
    );
    if let Some(raw) = raw.as_mut() {
        raw.restore_now();
    }
    active.store(false, Ordering::Release);
    let _ = stdin_thread.join();
    outcome
}

fn ensure_daemon_bootstrap_tty(tty_available: impl FnOnce() -> bool) -> Result<()> {
    if tty_available() {
        Ok(())
    } else {
        bail!(
            "cang --daemon requires stdin and stdout to be connected to a TTY for target terminal initialization"
        )
    }
}

fn daemon_bootstrap_tty_available(stdin_fd: i32, stdout_fd: i32) -> bool {
    is_tty(stdin_fd) && is_tty(stdout_fd)
}

fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

fn write_initial_attach_frames<W>(writer: &mut W, initial_size: Option<TerminalSize>) -> Result<()>
where
    W: Write,
{
    if let Some(size) = initial_size {
        trace_event_from_env(
            "host",
            "resize",
            &format!(
                "direction=host-to-guest-initial rows={} cols={}",
                size.rows, size.cols
            ),
        );
        write_frame(
            writer,
            &Frame::Resize {
                rows: size.rows,
                cols: size.cols,
            },
        )?;
    }
    trace_event_from_env("host", "attach", "direction=host-to-guest");
    write_frame(writer, &Frame::Attach)
}

fn proxy_stdin(
    writer: Arc<Mutex<UnixStream>>,
    active: Arc<AtomicBool>,
    input_policy: AttachInputPolicy,
) -> Result<()> {
    let mut filter = DetachFilter::default();
    let mut input = std::io::stdin().lock();
    let mut buf = [0u8; IO_BUF_SIZE];
    let mut last_size = terminal_size(libc::STDIN_FILENO);
    let stdin_is_tty = is_tty(libc::STDIN_FILENO);
    while active.load(Ordering::Acquire) {
        if let Some(size) = terminal_size_changed(&mut last_size) {
            trace_event_from_env(
                "host",
                "resize",
                &format!(
                    "direction=host-to-guest-live rows={} cols={}",
                    size.rows, size.cols
                ),
            );
            write_to_guest(
                &writer,
                &Frame::Resize {
                    rows: size.rows,
                    cols: size.cols,
                },
            )?;
        }
        match wait_for_stdin(ATTACH_IO_POLL_MS)? {
            StdinReadiness::TimedOut => {
                let mut output = Vec::new();
                filter.flush_incomplete_escape_sequence(&mut output);
                write_host_input_data_to_guest(
                    &writer,
                    &input_policy,
                    "host-to-guest-stdin-flush",
                    output,
                )?;
                continue;
            }
            StdinReadiness::Readable => {}
        }
        let n = match input.read(&mut buf) {
            Ok(0) => {
                let mut output = Vec::new();
                filter.flush_pending(&mut output);
                let _ = write_host_input_data_to_guest(
                    &writer,
                    &input_policy,
                    "host-to-guest-stdin-eof",
                    output,
                );
                match input_policy.stdin_eof_action(stdin_is_tty) {
                    StdinEofAction::Detach => {
                        trace_event_from_env(
                            "host",
                            "detach",
                            "direction=host-to-guest reason=stdin-eof",
                        );
                        let _ = write_to_guest(&writer, &Frame::Detach);
                    }
                    StdinEofAction::SendEndOfInput => {
                        trace_event_from_env(
                            "host",
                            "stdin-eof",
                            "direction=host-to-guest reason=stdin-eof action=end-input",
                        );
                        // The guest command reads the managed PTY, not the host's
                        // stdin, so EOF has to arrive as the PTY's own
                        // end-of-input byte for a command that reads stdin to
                        // finish instead of waiting forever.
                        let _ = write_host_input_data_to_guest(
                            &writer,
                            &input_policy,
                            "host-to-guest-stdin-end-of-input",
                            vec![PTY_END_OF_INPUT_BYTE],
                        );
                    }
                    StdinEofAction::Nothing => {
                        trace_event_from_env(
                            "host",
                            "stdin-eof",
                            "direction=host-to-guest reason=stdin-eof action=keep-session",
                        );
                    }
                }
                return Ok(());
            }
            Ok(n) => n,
            Err(err) => return Err(err).context("failed to read host terminal stdin"),
        };
        let mut output = Vec::with_capacity(n);
        if filter.push(&buf[..n], &mut output) {
            write_host_input_data_to_guest(
                &writer,
                &input_policy,
                "host-to-guest-stdin-detach",
                output,
            )?;
            trace_event_from_env(
                "host",
                "detach",
                "direction=host-to-guest reason=detach-key",
            );
            write_to_guest(&writer, &Frame::Detach)?;
            return Ok(());
        }
        write_host_input_data_to_guest(&writer, &input_policy, "host-to-guest-stdin", output)?;
    }
    Ok(())
}

fn write_host_input_data_to_guest(
    writer: &Arc<Mutex<UnixStream>>,
    input_policy: &AttachInputPolicy,
    trace_label: &str,
    output: Vec<u8>,
) -> Result<()> {
    let output = input_policy.filter_host_input(output);
    if output.is_empty() {
        return Ok(());
    }
    trace_data_from_env("host", trace_label, &output);
    write_to_guest(writer, &Frame::Data(output))
}

fn strip_focus_input(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut remaining = input;
    while !remaining.is_empty() {
        if remaining.starts_with(FOCUS_GAINED_INPUT) || remaining.starts_with(FOCUS_LOST_INPUT) {
            remaining = &remaining[FOCUS_GAINED_INPUT.len()..];
        } else {
            output.push(remaining[0]);
            remaining = &remaining[1..];
        }
    }
    output
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn write_to_guest(writer: &Arc<Mutex<UnixStream>>, frame: &Frame) -> Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| anyhow!("attach writer lock poisoned"))?;
    write_frame(&mut *writer, frame)
}

enum StdinReadiness {
    Readable,
    TimedOut,
}

fn wait_for_stdin(timeout_ms: i32) -> Result<StdinReadiness> {
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc > 0 {
            return Ok(StdinReadiness::Readable);
        }
        if rc == 0 {
            return Ok(StdinReadiness::TimedOut);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err).context("failed to poll host terminal stdin");
    }
}

fn terminal_size_changed(last_size: &mut Option<TerminalSize>) -> Option<TerminalSize> {
    size_changed(terminal_size(libc::STDIN_FILENO), last_size)
}

fn size_changed(
    current: Option<TerminalSize>,
    last_size: &mut Option<TerminalSize>,
) -> Option<TerminalSize> {
    if current.is_some() && current != *last_size {
        *last_size = current;
        current
    } else {
        None
    }
}

fn proxy_remote(
    mut stream: UnixStream,
    input_policy: &AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    proxy_remote_unix_with_profile(
        &mut stream,
        &mut stdout,
        &mut stderr,
        HostAttachProfiler::from_process_env(),
        input_policy,
        exit_observer,
    )
}

fn proxy_remote_unix_with_profile<W, E>(
    stream: &mut UnixStream,
    stdout: &mut W,
    stderr: &mut E,
    mut profiler: HostAttachProfiler,
    input_policy: &AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome>
where
    W: Write,
    E: Write,
{
    let mut source = UnixRemoteFrameReader::new(stream);
    let result = proxy_remote_loop(
        &mut source,
        stdout,
        &mut profiler,
        input_policy,
        exit_observer,
    );
    let report = profiler.report_to(stderr);
    if let Err(err) = report {
        return Err(err).context("failed to write cang attach profile report");
    }
    result
}

#[cfg(test)]
fn proxy_remote_with_profile<R, W, E>(
    stream: &mut R,
    stdout: &mut W,
    stderr: &mut E,
    mut profiler: HostAttachProfiler,
) -> Result<AttachOutcome>
where
    R: Read,
    W: Write,
    E: Write,
{
    let mut source = BlockingRemoteFrameReader::new(stream);
    let result = proxy_remote_loop(
        &mut source,
        stdout,
        &mut profiler,
        &AttachInputPolicy::default(),
        None,
    );
    let report = profiler.report_to(stderr);
    if let Err(err) = report {
        return Err(err).context("failed to write cang attach profile report");
    }
    result
}

fn proxy_remote_loop<S, W>(
    source: &mut S,
    stdout: &mut W,
    profiler: &mut HostAttachProfiler,
    input_policy: &AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome>
where
    S: RemoteFrameSource,
    W: Write,
{
    loop {
        let frame = source.read_frame(profiler)?;
        match frame {
            Some(Frame::Data(data)) => {
                input_policy.observe_guest_output(&data);
                trace_data_from_env("host", "guest-to-host-stdout", &data);
                let mut batch = StdoutBatch::new();
                batch.push_data(data, profiler);
                loop {
                    if batch.is_full() {
                        batch.flush_to(stdout, profiler)?;
                        break;
                    }
                    match source.try_read_frame(profiler)? {
                        TryRemoteFrame::Frame(Frame::Data(data)) => {
                            input_policy.observe_guest_output(&data);
                            trace_data_from_env("host", "guest-to-host-stdout", &data);
                            if !batch.can_accept(data.len()) {
                                batch.flush_to(stdout, profiler)?;
                            }
                            batch.push_data(data, profiler);
                        }
                        TryRemoteFrame::Frame(frame) => {
                            batch.flush_to(stdout, profiler)?;
                            return handle_remote_control_frame(frame, exit_observer);
                        }
                        TryRemoteFrame::NotReady => {
                            batch.flush_to(stdout, profiler)?;
                            break;
                        }
                        TryRemoteFrame::Eof => {
                            batch.flush_to(stdout, profiler)?;
                            return Ok(AttachOutcome::Detached);
                        }
                    }
                }
            }
            Some(frame) => return handle_remote_control_frame(frame, exit_observer),
            None => return Ok(AttachOutcome::Detached),
        }
    }
}

trait RemoteFrameSource {
    fn read_frame(&mut self, profiler: &mut HostAttachProfiler) -> Result<Option<Frame>>;
    fn try_read_frame(&mut self, profiler: &mut HostAttachProfiler) -> Result<TryRemoteFrame>;
}

enum TryRemoteFrame {
    Frame(Frame),
    NotReady,
    Eof,
}

#[cfg(test)]
struct BlockingRemoteFrameReader<'a, R> {
    stream: &'a mut R,
}

#[cfg(test)]
impl<'a, R> BlockingRemoteFrameReader<'a, R> {
    fn new(stream: &'a mut R) -> Self {
        Self { stream }
    }
}

#[cfg(test)]
impl<R: Read> RemoteFrameSource for BlockingRemoteFrameReader<'_, R> {
    fn read_frame(&mut self, profiler: &mut HostAttachProfiler) -> Result<Option<Frame>> {
        read_profiled_frame(self.stream, profiler)
    }

    fn try_read_frame(&mut self, _profiler: &mut HostAttachProfiler) -> Result<TryRemoteFrame> {
        Ok(TryRemoteFrame::NotReady)
    }
}

struct UnixRemoteFrameReader<'a> {
    stream: &'a mut UnixStream,
    buffer: Vec<u8>,
}

impl<'a> UnixRemoteFrameReader<'a> {
    fn new(stream: &'a mut UnixStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
        }
    }

    fn read_frame_blocking(&mut self) -> Result<Option<Frame>> {
        let mut scratch = [0u8; IO_BUF_SIZE];
        loop {
            if let Some(frame) = self.decode_buffered_frame()? {
                return Ok(Some(frame));
            }
            match self.stream.read(&mut scratch) {
                Ok(0) if self.buffer.is_empty() => return Ok(None),
                Ok(0) => bail!("truncated cang attach protocol frame"),
                Ok(n) => self.buffer.extend_from_slice(&scratch[..n]),
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context("failed to read cang attach stream"),
            }
        }
    }

    fn try_read_frame_now(&mut self) -> Result<TryRemoteFrame> {
        let mut scratch = [0u8; IO_BUF_SIZE];
        loop {
            if let Some(frame) = self.decode_buffered_frame()? {
                return Ok(TryRemoteFrame::Frame(frame));
            }
            let rc = unsafe {
                libc::recv(
                    self.stream.as_raw_fd(),
                    scratch.as_mut_ptr().cast(),
                    scratch.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if rc > 0 {
                let n = usize::try_from(rc).expect("positive recv result fits usize");
                self.buffer.extend_from_slice(&scratch[..n]);
                continue;
            }
            if rc == 0 {
                if self.buffer.is_empty() {
                    return Ok(TryRemoteFrame::Eof);
                }
                bail!("truncated cang attach protocol frame");
            }
            let err = std::io::Error::last_os_error();
            match err.kind() {
                ErrorKind::WouldBlock => return Ok(TryRemoteFrame::NotReady),
                ErrorKind::Interrupted => continue,
                _ => return Err(err).context("failed to drain cang attach stream"),
            }
        }
    }

    fn decode_buffered_frame(&mut self) -> Result<Option<Frame>> {
        if self.buffer.len() < FRAME_HEADER_LEN {
            return Ok(None);
        }
        let payload_len = u32::from_be_bytes([
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
            self.buffer[4],
        ]) as usize;
        if payload_len > MAX_ATTACH_PAYLOAD_LEN {
            bail!("cang attach protocol frame payload exceeds maximum length");
        }
        let frame_len = FRAME_HEADER_LEN + payload_len;
        if self.buffer.len() < frame_len {
            return Ok(None);
        }
        let frame_bytes: Vec<u8> = self.buffer.drain(..frame_len).collect();
        let mut cursor = std::io::Cursor::new(frame_bytes);
        let frame = read_frame(&mut cursor)?
            .ok_or_else(|| anyhow!("buffered frame decoded as clean EOF"))?;
        Ok(Some(frame))
    }
}

impl RemoteFrameSource for UnixRemoteFrameReader<'_> {
    fn read_frame(&mut self, profiler: &mut HostAttachProfiler) -> Result<Option<Frame>> {
        if profiler.is_enabled() {
            let started = Instant::now();
            let frame = self.read_frame_blocking();
            profiler.record_frame_read(started.elapsed());
            frame
        } else {
            self.read_frame_blocking()
        }
    }

    fn try_read_frame(&mut self, profiler: &mut HostAttachProfiler) -> Result<TryRemoteFrame> {
        if profiler.is_enabled() {
            let started = Instant::now();
            let frame = self.try_read_frame_now();
            if matches!(
                frame,
                Ok(TryRemoteFrame::Frame(_)) | Ok(TryRemoteFrame::Eof)
            ) {
                profiler.record_frame_read(started.elapsed());
            }
            frame
        } else {
            self.try_read_frame_now()
        }
    }
}

fn read_profiled_frame<R: Read>(
    stream: &mut R,
    profiler: &mut HostAttachProfiler,
) -> Result<Option<Frame>> {
    if profiler.is_enabled() {
        let started = Instant::now();
        let frame = read_frame(stream);
        profiler.record_frame_read(started.elapsed());
        frame
    } else {
        read_frame(stream)
    }
}

struct StdoutBatch {
    bytes: Vec<u8>,
    frames: usize,
}

impl StdoutBatch {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            frames: 0,
        }
    }

    fn can_accept(&self, byte_count: usize) -> bool {
        self.frames == 0
            || (self.frames < HOST_OUTPUT_BATCH_MAX_FRAMES
                && self.bytes.len().saturating_add(byte_count) <= HOST_OUTPUT_BATCH_MAX_BYTES)
    }

    fn is_full(&self) -> bool {
        self.frames >= HOST_OUTPUT_BATCH_MAX_FRAMES
            || self.bytes.len() >= HOST_OUTPUT_BATCH_MAX_BYTES
    }

    fn push_data(&mut self, data: Vec<u8>, profiler: &mut HostAttachProfiler) {
        profiler.record_data_frame(data.len());
        self.frames += 1;
        self.bytes.extend(data);
    }

    fn flush_to<W: Write>(
        &mut self,
        stdout: &mut W,
        profiler: &mut HostAttachProfiler,
    ) -> Result<()> {
        if self.bytes.is_empty() {
            return Ok(());
        }
        let byte_count = self.bytes.len();
        let frame_count = self.frames;
        if profiler.is_enabled() {
            let started = Instant::now();
            stdout.write_all(&self.bytes)?;
            profiler.record_stdout_write(started.elapsed());
            let started = Instant::now();
            stdout.flush()?;
            profiler.record_stdout_flush(started.elapsed());
        } else {
            stdout.write_all(&self.bytes)?;
            stdout.flush()?;
        }
        profiler.record_stdout_batch(frame_count, byte_count);
        self.bytes.clear();
        self.frames = 0;
        Ok(())
    }
}

fn handle_remote_control_frame(
    frame: Frame,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    match frame {
        Frame::Exit { code } => {
            observe_exit(exit_observer, code)?;
            Ok(AttachOutcome::Exited(code))
        }
        Frame::Detach => Ok(AttachOutcome::Detached),
        Frame::Busy => bail!("cang task already has an attached client"),
        Frame::Error(message) => bail!("cang attach failed: {message}"),
        frame => bail!("unexpected cang attach frame from guest: {frame:?}"),
    }
}

fn proxy_remote_until_daemon_idle<W: Write>(
    stream: &mut UnixStream,
    writer: &Arc<Mutex<UnixStream>>,
    stdout: &mut W,
    startup_timeout: Duration,
    idle_timeout: Duration,
    input_policy: &AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    let mut stderr = std::io::stderr().lock();
    proxy_remote_until_daemon_idle_with_profile(
        stream,
        writer,
        stdout,
        &mut stderr,
        DaemonIdleTimeouts {
            startup: startup_timeout,
            idle: idle_timeout,
        },
        HostAttachProfiler::from_process_env(),
        AttachProxyOptions {
            input_policy,
            exit_observer,
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct DaemonIdleTimeouts {
    startup: Duration,
    idle: Duration,
}

fn proxy_remote_until_daemon_idle_with_profile<W: Write, E: Write>(
    stream: &mut UnixStream,
    writer: &Arc<Mutex<UnixStream>>,
    stdout: &mut W,
    stderr: &mut E,
    timeouts: DaemonIdleTimeouts,
    mut profiler: HostAttachProfiler,
    options: AttachProxyOptions<'_>,
) -> Result<AttachOutcome> {
    let result = proxy_remote_until_daemon_idle_loop(
        stream,
        writer,
        stdout,
        timeouts,
        &mut profiler,
        options.input_policy,
        options.exit_observer,
    );
    let report = profiler.report_to(stderr);
    if let Err(err) = report {
        return Err(err).context("failed to write cang attach profile report");
    }
    result
}

fn proxy_remote_until_daemon_idle_loop<W: Write>(
    stream: &mut UnixStream,
    writer: &Arc<Mutex<UnixStream>>,
    stdout: &mut W,
    timeouts: DaemonIdleTimeouts,
    profiler: &mut HostAttachProfiler,
    input_policy: &AttachInputPolicy,
    exit_observer: ExitObserver<'_>,
) -> Result<AttachOutcome> {
    let startup_deadline = Instant::now() + timeouts.startup;
    let mut saw_output = false;
    loop {
        let timeout = if saw_output {
            timeouts.idle
        } else {
            startup_deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default()
        };
        match wait_for_remote_frame(stream.as_raw_fd(), timeout)? {
            RemoteReadiness::TimedOut if saw_output => {
                write_to_guest(writer, &Frame::Detach)?;
                return Ok(AttachOutcome::Detached);
            }
            RemoteReadiness::TimedOut => {
                bail!(
                    "timed out waiting for initial output from cang --daemon bootstrap before detach"
                );
            }
            RemoteReadiness::Readable => {}
        }
        let frame = read_profiled_frame(stream, profiler)?;
        match frame {
            Some(Frame::Data(data)) => {
                input_policy.observe_guest_output(&data);
                trace_data_from_env("host", "guest-to-host-stdout", &data);
                let mut batch = StdoutBatch::new();
                batch.push_data(data, profiler);
                batch.flush_to(stdout, profiler)?;
                saw_output = true;
            }
            Some(Frame::Exit { code }) => {
                observe_exit(exit_observer, code)?;
                return Ok(AttachOutcome::Exited(code));
            }
            Some(Frame::Detach) | None => return Ok(AttachOutcome::Detached),
            Some(Frame::Busy) => bail!("cang task already has an attached client"),
            Some(Frame::Error(message)) => bail!("cang attach failed: {message}"),
            Some(frame) => bail!("unexpected cang attach frame from guest: {frame:?}"),
        }
    }
}

enum RemoteReadiness {
    Readable,
    TimedOut,
}

fn wait_for_remote_frame(fd: i32, timeout: Duration) -> Result<RemoteReadiness> {
    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc > 0 {
            return Ok(RemoteReadiness::Readable);
        }
        if rc == 0 {
            return Ok(RemoteReadiness::TimedOut);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err).context("failed to poll cang attach stream");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalSize {
    rows: u16,
    cols: u16,
}

fn terminal_size(fd: i32) -> Option<TerminalSize> {
    let mut winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut winsize) };
    if rc == 0 && winsize.ws_row > 0 && winsize.ws_col > 0 {
        Some(TerminalSize {
            rows: winsize.ws_row,
            cols: winsize.ws_col,
        })
    } else {
        None
    }
}

#[derive(Debug)]
struct RawTerminalMode {
    fd: i32,
    output_fd: i32,
    original: libc::termios,
    active: bool,
}

impl RawTerminalMode {
    fn enter(fd: i32) -> Result<Option<Self>> {
        if !is_tty(fd) {
            return Ok(None);
        }
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to read terminal mode");
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to enter raw terminal mode");
        }
        Ok(Some(Self {
            fd,
            output_fd: libc::STDOUT_FILENO,
            original,
            active: true,
        }))
    }

    fn restore_now(&mut self) {
        if self.active {
            let terminal_reset =
                b"\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1049l";
            let _ = unsafe {
                libc::write(
                    self.output_fd,
                    terminal_reset.as_ptr().cast(),
                    terminal_reset.len(),
                )
            };
            let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
            self.active = false;
        }
    }
}

impl Drop for RawTerminalMode {
    fn drop(&mut self) {
        self.restore_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cang_attach_protocol::{DETACH_PREFIX_BYTE, DETACH_SUFFIX_BYTE};
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    #[test]
    fn default_attach_input_policy_preserves_focus_reports() {
        let input = b"a\x1b[Ib\x1b[Oc".to_vec();

        let output = AttachInputPolicy::default().filter_host_input(input.clone());

        assert_eq!(output, input);
    }

    #[test]
    fn attach_input_policy_suppresses_exact_focus_reports() {
        let policy = AttachInputPolicy::new(true, false);

        let output = policy.filter_host_input(b"a\x1b[Ib\x1b[Oc".to_vec());

        assert_eq!(output, b"abc");
    }

    #[test]
    fn attach_input_policy_preserves_non_focus_escape_input() {
        let policy = AttachInputPolicy::new(true, false);
        let input = b"\x1b[0n\x1b[?62;22;52c\x1b[74;1R\x1b\x1b[".to_vec();

        let output = policy.filter_host_input(input.clone());

        assert_eq!(output, input);
    }

    #[test]
    fn attach_input_policy_preserves_mixed_non_focus_order() {
        let policy = AttachInputPolicy::new(true, false);

        let output = policy.filter_host_input(b"pre\x1b[0n\x1b[Imid\x1b[74;1R\x1b[Opost".to_vec());

        assert_eq!(output, b"pre\x1b[0nmid\x1b[74;1Rpost");
    }

    #[test]
    fn focus_report_guard_suppresses_focus_reports_after_focus_enable_output() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        let output = policy.filter_host_input_at(b"\x1b[I\x1b[O".to_vec(), now);

        assert!(output.is_empty());
    }

    #[test]
    fn focus_report_guard_clears_after_forwarding_non_focus_input() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        let first = policy.filter_host_input_at(b"\x1b[Ia".to_vec(), now);
        let second = policy.filter_host_input_at(b"\x1b[O".to_vec(), now);

        assert_eq!(first, b"a");
        assert_eq!(second, b"\x1b[O");
    }

    #[test]
    fn focus_report_guard_clears_within_same_input_batch() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        let output = policy.filter_host_input_at(b"\x1b[Ia\x1b[Ob".to_vec(), now);

        assert_eq!(output, b"a\x1b[Ob");
    }

    #[test]
    fn focus_report_guard_preserves_batch_when_non_focus_precedes_focus() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        let output = policy.filter_host_input_at(b"a\x1b[I".to_vec(), now);

        assert_eq!(output, b"a\x1b[I");
    }

    #[test]
    fn focus_report_guard_expires_after_bounded_window() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        let output = policy.filter_host_input_at(
            b"\x1b[I".to_vec(),
            now + FOCUS_REPORT_GUARD_WINDOW + Duration::from_millis(1),
        );

        assert_eq!(output, b"\x1b[I");
    }

    #[test]
    fn focus_report_guard_restarts_after_reasserted_focus_enable_output() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        policy.observe_guest_output_at(b"\x1b[?1004h", now + Duration::from_millis(600));
        let output = policy.filter_host_input_at(b"\x1b[I".to_vec(), now + Duration::from_secs(1));

        assert!(output.is_empty());
    }

    #[test]
    fn focus_report_guard_detects_focus_enable_split_across_output_chunks() {
        let policy = AttachInputPolicy::new(false, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?", now);
        assert_eq!(
            policy.filter_host_input_at(b"\x1b[I".to_vec(), now),
            b"\x1b[I"
        );
        policy.observe_guest_output_at(b"1004h", now);

        assert!(
            policy
                .filter_host_input_at(b"\x1b[I".to_vec(), now)
                .is_empty()
        );
    }

    #[test]
    fn no_focus_input_takes_precedence_over_focus_report_guard_timeout() {
        let policy = AttachInputPolicy::new(true, true);
        let now = Instant::now();

        policy.observe_guest_output_at(b"\x1b[?1004h", now);
        let output = policy.filter_host_input_at(
            b"a\x1b[Ib\x1b[Oc".to_vec(),
            now + FOCUS_REPORT_GUARD_WINDOW + Duration::from_millis(1),
        );

        assert_eq!(output, b"abc");
    }

    #[test]
    fn attach_outcome_messages_are_user_visible() {
        assert_eq!(AttachOutcome::Detached.message(), "cang: detached\n");
        assert_eq!(
            AttachOutcome::Exited(3).message(),
            "cang: session exited with status 3\n"
        );
    }

    #[test]
    fn raw_terminal_restore_now_disables_mouse_tracking_and_disarms_drop_fallback() {
        let mut pipe_fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
        let mut raw = RawTerminalMode {
            fd: -1,
            output_fd: pipe_fds[1],
            original: unsafe { std::mem::zeroed() },
            active: true,
        };

        raw.restore_now();
        raw.restore_now();
        unsafe { libc::close(pipe_fds[1]) };
        let mut output = Vec::new();
        unsafe { std::fs::File::from_raw_fd(pipe_fds[0]) }
            .read_to_end(&mut output)
            .unwrap();

        assert_eq!(
            output,
            b"\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1049l"
        );
        assert!(!raw.active);
    }

    #[test]
    fn detach_filter_passes_normal_bytes_until_escape() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"abc", &mut output));
        assert_eq!(output, b"abc");
        assert!(filter.push(
            &[b'd', DETACH_PREFIX_BYTE, DETACH_SUFFIX_BYTE, b'e'],
            &mut output
        ));
        assert_eq!(output, b"abcd");
    }

    #[test]
    fn terminal_size_changed_reports_only_real_changes() {
        let mut previous = Some(TerminalSize { rows: 24, cols: 80 });
        assert_eq!(
            size_changed(Some(TerminalSize { rows: 24, cols: 80 }), &mut previous),
            None
        );
        assert_eq!(previous, Some(TerminalSize { rows: 24, cols: 80 }));
        let changed = Some(TerminalSize {
            rows: 40,
            cols: 120,
        });
        assert_eq!(size_changed(changed, &mut previous), changed);
        assert_eq!(previous, changed);
    }

    #[test]
    fn post_ready_connect_policy_is_tiny_and_distinct_from_reconnect() {
        let post_ready = ConnectPolicy::post_ready();
        let reconnect = ConnectPolicy::reconnect();

        assert!(post_ready.timeout <= Duration::from_millis(250));
        assert!(post_ready.retry <= Duration::from_millis(25));
        assert!(post_ready.timeout < reconnect.timeout);
        assert!(post_ready.retry < reconnect.retry);
    }

    #[test]
    fn post_ready_attach_retries_socket_close_before_handshake() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("attach.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (first, _) = listener.accept().unwrap();
            drop(first);

            let (mut client, _) = listener.accept().unwrap();
            write_frame(
                &mut client,
                &Frame::Hello {
                    version: PROTOCOL_VERSION,
                },
            )
            .unwrap();
            assert_eq!(read_frame(&mut client).unwrap(), Some(Frame::Attach));
            write_frame(&mut client, &Frame::Exit { code: 7 }).unwrap();
        });

        let outcome =
            attach_to_ready_socket_with_input_policy(&socket, false, AttachInputPolicy::default())
                .unwrap();

        assert_eq!(outcome, AttachOutcome::Exited(7));
        server.join().unwrap();
    }

    #[test]
    fn initial_attach_frames_send_resize_before_command_start_signal() {
        let mut bytes = Vec::new();
        write_initial_attach_frames(
            &mut bytes,
            Some(TerminalSize {
                rows: 30,
                cols: 100,
            }),
        )
        .unwrap();
        let mut cursor = std::io::Cursor::new(bytes);

        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Some(Frame::Resize {
                rows: 30,
                cols: 100
            })
        );
        assert_eq!(read_frame(&mut cursor).unwrap(), Some(Frame::Attach));
    }

    #[test]
    fn initial_attach_frames_send_attach_when_resize_unknown() {
        let mut bytes = Vec::new();
        write_initial_attach_frames(&mut bytes, None).unwrap();
        let mut cursor = std::io::Cursor::new(bytes);

        assert_eq!(read_frame(&mut cursor).unwrap(), Some(Frame::Attach));
    }

    #[test]
    fn daemon_bootstrap_requires_tty_fds() {
        let file = tempfile::tempfile().unwrap();

        assert!(!daemon_bootstrap_tty_available(
            file.as_raw_fd(),
            file.as_raw_fd()
        ));
    }

    #[test]
    fn daemon_tty_failure_closes_without_sending_attach() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let server_thread = thread::spawn(move || {
            assert_eq!(read_frame(&mut server).unwrap(), None);
        });

        let err = attach_stream_after_hello_daemon_with_tty_check(
            client,
            AttachInputPolicy::default(),
            None,
            || false,
        )
        .expect_err("daemon attach should fail when TTY bootstrap is unavailable");

        assert!(
            format!("{err:#}").contains("requires stdin and stdout"),
            "unexpected error: {err:#}"
        );
        server_thread.join().unwrap();
    }

    #[test]
    fn daemon_remote_proxy_detaches_after_first_output_idle() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = Arc::new(Mutex::new(client.try_clone().unwrap()));
        let server_thread = thread::spawn(move || {
            write_frame(&mut server, &Frame::Data(b"fish> ".to_vec())).unwrap();
            assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Detach));
        });
        let mut output = Vec::new();

        let outcome = proxy_remote_until_daemon_idle(
            &mut client,
            &writer,
            &mut output,
            Duration::from_secs(1),
            Duration::from_millis(20),
            &AttachInputPolicy::default(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(output, b"fish> ");
        server_thread.join().unwrap();
    }

    #[test]
    fn daemon_remote_proxy_observes_focus_enable_for_report_guard() {
        let policy = AttachInputPolicy::new(false, true);
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = Arc::new(Mutex::new(client.try_clone().unwrap()));
        let server_thread = thread::spawn(move || {
            write_frame(&mut server, &Frame::Data(b"\x1b[?1004h".to_vec())).unwrap();
            assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Detach));
        });
        let mut output = Vec::new();

        let outcome = proxy_remote_until_daemon_idle(
            &mut client,
            &writer,
            &mut output,
            Duration::from_secs(1),
            Duration::from_millis(20),
            &policy,
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(output, b"\x1b[?1004h");
        assert!(policy.filter_host_input(b"\x1b[I".to_vec()).is_empty());
        server_thread.join().unwrap();
    }

    #[test]
    fn daemon_remote_proxy_returns_exit_before_idle_detach() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = Arc::new(Mutex::new(client.try_clone().unwrap()));
        let server_thread = thread::spawn(move || {
            write_frame(&mut server, &Frame::Data(b"booting".to_vec())).unwrap();
            write_frame(&mut server, &Frame::Exit { code: 9 }).unwrap();
        });
        let mut output = Vec::new();

        let outcome = proxy_remote_until_daemon_idle(
            &mut client,
            &writer,
            &mut output,
            Duration::from_secs(1),
            Duration::from_millis(200),
            &AttachInputPolicy::default(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Exited(9));
        assert_eq!(output, b"booting");
        server_thread.join().unwrap();
    }

    #[test]
    fn daemon_remote_proxy_times_out_without_initial_output() {
        let (mut client, _server) = UnixStream::pair().unwrap();
        let writer = Arc::new(Mutex::new(client.try_clone().unwrap()));
        let mut output = Vec::new();

        let err = proxy_remote_until_daemon_idle(
            &mut client,
            &writer,
            &mut output,
            Duration::from_millis(20),
            Duration::from_millis(20),
            &AttachInputPolicy::default(),
            None,
        )
        .expect_err("daemon bootstrap should require initial output before detach");

        assert!(
            format!("{err:#}").contains("timed out waiting for initial output"),
            "unexpected error: {err:#}"
        );
        assert!(output.is_empty());
    }

    #[test]
    fn post_ready_attach_retries_transient_busy_from_readiness_probe_teardown() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("attach.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            write_frame(&mut first, &Frame::Busy).unwrap();

            let (mut client, _) = listener.accept().unwrap();
            write_frame(
                &mut client,
                &Frame::Hello {
                    version: PROTOCOL_VERSION,
                },
            )
            .unwrap();
            assert_eq!(read_frame(&mut client).unwrap(), Some(Frame::Attach));
            write_frame(&mut client, &Frame::Exit { code: 0 }).unwrap();
        });

        let outcome =
            attach_to_ready_socket_with_input_policy(&socket, false, AttachInputPolicy::default())
                .unwrap();

        assert_eq!(outcome, AttachOutcome::Exited(0));
        server.join().unwrap();
    }

    #[test]
    fn proxy_remote_records_exit_before_returning_exited_outcome() {
        let mut stream = Vec::new();
        write_frame(&mut stream, &Frame::Exit { code: 130 }).unwrap();
        let mut stream = std::io::Cursor::new(stream);
        let mut stdout = Vec::new();
        let mut profiler = HostAttachProfiler::new(false);
        let observed = std::cell::Cell::new(None);
        let observer = |code| {
            observed.set(Some(code));
            Ok(())
        };

        let outcome = proxy_remote_loop(
            &mut BlockingRemoteFrameReader::new(&mut stream),
            &mut stdout,
            &mut profiler,
            &AttachInputPolicy::default(),
            Some(&observer),
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Exited(130));
        assert_eq!(observed.get(), Some(130));
        assert!(stdout.is_empty());
    }

    #[test]
    fn attach_profile_proxy_preserves_stdout_and_reports_to_stderr() {
        let mut stream = Vec::new();
        write_frame(&mut stream, &Frame::Data(b"hello".to_vec())).unwrap();
        write_frame(&mut stream, &Frame::Detach).unwrap();
        let mut stream = std::io::Cursor::new(stream);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_with_profile(
            &mut stream,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(true),
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(stdout, b"hello");
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.starts_with("cang attach profile role=host "));
        assert!(stderr.contains("frames=1"));
        assert!(stderr.contains("bytes=5"));
        assert!(stderr.contains("frame_max_bytes=5"));
        assert!(stderr.contains("frame_avg_bytes=5"));
        assert!(stderr.contains("stdout_batches=1"));
        assert!(stderr.contains("stdout_batch_frames_max=1"));
        assert!(stderr.contains("stdout_batch_bytes_max=5"));
        assert!(stderr.contains("stdout_batch_frames_avg=1"));
        assert!(stderr.contains("stdout_write_count=1"));
        assert!(stderr.contains("stdout_write_total_us="));
        assert!(stderr.contains("stdout_flush_count=1"));
        assert!(stderr.contains("stdout_flush_total_us="));
    }

    #[derive(Default)]
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn encoded_frame(frame: &Frame) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, frame).unwrap();
        bytes
    }

    #[test]
    fn unix_remote_proxy_batches_consecutive_ready_data_frames() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(&mut server, &Frame::Data(b"ab".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Data(b"cd".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Data(b"ef".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Detach).unwrap();
        let mut stdout = CountingWriter::default();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_unix_with_profile(
            &mut client,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(true),
            &AttachInputPolicy::default(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(stdout.bytes, b"abcdef");
        assert_eq!(stdout.writes, 1);
        assert_eq!(stdout.flushes, 1);
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("frames=3"));
        assert!(stderr.contains("bytes=6"));
        assert!(stderr.contains("stdout_batches=1"));
        assert!(stderr.contains("stdout_batch_frames_max=3"));
        assert!(stderr.contains("stdout_batch_bytes_max=6"));
        assert!(stderr.contains("stdout_batch_frames_avg=3"));
        assert!(stderr.contains("stdout_write_count=1"));
        assert!(stderr.contains("stdout_flush_count=1"));
    }

    #[test]
    fn unix_remote_proxy_observes_focus_enable_for_report_guard() {
        let policy = AttachInputPolicy::new(false, true);
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(&mut server, &Frame::Data(b"\x1b[?1004h".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Detach).unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_unix_with_profile(
            &mut client,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(true),
            &policy,
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(stdout, b"\x1b[?1004h");
        assert!(policy.filter_host_input(b"\x1b[I".to_vec()).is_empty());
    }

    struct NotifyingWriter {
        bytes: Vec<u8>,
        first_flush: Option<mpsc::Sender<()>>,
    }

    impl NotifyingWriter {
        fn new(first_flush: mpsc::Sender<()>) -> Self {
            Self {
                bytes: Vec::new(),
                first_flush: Some(first_flush),
            }
        }
    }

    impl Write for NotifyingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.bytes == b"first"
                && let Some(tx) = self.first_flush.take()
            {
                tx.send(()).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn unix_remote_proxy_flushes_before_waiting_for_partial_next_frame() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let first = encoded_frame(&Frame::Data(b"first".to_vec()));
        let second = encoded_frame(&Frame::Data(b"second".to_vec()));
        let detach = encoded_frame(&Frame::Detach);
        let (flushed_tx, flushed_rx) = mpsc::channel();
        let server_thread = thread::spawn(move || {
            server.write_all(&first).unwrap();
            server.write_all(&second[..3]).unwrap();
            flushed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first frame should flush before partial second frame completes");
            server.write_all(&second[3..]).unwrap();
            server.write_all(&detach).unwrap();
        });
        let mut stdout = NotifyingWriter::new(flushed_tx);
        let mut stderr = Vec::new();

        let outcome = proxy_remote_unix_with_profile(
            &mut client,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(true),
            &AttachInputPolicy::default(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(stdout.bytes, b"firstsecond");
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("frames=2"));
        assert!(stderr.contains("stdout_batches=2"));
        server_thread.join().unwrap();
    }

    #[test]
    fn unix_remote_proxy_does_not_set_nonblocking_on_shared_socket() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let mut stdin_writer_clone = client.try_clone().unwrap();
        assert!(!fd_is_nonblocking(client.as_raw_fd()));
        assert!(!fd_is_nonblocking(stdin_writer_clone.as_raw_fd()));
        let server_thread = thread::spawn(move || {
            write_frame(&mut server, &Frame::Data(b"ready".to_vec())).unwrap();
            write_frame(&mut server, &Frame::Detach).unwrap();
            assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Detach));
        });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_unix_with_profile(
            &mut client,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(true),
            &AttachInputPolicy::default(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(stdout, b"ready");
        assert!(!fd_is_nonblocking(client.as_raw_fd()));
        assert!(!fd_is_nonblocking(stdin_writer_clone.as_raw_fd()));
        write_frame(&mut stdin_writer_clone, &Frame::Detach).unwrap();
        server_thread.join().unwrap();
    }

    fn fd_is_nonblocking(fd: i32) -> bool {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL failed");
        flags & libc::O_NONBLOCK != 0
    }

    #[test]
    fn stdin_eof_detaches_an_interactive_session() {
        let policy = AttachInputPolicy::default();

        assert_eq!(policy.stdin_eof_action(false), StdinEofAction::Detach);
        assert_eq!(policy.stdin_eof_action(true), StdinEofAction::Detach);
    }

    #[test]
    fn stdin_eof_ends_the_input_of_a_launcher_command_session() {
        let policy = AttachInputPolicy::default().end_session_input_on_stdin_eof();

        assert_eq!(
            policy.stdin_eof_action(false),
            StdinEofAction::SendEndOfInput
        );
        // A host terminal that returns EOF is a closed terminal, not a finished
        // input stream, so no end-of-input byte is invented for it.
        assert_eq!(policy.stdin_eof_action(true), StdinEofAction::Nothing);
    }

    #[test]
    fn launcher_frame_stream_relays_output_before_the_exit_status() {
        // A `cang -- <command>` launch streams the payload's merged output first
        // and its exit status last; both have to survive the attach proxy, and
        // stdout must be flushed before the status ends the proxy.
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(&mut server, &Frame::Data(b"HI-OUT\r\n".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Data(b"HI-ERR\r\n".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Exit { code: 3 }).unwrap();
        let mut stdout = CountingWriter::default();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_unix_with_profile(
            &mut client,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(false),
            &AttachInputPolicy::default().end_session_input_on_stdin_eof(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Exited(3));
        assert_eq!(stdout.bytes, b"HI-OUT\r\nHI-ERR\r\n");
        assert_eq!(stdout.flushes, 1);
    }

    #[test]
    fn unix_remote_proxy_flushes_data_before_exit() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(&mut server, &Frame::Data(b"booting".to_vec())).unwrap();
        write_frame(&mut server, &Frame::Exit { code: 17 }).unwrap();
        let mut stdout = CountingWriter::default();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_unix_with_profile(
            &mut client,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(true),
            &AttachInputPolicy::default(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Exited(17));
        assert_eq!(stdout.bytes, b"booting");
        assert_eq!(stdout.writes, 1);
        assert_eq!(stdout.flushes, 1);
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("stdout_batches=1"));
        assert!(stderr.contains("stdout_write_count=1"));
        assert!(stderr.contains("stdout_flush_count=1"));
    }

    #[test]
    fn attach_profile_proxy_is_quiet_when_disabled() {
        let mut stream = Vec::new();
        write_frame(&mut stream, &Frame::Data(b"hello".to_vec())).unwrap();
        write_frame(&mut stream, &Frame::Detach).unwrap();
        let mut stream = std::io::Cursor::new(stream);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_with_profile(
            &mut stream,
            &mut stdout,
            &mut stderr,
            HostAttachProfiler::new(false),
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(stdout, b"hello");
        assert!(stderr.is_empty());
    }

    #[test]
    fn daemon_attach_profile_reports_output_path() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = Arc::new(Mutex::new(client.try_clone().unwrap()));
        let server_thread = thread::spawn(move || {
            write_frame(&mut server, &Frame::Data(b"ready".to_vec())).unwrap();
            assert_eq!(read_frame(&mut server).unwrap(), Some(Frame::Detach));
        });
        let mut output = Vec::new();
        let mut stderr = Vec::new();

        let outcome = proxy_remote_until_daemon_idle_with_profile(
            &mut client,
            &writer,
            &mut output,
            &mut stderr,
            DaemonIdleTimeouts {
                startup: Duration::from_secs(1),
                idle: Duration::from_millis(20),
            },
            HostAttachProfiler::new(true),
            AttachProxyOptions {
                input_policy: &AttachInputPolicy::default(),
                exit_observer: None,
            },
        )
        .unwrap();

        assert_eq!(outcome, AttachOutcome::Detached);
        assert_eq!(output, b"ready");
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("role=host"));
        assert!(stderr.contains("frame_max_bytes=5"));
        assert!(stderr.contains("stdout_batches=1"));
        assert!(stderr.contains("stdout_write_count=1"));
        assert!(stderr.contains("stdout_flush_count=1"));
        server_thread.join().unwrap();
    }
}
