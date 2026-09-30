use anyhow::{Context, Result, bail};
use cang_attach_protocol::{
    Frame, PROTOCOL_VERSION, read_frame,
    terminal_trace::{trace_data_from_env, trace_event_from_env},
    write_frame,
};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::guest_init::runtime::attach_profile::{self, GuestAttachProfiler};
use crate::guest_init::runtime::vsock::VsockListener;

use crate::guest_init::components::env::GuestPermissions;
use crate::guest_init::components::fdwatch;
use crate::guest_init::components::home::identity::DevIdentity;
use crate::guest_init::process;

const IO_BUF_SIZE: usize = 16 * 1024;
const POLL_TIMEOUT_MS: i32 = 100;
const PTY_INPUT_WRITE_POLL_TIMEOUT_MS: i32 = 10;
const ATTACHED_PTY_DRAIN_MAX_READS: usize = 4;
const ATTACHED_PTY_DRAIN_MAX_BYTES: usize = 64 * 1024;
const ATTACHED_PTY_DRAIN_MAX_ELAPSED: Duration = Duration::from_millis(1);
const DEFAULT_TERMINAL_ROWS: u16 = 24;
const DEFAULT_TERMINAL_COLS: u16 = 80;
const MIN_TERMINAL_STATE_ROWS: u16 = 2;
const TERMINAL_SCROLLBACK_ROWS: usize = 2_000;
const PTY_RAW_PASSTHROUGH_ENV: &str = "CANG_PTY_RAW_PASSTHROUGH";
const GUEST_DEBUG_ENV: &str = "CANG_GUEST_DEBUG";
const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";
const EXIT_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";
// Reset attributes, home the cursor, and clear the visible viewport before
// replaying the tracked terminal state so stale host-terminal cells disappear.
const CLEAR_VISIBLE_SCREEN: &[u8] = b"\x1b[m\x1b[H\x1b[J";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) struct ManagedSessionConfig {
    pub(in crate::guest_init) port: u32,
    pub(in crate::guest_init) protocol_version: u16,
    pub(in crate::guest_init) attach_profile: bool,
}

pub(in crate::guest_init) fn run(
    command: &[String],
    identity: &DevIdentity,
    permissions: GuestPermissions,
    drop_to_identity: bool,
    config: ManagedSessionConfig,
) -> Result<()> {
    if config.protocol_version != PROTOCOL_VERSION {
        bail!(
            "cang attach protocol version mismatch: guest supports {PROTOCOL_VERSION}, host requested {}",
            config.protocol_version
        );
    }
    // A resident supervisor must outlive guest memory pressure: libkrun's init
    // reboots the guest when the image entrypoint exits, so an OOM kill of this
    // process would look like the task finishing.
    process::protect_session_supervisor();
    managed_debug(&format!("starting port={}", config.port));
    managed_debug("vsock bind begin");
    let listener = VsockListener::bind(config.port)?;
    managed_debug("vsock bind complete");
    let pty_forwarding_mode = PtyForwardingMode::from_process_env();
    attach_profile::clear_process_env();
    PtyForwardingMode::clear_process_env();
    run_pre_start_event_loop(
        command,
        identity,
        permissions,
        drop_to_identity,
        listener,
        config.attach_profile,
        pty_forwarding_mode,
    )
}

fn managed_debug(message: &str) {
    if std::env::var(GUEST_DEBUG_ENV).ok().as_deref() == Some("1") {
        eprintln!("cang-guest-init: debug: managed session {message}");
    }
}

fn run_pre_start_event_loop(
    command: &[String],
    identity: &DevIdentity,
    permissions: GuestPermissions,
    drop_to_identity: bool,
    listener: VsockListener,
    attach_profile: bool,
    pty_forwarding_mode: PtyForwardingMode,
) -> Result<()> {
    loop {
        managed_debug("pre-start accept begin");
        let client = listener.accept()?;
        managed_debug("pre-start accept complete");
        match handle_pre_start_client(client)? {
            PreStartClientResult::Wait => continue,
            PreStartClientResult::Start {
                mut client,
                initial_size,
            } => {
                let pty = match Pty::open() {
                    Ok(pty) => pty,
                    Err(err) => {
                        let _ = write_frame(&mut client, &Frame::Error(format!("{err:#}")));
                        return Err(err);
                    }
                };
                let effective_size =
                    match apply_initial_winsize(pty.master.as_raw_fd(), initial_size) {
                        Ok(size) => size,
                        Err(err) => {
                            let _ = write_frame(&mut client, &Frame::Error(format!("{err:#}")));
                            return Err(err);
                        }
                    };
                let child =
                    match spawn_pty_child(&pty, command, identity, permissions, drop_to_identity) {
                        Ok(child) => child,
                        Err(err) => {
                            let _ = write_frame(&mut client, &Frame::Error(format!("{err:#}")));
                            return Err(err);
                        }
                    };
                let mut terminal_state = TerminalState::new(effective_size);
                match serve_attached_client(
                    &pty.master,
                    child,
                    client,
                    &listener,
                    &mut terminal_state,
                    attach_profile,
                    pty_forwarding_mode,
                )? {
                    ClientResult::Detached => {
                        return run_event_loop(
                            pty.master,
                            child,
                            listener,
                            terminal_state,
                            attach_profile,
                            pty_forwarding_mode,
                        );
                    }
                    ClientResult::ChildExited(code) => std::process::exit(code),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PtyForwardingMode {
    Normalized,
    RawPassthrough,
}

impl PtyForwardingMode {
    fn from_process_env() -> Self {
        Self::from_env_value(std::env::var(PTY_RAW_PASSTHROUGH_ENV).ok().as_deref())
    }

    fn from_env_value(value: Option<&str>) -> Self {
        match value.filter(|value| pty_raw_passthrough_env_value_enabled(value)) {
            Some(_) => Self::RawPassthrough,
            None => Self::Normalized,
        }
    }

    fn clear_process_env() {
        // SAFETY: guest-init consumes this diagnostic flag during single-threaded
        // managed-session setup before forking the user shell.
        unsafe { std::env::remove_var(PTY_RAW_PASSTHROUGH_ENV) };
    }
}

fn pty_raw_passthrough_env_value_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PtySize {
    rows: u16,
    cols: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self {
            rows: DEFAULT_TERMINAL_ROWS,
            cols: DEFAULT_TERMINAL_COLS,
        }
    }
}

struct TerminalState {
    parser: vt100::Parser,
    output_normalizer: TerminalOutputNormalizer,
}

impl TerminalState {
    fn new(size: PtySize) -> Self {
        Self {
            parser: vt100::Parser::new(
                size.rows.max(MIN_TERMINAL_STATE_ROWS),
                size.cols,
                TERMINAL_SCROLLBACK_ROWS,
            ),
            output_normalizer: TerminalOutputNormalizer::default(),
        }
    }

    fn normalize_output(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.output_normalizer.normalize(bytes)
    }

    fn record_normalized_output(&mut self, normalized: &[u8]) {
        self.parser.process(normalized);
    }

    fn record_output(&mut self, bytes: &[u8]) -> Vec<u8> {
        let normalized = self.normalize_output(bytes);
        self.record_normalized_output(&normalized);
        normalized
    }

    fn resize(&mut self, size: PtySize) {
        self.parser
            .screen_mut()
            .set_size(size.rows.max(MIN_TERMINAL_STATE_ROWS), size.cols);
    }

    #[cfg(test)]
    fn size(&self) -> PtySize {
        let (rows, cols) = self.parser.screen().size();
        PtySize { rows, cols }
    }

    fn render_restore(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut restore = Vec::new();
        if screen.alternate_screen() {
            restore.extend_from_slice(ENTER_ALTERNATE_SCREEN);
        } else {
            restore.extend_from_slice(EXIT_ALTERNATE_SCREEN);
        }
        restore.extend_from_slice(CLEAR_VISIBLE_SCREEN);
        restore.extend(screen.contents_formatted());
        restore.extend(screen.cursor_state_formatted());
        restore.extend(screen.attributes_formatted());
        restore.extend(screen.input_mode_formatted());
        restore
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterSet {
    Ascii,
    DecSpecialGraphics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphicSet {
    G0,
    G1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalizerState {
    Ground,
    Escape,
    DesignateG0,
    DesignateG1,
    EscapePassthrough,
    CsiPassthrough,
    StringPassthrough,
    StringPassthroughEscaped,
}

#[derive(Debug)]
struct TerminalOutputNormalizer {
    g0: CharacterSet,
    g1: CharacterSet,
    active: GraphicSet,
    state: NormalizerState,
}

impl Default for TerminalOutputNormalizer {
    fn default() -> Self {
        Self {
            g0: CharacterSet::Ascii,
            g1: CharacterSet::Ascii,
            active: GraphicSet::G0,
            state: NormalizerState::Ground,
        }
    }
}

impl TerminalOutputNormalizer {
    fn normalize(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            self.process_byte(byte, &mut output);
        }
        output
    }

    fn process_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        match self.state {
            NormalizerState::Ground => self.process_ground_byte(byte, output),
            NormalizerState::Escape => self.process_escape_byte(byte, output),
            NormalizerState::DesignateG0 => {
                self.process_designation_byte(byte, CharacterSetDesignator::G0, b'(', output)
            }
            NormalizerState::DesignateG1 => {
                self.process_designation_byte(byte, CharacterSetDesignator::G1, b')', output)
            }
            NormalizerState::EscapePassthrough => {
                output.push(byte);
                if is_escape_final_byte(byte) {
                    self.state = NormalizerState::Ground;
                }
            }
            NormalizerState::CsiPassthrough => {
                output.push(byte);
                if is_csi_final_byte(byte) {
                    self.state = NormalizerState::Ground;
                }
            }
            NormalizerState::StringPassthrough => {
                output.push(byte);
                match byte {
                    0x07 => self.state = NormalizerState::Ground,
                    0x1b => self.state = NormalizerState::StringPassthroughEscaped,
                    _ => {}
                }
            }
            NormalizerState::StringPassthroughEscaped => {
                output.push(byte);
                self.state = if byte == b'\\' {
                    NormalizerState::Ground
                } else {
                    NormalizerState::StringPassthrough
                };
            }
        }
    }

    fn process_ground_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        match byte {
            0x0e => self.active = GraphicSet::G1,
            0x0f => self.active = GraphicSet::G0,
            0x1b => self.state = NormalizerState::Escape,
            0x20..=0x7e if self.active_charset() == CharacterSet::DecSpecialGraphics => {
                if let Some(mapped) = dec_special_graphics_utf8(byte) {
                    output.extend_from_slice(mapped.as_bytes());
                } else {
                    output.push(byte);
                }
            }
            _ => output.push(byte),
        }
    }

    fn process_escape_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        match byte {
            b'(' => self.state = NormalizerState::DesignateG0,
            b')' => self.state = NormalizerState::DesignateG1,
            b'[' => {
                output.extend_from_slice(b"\x1b[");
                self.state = NormalizerState::CsiPassthrough;
            }
            b']' | b'P' | b'_' | b'^' | b'X' => {
                output.extend_from_slice(&[0x1b, byte]);
                self.state = NormalizerState::StringPassthrough;
            }
            b'c' => {
                self.reset();
                output.extend_from_slice(b"\x1bc");
                self.state = NormalizerState::Ground;
            }
            0x20..=0x2f => {
                output.extend_from_slice(&[0x1b, byte]);
                self.state = NormalizerState::EscapePassthrough;
            }
            _ => {
                output.extend_from_slice(&[0x1b, byte]);
                self.state = NormalizerState::Ground;
            }
        }
    }

    fn process_designation_byte(
        &mut self,
        byte: u8,
        designator: CharacterSetDesignator,
        intermediate: u8,
        output: &mut Vec<u8>,
    ) {
        match byte {
            b'0' => self.set_charset(designator, CharacterSet::DecSpecialGraphics),
            b'B' => self.set_charset(designator, CharacterSet::Ascii),
            _ => output.extend_from_slice(&[0x1b, intermediate, byte]),
        }
        self.state = NormalizerState::Ground;
    }

    fn active_charset(&self) -> CharacterSet {
        match self.active {
            GraphicSet::G0 => self.g0,
            GraphicSet::G1 => self.g1,
        }
    }

    fn set_charset(&mut self, designator: CharacterSetDesignator, charset: CharacterSet) {
        match designator {
            CharacterSetDesignator::G0 => self.g0 = charset,
            CharacterSetDesignator::G1 => self.g1 = charset,
        }
    }

    fn reset(&mut self) {
        self.g0 = CharacterSet::Ascii;
        self.g1 = CharacterSet::Ascii;
        self.active = GraphicSet::G0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterSetDesignator {
    G0,
    G1,
}

fn is_escape_final_byte(byte: u8) -> bool {
    (0x30..=0x7e).contains(&byte)
}

fn is_csi_final_byte(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

fn dec_special_graphics_utf8(byte: u8) -> Option<&'static str> {
    match byte {
        b'+' => Some("→"),
        b',' => Some("←"),
        b'-' => Some("↑"),
        b'.' => Some("↓"),
        b'0' => Some("▮"),
        b'`' => Some("◆"),
        b'a' => Some("▒"),
        b'b' => Some("␉"),
        b'c' => Some("␌"),
        b'd' => Some("␍"),
        b'e' => Some("␊"),
        b'f' => Some("°"),
        b'g' => Some("±"),
        b'h' => Some("␤"),
        b'i' => Some("␋"),
        b'j' => Some("┘"),
        b'k' => Some("┐"),
        b'l' => Some("┌"),
        b'm' => Some("└"),
        b'n' => Some("┼"),
        b'o' => Some("⎺"),
        b'p' => Some("⎻"),
        b'q' => Some("─"),
        b'r' => Some("⎼"),
        b's' => Some("⎽"),
        b't' => Some("├"),
        b'u' => Some("┤"),
        b'v' => Some("┴"),
        b'w' => Some("┬"),
        b'x' => Some("│"),
        b'y' => Some("≤"),
        b'z' => Some("≥"),
        b'{' => Some("π"),
        b'|' => Some("≠"),
        b'}' => Some("£"),
        b'~' => Some("·"),
        _ => None,
    }
}

enum PreStartClientResult<T> {
    Wait,
    Start {
        client: T,
        initial_size: Option<PtySize>,
    },
}

fn handle_pre_start_client<T>(mut client: T) -> Result<PreStartClientResult<T>>
where
    T: Read + Write,
{
    managed_debug("pre-start hello write begin");
    write_frame(
        &mut client,
        &Frame::Hello {
            version: PROTOCOL_VERSION,
        },
    )?;
    managed_debug("pre-start hello write complete");
    let mut initial_size = None;
    loop {
        match read_frame(&mut client)? {
            Some(Frame::Attach) => {
                managed_debug("pre-start attach received");
                trace_event_from_env("guest", "attach", "direction=host-to-guest-pre-start");
                return Ok(PreStartClientResult::Start {
                    client,
                    initial_size,
                });
            }
            Some(Frame::Resize { rows, cols }) => {
                trace_event_from_env(
                    "guest",
                    "resize",
                    &format!("direction=host-to-guest-pre-start rows={rows} cols={cols}"),
                );
                initial_size = Some(PtySize { rows, cols });
            }
            Some(Frame::Detach) | None => {
                managed_debug("pre-start readiness probe detached");
                return Ok(PreStartClientResult::Wait);
            }
            Some(frame) => {
                write_frame(
                    &mut client,
                    &Frame::Error(format!("expected attach frame, got {frame:?}")),
                )?;
                return Ok(PreStartClientResult::Wait);
            }
        }
    }
}

fn run_event_loop(
    master: File,
    child: libc::pid_t,
    listener: VsockListener,
    mut terminal_state: TerminalState,
    attach_profile: bool,
    pty_forwarding_mode: PtyForwardingMode,
) -> Result<()> {
    let fd_env = fdwatch::ProcFdEnv;
    let mut fd_watch = fdwatch::FdWatch::new(fdwatch::DEFAULT_WATCH_INTERVAL);
    loop {
        if let Some(tick) = fd_watch.poll(Instant::now(), &fd_env) {
            fdwatch::record_and_warn(&tick);
        }
        if let Some(code) = reap_child(child)? {
            std::process::exit(code);
        }
        let mut fds = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master.as_raw_fd(),
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
            return Err(err).context("managed session poll failed");
        }
        if fds[1].revents & libc::POLLIN != 0 {
            drain_detached_pty_output(master.as_raw_fd(), &mut terminal_state)?;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let client = listener.accept()?;
            match serve_client(
                &master,
                child,
                client,
                &listener,
                &mut terminal_state,
                attach_profile,
                pty_forwarding_mode,
            )? {
                ClientResult::Detached => continue,
                ClientResult::ChildExited(code) => std::process::exit(code),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientResult {
    Detached,
    ChildExited(i32),
}

fn serve_client(
    master: &File,
    child: libc::pid_t,
    mut client: File,
    listener: &VsockListener,
    terminal_state: &mut TerminalState,
    attach_profile: bool,
    pty_forwarding_mode: PtyForwardingMode,
) -> Result<ClientResult> {
    write_frame(
        &mut client,
        &Frame::Hello {
            version: PROTOCOL_VERSION,
        },
    )?;
    if !read_post_start_attach_request(master, &mut client, terminal_state)? {
        return Ok(ClientResult::Detached);
    }
    drain_detached_pty_output(master.as_raw_fd(), terminal_state)?;
    let restore = terminal_state.render_restore();
    if !restore.is_empty() {
        trace_data_from_env("guest", "restore-to-host-output", &restore);
        write_frame(&mut client, &Frame::Data(restore))?;
    }
    let foreground_pgid = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    if foreground_pgid < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to read managed PTY foreground process group");
    }
    if unsafe { libc::kill(-foreground_pgid, libc::SIGWINCH) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to request managed PTY foreground redraw");
    }
    serve_attached_client(
        master,
        child,
        client,
        listener,
        terminal_state,
        attach_profile,
        pty_forwarding_mode,
    )
}

fn read_post_start_attach_request<T>(
    master: &File,
    client: &mut T,
    terminal_state: &mut TerminalState,
) -> Result<bool>
where
    T: Read + Write,
{
    loop {
        match read_frame(client)? {
            Some(Frame::Attach) => {
                trace_event_from_env("guest", "attach", "direction=host-to-guest-post-start");
                return Ok(true);
            }
            Some(Frame::Resize { rows, cols }) => {
                trace_event_from_env(
                    "guest",
                    "resize",
                    &format!("direction=host-to-guest-post-start rows={rows} cols={cols}"),
                );
                let size = PtySize { rows, cols };
                set_winsize(master.as_raw_fd(), rows, cols)?;
                terminal_state.resize(size);
            }
            Some(Frame::Detach) | None => return Ok(false),
            Some(frame) => {
                write_frame(
                    client,
                    &Frame::Error(format!("expected attach frame, got {frame:?}")),
                )?;
                return Ok(false);
            }
        }
    }
}

fn serve_attached_client(
    master: &File,
    child: libc::pid_t,
    mut client: File,
    listener: &VsockListener,
    terminal_state: &mut TerminalState,
    attach_profile: bool,
    pty_forwarding_mode: PtyForwardingMode,
) -> Result<ClientResult> {
    let mut profiler = GuestAttachProfiler::new(attach_profile);
    let active = Arc::new(AtomicBool::new(true));
    let reader_active = active.clone();
    let (resize_tx, resize_rx) = mpsc::channel();
    let mut client_reader = client.try_clone()?;
    let mut pty_writer = duplicate_file(master)?;
    let input_thread = thread::spawn(move || -> Result<()> {
        while reader_active.load(Ordering::SeqCst) {
            match read_frame(&mut client_reader)? {
                Some(Frame::Data(data)) => {
                    trace_data_from_env("guest", "host-to-guest-pty-input", &data);
                    write_all_retrying_would_block(&mut pty_writer, &data)?;
                }
                Some(Frame::Resize { rows, cols }) => {
                    trace_event_from_env(
                        "guest",
                        "resize",
                        &format!("direction=host-to-guest-attached rows={rows} cols={cols}"),
                    );
                    set_winsize(pty_writer.as_raw_fd(), rows, cols)?;
                    let _ = resize_tx.send(PtySize { rows, cols });
                }
                Some(Frame::Detach) | None => {
                    trace_event_from_env("guest", "detach", "direction=host-to-guest");
                    break;
                }
                Some(Frame::Attach) => {
                    trace_event_from_env("guest", "attach", "direction=host-to-guest-attached");
                }
                Some(frame) => bail!("unexpected attach client frame: {frame:?}"),
            }
        }
        reader_active.store(false, Ordering::SeqCst);
        Ok(())
    });

    let mut pty_reader = duplicate_file(master)?;
    let mut buf = [0u8; IO_BUF_SIZE];
    let drain_limits = AttachedPtyDrainLimits::default();
    let fd_env = fdwatch::ProcFdEnv;
    let mut fd_watch = fdwatch::FdWatch::new(fdwatch::DEFAULT_WATCH_INTERVAL);
    while active.load(Ordering::SeqCst) {
        if let Some(tick) = fd_watch.poll(Instant::now(), &fd_env) {
            fdwatch::record_and_warn(&tick);
        }
        apply_pending_resizes(&resize_rx, terminal_state);
        if let Some(code) = reap_child(child)? {
            forward_final_attached_pty_output(
                &mut pty_reader,
                &mut buf,
                &mut client,
                terminal_state,
                &mut profiler,
                pty_forwarding_mode,
            )?;
            let _ = write_frame(&mut client, &Frame::Exit { code });
            let _ = profiler.report_to(&mut std::io::stderr().lock());
            stop_client_input(&active, client.as_raw_fd(), input_thread);
            return Ok(ClientResult::ChildExited(code));
        }
        let mut fds = [
            libc::pollfd {
                fd: pty_reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
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
            return Err(err).context("managed session client poll failed");
        }
        if fds[1].revents & libc::POLLIN != 0
            && let Ok(mut extra_client) = listener.accept()
        {
            let _ = write_frame(&mut extra_client, &Frame::Busy);
        }
        if fds[0].revents & libc::POLLIN == 0 {
            continue;
        }
        profiler.record_pty_readable();
        let burst = read_attached_pty_burst_with_restored_blocking(
            &mut pty_reader,
            &mut buf,
            &mut profiler,
            drain_limits,
        )?;
        if burst.bytes.is_empty() {
            trace_event_from_env(
                "guest",
                "pty_burst",
                &format!(
                    "direction=pty-to-guest-init bytes=0 reads={} stop={}",
                    burst.reads,
                    burst.stop_reason.as_trace_value()
                ),
            );
            if burst.stop_reason == AttachedPtyDrainStop::Eof {
                thread::sleep(Duration::from_millis(10));
            }
            continue;
        }
        trace_event_from_env(
            "guest",
            "pty_burst",
            &format!(
                "direction=pty-to-guest-init bytes={} reads={} stop={}",
                burst.bytes.len(),
                burst.reads,
                burst.stop_reason.as_trace_value()
            ),
        );
        if !forward_and_record_pty_output(
            &burst.bytes,
            &mut client,
            terminal_state,
            &mut profiler,
            pty_forwarding_mode,
        ) {
            break;
        }
    }
    apply_pending_resizes(&resize_rx, terminal_state);
    let _ = profiler.report_to(&mut std::io::stderr().lock());
    stop_client_input(&active, client.as_raw_fd(), input_thread);
    Ok(ClientResult::Detached)
}

#[derive(Debug, Clone, Copy)]
struct AttachedPtyDrainLimits {
    max_reads: usize,
    max_bytes: usize,
    max_elapsed: Duration,
}

impl Default for AttachedPtyDrainLimits {
    fn default() -> Self {
        Self {
            max_reads: ATTACHED_PTY_DRAIN_MAX_READS,
            max_bytes: ATTACHED_PTY_DRAIN_MAX_BYTES,
            max_elapsed: ATTACHED_PTY_DRAIN_MAX_ELAPSED,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedPtyDrainStop {
    WouldBlock,
    ReadBound,
    ByteBound,
    ElapsedBound,
    Eof,
}

impl AttachedPtyDrainStop {
    fn as_trace_value(self) -> &'static str {
        match self {
            Self::WouldBlock => "would_block",
            Self::ReadBound => "read_bound",
            Self::ByteBound => "byte_bound",
            Self::ElapsedBound => "elapsed_bound",
            Self::Eof => "eof",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AttachedPtyBurst {
    bytes: Vec<u8>,
    reads: usize,
    stop_reason: AttachedPtyDrainStop,
}

fn read_attached_pty_burst_with_restored_blocking(
    reader: &mut File,
    buf: &mut [u8; IO_BUF_SIZE],
    profiler: &mut GuestAttachProfiler,
    limits: AttachedPtyDrainLimits,
) -> Result<AttachedPtyBurst> {
    set_nonblocking(reader.as_raw_fd(), true)?;
    let drain_result = read_attached_pty_burst(reader, buf, profiler, limits);
    let restore_result = set_nonblocking(reader.as_raw_fd(), false);
    match (drain_result, restore_result) {
        (Ok(burst), Ok(())) => Ok(burst),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(err).context("failed to restore attached PTY blocking mode"),
        (Err(drain_err), Err(restore_err)) => Err(drain_err).with_context(|| {
            format!("also failed to restore attached PTY blocking mode: {restore_err:#}")
        }),
    }
}

fn read_attached_pty_burst<R>(
    reader: &mut R,
    buf: &mut [u8; IO_BUF_SIZE],
    profiler: &mut GuestAttachProfiler,
    limits: AttachedPtyDrainLimits,
) -> Result<AttachedPtyBurst>
where
    R: Read,
{
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut reads = 0;
    let stop_reason = loop {
        if reads >= limits.max_reads {
            break AttachedPtyDrainStop::ReadBound;
        }
        if bytes.len() >= limits.max_bytes {
            break AttachedPtyDrainStop::ByteBound;
        }
        let remaining = limits.max_bytes - bytes.len();
        let read_len = buf.len().min(remaining);
        let read_started = Instant::now();
        match reader.read(&mut buf[..read_len]) {
            Ok(0) => break AttachedPtyDrainStop::Eof,
            Ok(n) => {
                profiler.record_pty_read(n, read_started.elapsed(), IO_BUF_SIZE);
                reads += 1;
                bytes.extend_from_slice(&buf[..n]);
                if reads >= limits.max_reads {
                    break AttachedPtyDrainStop::ReadBound;
                }
                if bytes.len() >= limits.max_bytes {
                    break AttachedPtyDrainStop::ByteBound;
                }
                if started.elapsed() >= limits.max_elapsed {
                    break AttachedPtyDrainStop::ElapsedBound;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                break AttachedPtyDrainStop::WouldBlock;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).context("failed to drain attached PTY output"),
        }
    };
    profiler.record_pty_drain(
        reads,
        stop_reason == AttachedPtyDrainStop::WouldBlock,
        matches!(
            stop_reason,
            AttachedPtyDrainStop::ReadBound
                | AttachedPtyDrainStop::ByteBound
                | AttachedPtyDrainStop::ElapsedBound
        ),
    );
    Ok(AttachedPtyBurst {
        bytes,
        reads,
        stop_reason,
    })
}

struct ForwardedPtyOutput {
    parser_bytes: Vec<u8>,
    write_succeeded: bool,
    normalize_elapsed: Duration,
}

fn forward_and_record_pty_output(
    bytes: &[u8],
    client: &mut impl Write,
    terminal_state: &mut TerminalState,
    profiler: &mut GuestAttachProfiler,
    mode: PtyForwardingMode,
) -> bool {
    let forwarded = normalize_and_write_pty_output(bytes, client, terminal_state, profiler, mode);
    let write_succeeded = forwarded.write_succeeded;
    record_forwarded_pty_output(&forwarded, terminal_state, profiler);
    write_succeeded
}

fn normalize_and_write_pty_output(
    bytes: &[u8],
    client: &mut impl Write,
    terminal_state: &mut TerminalState,
    profiler: &mut GuestAttachProfiler,
    mode: PtyForwardingMode,
) -> ForwardedPtyOutput {
    let normalize_started = Instant::now();
    let normalized = terminal_state.normalize_output(bytes);
    let normalize_elapsed = normalize_started.elapsed();
    match mode {
        PtyForwardingMode::Normalized => {
            trace_data_from_env("guest", "pty-to-host-normalized-output", &normalized);
            let frame_bytes = normalized.len();
            let frame = Frame::Data(normalized);

            let frame_write_started = Instant::now();
            let write_succeeded = write_frame(client, &frame).is_ok();
            profiler.record_frame_write(frame_bytes, frame_write_started.elapsed());

            let Frame::Data(parser_bytes) = frame else {
                unreachable!("live PTY output frame must be data");
            };
            ForwardedPtyOutput {
                parser_bytes,
                write_succeeded,
                normalize_elapsed,
            }
        }
        PtyForwardingMode::RawPassthrough => {
            trace_data_from_env("guest", "pty-to-host-raw-output", bytes);
            trace_data_from_env("guest", "pty-to-parser-normalized-output", &normalized);
            let frame_bytes = bytes.len();
            let frame = Frame::Data(bytes.to_vec());

            let frame_write_started = Instant::now();
            let write_succeeded = write_frame(client, &frame).is_ok();
            profiler.record_frame_write(frame_bytes, frame_write_started.elapsed());

            ForwardedPtyOutput {
                parser_bytes: normalized,
                write_succeeded,
                normalize_elapsed,
            }
        }
    }
}

fn record_forwarded_pty_output(
    forwarded: &ForwardedPtyOutput,
    terminal_state: &mut TerminalState,
    profiler: &mut GuestAttachProfiler,
) {
    let parser_started = Instant::now();
    terminal_state.record_normalized_output(&forwarded.parser_bytes);
    let parser_elapsed = parser_started.elapsed();
    profiler.record_terminal_processing(forwarded.normalize_elapsed, parser_elapsed);
}

fn apply_pending_resizes(resize_rx: &mpsc::Receiver<PtySize>, terminal_state: &mut TerminalState) {
    while let Ok(size) = resize_rx.try_recv() {
        trace_event_from_env(
            "guest",
            "resize",
            &format!(
                "direction=applied-to-terminal-state rows={} cols={}",
                size.rows, size.cols
            ),
        );
        terminal_state.resize(size);
    }
}

fn stop_client_input(
    active: &AtomicBool,
    client_fd: RawFd,
    input_thread: thread::JoinHandle<Result<()>>,
) {
    active.store(false, Ordering::SeqCst);
    let _ = unsafe { libc::shutdown(client_fd, libc::SHUT_RDWR) };
    let _ = input_thread.join();
}

fn write_all_retrying_would_block(file: &mut File, mut data: &[u8]) -> Result<()> {
    while !data.is_empty() {
        match file.write(data) {
            Ok(0) => bail!("failed to write PTY input: zero-length write"),
            Ok(n) => data = &data[n..],
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                poll_fd(
                    file.as_raw_fd(),
                    libc::POLLOUT,
                    PTY_INPUT_WRITE_POLL_TIMEOUT_MS,
                )
                .context("failed waiting for managed PTY input to become writable")?;
            }
            Err(err) => return Err(err).context("failed to write PTY input"),
        }
    }
    Ok(())
}

/// Forward whatever the PTY master still holds once the child has exited.
///
/// The child can exit between two polls with its last output still buffered in
/// the master, so the attached client has to be given that output before the
/// `Exit` frame - otherwise a session that ends drops its final bytes. A master
/// whose slave side has closed reports `EIO` in place of EOF, so any read that
/// yields nothing ends the drain: the child is gone, and whatever was readable
/// has already been forwarded.
fn forward_final_attached_pty_output(
    reader: &mut File,
    buf: &mut [u8; IO_BUF_SIZE],
    client: &mut File,
    terminal_state: &mut TerminalState,
    profiler: &mut GuestAttachProfiler,
    mode: PtyForwardingMode,
) -> Result<()> {
    set_nonblocking(reader.as_raw_fd(), true)?;
    let drain = forward_final_attached_pty_output_nonblocking(
        reader,
        buf,
        client,
        terminal_state,
        profiler,
        mode,
    );
    let restore = set_nonblocking(reader.as_raw_fd(), false);
    drain.and(restore)
}

fn forward_final_attached_pty_output_nonblocking(
    reader: &mut File,
    buf: &mut [u8; IO_BUF_SIZE],
    client: &mut File,
    terminal_state: &mut TerminalState,
    profiler: &mut GuestAttachProfiler,
    mode: PtyForwardingMode,
) -> Result<()> {
    loop {
        match reader.read(buf) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                if !forward_and_record_pty_output(
                    &buf[..read],
                    client,
                    terminal_state,
                    profiler,
                    mode,
                ) {
                    return Ok(());
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(()),
        }
    }
}

fn drain_detached_pty_output(master_fd: RawFd, terminal_state: &mut TerminalState) -> Result<()> {
    let mut file = duplicate_fd(master_fd)?;
    set_nonblocking(file.as_raw_fd(), true)?;
    let result = drain_nonblocking(&mut file, terminal_state);
    let restore_result = set_nonblocking(file.as_raw_fd(), false);
    result.and(restore_result)
}

fn drain_nonblocking(file: &mut File, terminal_state: &mut TerminalState) -> Result<()> {
    let mut buf = [0u8; IO_BUF_SIZE];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                trace_data_from_env("guest", "pty-to-detached-state", &buf[..n]);
                terminal_state.record_output(&buf[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(err) => return Err(err).context("failed to drain detached PTY output"),
        }
    }
}

fn poll_fd(fd: RawFd, events: i16, timeout_ms: i32) -> Result<()> {
    loop {
        let mut pollfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("poll failed");
        }
        if rc == 0 {
            continue;
        }
        if pollfd.revents & libc::POLLNVAL != 0 {
            bail!("polled invalid fd");
        }
        if pollfd.revents & libc::POLLERR != 0 {
            bail!("polled fd reported error");
        }
        if pollfd.revents & libc::POLLHUP != 0 && pollfd.revents & events == 0 {
            bail!("polled fd hung up");
        }
        if pollfd.revents & events != 0 {
            return Ok(());
        }
    }
}

fn reap_child(child: libc::pid_t) -> Result<Option<i32>> {
    let mut status = 0;
    let rc = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
    if rc == 0 {
        return Ok(None);
    }
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ECHILD) {
            return Ok(Some(0));
        }
        return Err(err).context("failed to wait for managed session child");
    }
    if libc::WIFEXITED(status) {
        Ok(Some(libc::WEXITSTATUS(status)))
    } else {
        Ok(Some(1))
    }
}

struct Pty {
    master: File,
    slave_path: String,
}

impl Pty {
    fn open() -> Result<Self> {
        let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("failed to open PTY master");
        }
        if unsafe { libc::grantpt(fd) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to grant PTY slave");
        }
        if unsafe { libc::unlockpt(fd) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to unlock PTY slave");
        }
        let mut buf = vec![0i8; 256];
        if unsafe { libc::ptsname_r(fd, buf.as_mut_ptr(), buf.len()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to resolve PTY slave path");
        }
        let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        Ok(Self {
            master: unsafe { File::from_raw_fd(fd) },
            slave_path: cstr.to_string_lossy().into_owned(),
        })
    }
}

fn spawn_pty_child(
    pty: &Pty,
    command: &[String],
    identity: &DevIdentity,
    permissions: GuestPermissions,
    drop_to_identity: bool,
) -> Result<libc::pid_t> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!(
            "failed to fork managed session child: {}",
            std::io::Error::last_os_error()
        );
    }
    if pid == 0 {
        // The workload subtree inherits the supervisor's OOM score, so it has to
        // stay killable: the guest needs a victim for the kernel to pick.
        process::allow_workload_kill();
        let code = child_main(
            &pty.slave_path,
            command,
            identity,
            permissions,
            drop_to_identity,
        );
        std::process::exit(code);
    }
    Ok(pid)
}

fn child_main(
    slave_path: &str,
    command: &[String],
    identity: &DevIdentity,
    permissions: GuestPermissions,
    drop_to_identity: bool,
) -> i32 {
    if let Err(err) =
        child_main_result(slave_path, command, identity, permissions, drop_to_identity)
    {
        eprintln!("cang-guest-init managed session child: {err:#}");
        127
    } else {
        0
    }
}

fn child_main_result(
    slave_path: &str,
    command: &[String],
    identity: &DevIdentity,
    permissions: GuestPermissions,
    drop_to_identity: bool,
) -> Result<()> {
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create managed session");
    }
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(slave_path)
        .with_context(|| format!("failed to open PTY slave {slave_path}"))?;
    if unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set controlling PTY");
    }
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        if unsafe { libc::dup2(slave.as_raw_fd(), fd) } < 0 {
            return Err(std::io::Error::last_os_error()).context("failed to wire PTY stdio");
        }
    }
    if drop_to_identity {
        process::drop_to_identity_and_exec(identity, permissions, command)
    } else {
        process::exec_command(command)
    }
}

fn apply_initial_winsize(fd: RawFd, initial_size: Option<PtySize>) -> Result<PtySize> {
    let effective_size = initial_size.unwrap_or_default();
    set_winsize(fd, effective_size.rows, effective_size.cols)?;
    Ok(effective_size)
}

fn set_winsize(fd: RawFd, rows: u16, cols: u16) -> Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to resize managed PTY");
    }
    Ok(())
}

fn duplicate_file(file: &File) -> Result<File> {
    duplicate_fd(file.as_raw_fd())
}

fn duplicate_fd(fd: RawFd) -> Result<File> {
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to duplicate fd");
    }
    Ok(unsafe { File::from_raw_fd(dup) })
}

fn set_nonblocking(fd: RawFd, enabled: bool) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to read fd flags");
    }
    let next = if enabled {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, next) } < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to update fd flags");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_init::runtime::attach_profile;
    use std::ffi::{CStr, CString};
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};

    #[test]
    fn managed_session_rejects_protocol_mismatch() {
        assert_ne!(
            ManagedSessionConfig {
                port: 1,
                protocol_version: PROTOCOL_VERSION + 1,
                attach_profile: false,
            },
            ManagedSessionConfig {
                port: 1,
                protocol_version: PROTOCOL_VERSION,
                attach_profile: false,
            }
        );
    }

    #[test]
    fn pre_start_detach_is_readiness_only() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handle_pre_start_client(server));

        assert_eq!(
            read_frame(&mut client).unwrap(),
            Some(Frame::Hello {
                version: PROTOCOL_VERSION
            })
        );
        write_frame(&mut client, &Frame::Detach).unwrap();

        match server.join().unwrap().unwrap() {
            PreStartClientResult::Wait => {}
            PreStartClientResult::Start { .. } => panic!("readiness detach started command"),
        }
    }

    #[test]
    fn pre_start_resize_does_not_start_command() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handle_pre_start_client(server));

        assert_eq!(
            read_frame(&mut client).unwrap(),
            Some(Frame::Hello {
                version: PROTOCOL_VERSION
            })
        );
        write_frame(
            &mut client,
            &Frame::Resize {
                rows: 40,
                cols: 120,
            },
        )
        .unwrap();
        write_frame(&mut client, &Frame::Detach).unwrap();

        match server.join().unwrap().unwrap() {
            PreStartClientResult::Wait => {}
            PreStartClientResult::Start { .. } => panic!("pre-attach resize started command"),
        }
    }

    #[test]
    fn pre_start_attach_returns_start_with_latest_resize() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handle_pre_start_client(server));

        assert_eq!(
            read_frame(&mut client).unwrap(),
            Some(Frame::Hello {
                version: PROTOCOL_VERSION
            })
        );
        write_frame(&mut client, &Frame::Resize { rows: 24, cols: 80 }).unwrap();
        write_frame(
            &mut client,
            &Frame::Resize {
                rows: 50,
                cols: 160,
            },
        )
        .unwrap();
        write_frame(&mut client, &Frame::Attach).unwrap();

        match server.join().unwrap().unwrap() {
            PreStartClientResult::Start { initial_size, .. } => {
                assert_eq!(
                    initial_size,
                    Some(PtySize {
                        rows: 50,
                        cols: 160
                    })
                );
            }
            PreStartClientResult::Wait => panic!("real attach did not start command"),
        }
    }

    #[test]
    fn pre_start_invalid_frame_errors_without_starting() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handle_pre_start_client(server));

        assert_eq!(
            read_frame(&mut client).unwrap(),
            Some(Frame::Hello {
                version: PROTOCOL_VERSION
            })
        );
        write_frame(&mut client, &Frame::Data(b"lost input".to_vec())).unwrap();
        match read_frame(&mut client).unwrap() {
            Some(Frame::Error(message)) => assert!(message.contains("expected attach frame")),
            frame => panic!("expected error frame, got {frame:?}"),
        }

        match server.join().unwrap().unwrap() {
            PreStartClientResult::Wait => {}
            PreStartClientResult::Start { .. } => panic!("invalid pre-start data started command"),
        }
    }

    fn read_pty_winsize(fd: RawFd) -> Result<PtySize> {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to read PTY winsize");
        }
        Ok(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
        })
    }

    #[test]
    fn initial_winsize_defaults_kernel_pty_when_missing() {
        let pty = Pty::open().unwrap();

        let effective_size = apply_initial_winsize(pty.master.as_raw_fd(), None).unwrap();

        assert_eq!(effective_size, PtySize::default());
        assert_eq!(
            read_pty_winsize(pty.master.as_raw_fd()).unwrap(),
            PtySize::default()
        );
    }

    #[test]
    fn initial_winsize_preserves_explicit_kernel_pty_size() {
        let pty = Pty::open().unwrap();
        let explicit_size = PtySize {
            rows: 50,
            cols: 160,
        };

        let effective_size =
            apply_initial_winsize(pty.master.as_raw_fd(), Some(explicit_size)).unwrap();

        assert_eq!(effective_size, explicit_size);
        assert_eq!(
            read_pty_winsize(pty.master.as_raw_fd()).unwrap(),
            explicit_size
        );
    }

    #[test]
    fn attached_client_receives_initial_pty_output() {
        let pty = Pty::open().unwrap();
        let slave = pty_slave_cstring(&pty);
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            close_inherited_fds();
            let code = write_then_wait_on_pty_slave(&slave, b"primary-da-visible\n");
            // SAFETY: a forked child of this multi-threaded harness must leave
            // through `_exit`; `std::process::exit` runs the stdio flush that can
            // deadlock on a lock another harness thread held at fork time.
            unsafe { libc::_exit(code) }
        }

        let temp = tempfile::tempdir().unwrap();
        let unix_listener = UnixListener::bind(temp.path().join("listener.sock")).unwrap();
        let listener = VsockListener {
            fd: unix_listener.into_raw_fd(),
        };
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_file = unsafe { File::from_raw_fd(server.into_raw_fd()) };

        let server = thread::spawn(move || {
            let mut terminal_state = TerminalState::new(PtySize::default());
            serve_attached_client(
                &pty.master,
                child,
                server_file,
                &listener,
                &mut terminal_state,
                false,
                PtyForwardingMode::Normalized,
            )
        });

        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert_data_frames_eq(&mut client, b"primary-da-visible\r\n");
        write_frame(&mut client, &Frame::Detach).unwrap();
        assert_eq!(server.join().unwrap().unwrap(), ClientResult::Detached);
        // The child waits for this signal, so the detach above always landed on
        // a live session; end it the way `run_sigwinch_redraw_pty_child` is ended.
        assert_eq!(unsafe { libc::kill(child, libc::SIGTERM) }, 0);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGTERM);
    }

    #[test]
    fn final_pty_output_is_forwarded_after_the_writers_close() {
        let pty = Pty::open().unwrap();
        {
            let mut slave = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pty.slave_path.as_str())
                .unwrap();
            slave.write_all(b"final-visible\n").unwrap();
        }

        let (mut peer, client) = UnixStream::pair().unwrap();
        let mut client = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        let mut master = pty.master;
        let mut terminal_state = TerminalState::new(PtySize::default());
        let mut profiler = GuestAttachProfiler::new(false);
        forward_final_attached_pty_output(
            &mut master,
            &mut [0u8; IO_BUF_SIZE],
            &mut client,
            &mut terminal_state,
            &mut profiler,
            PtyForwardingMode::Normalized,
        )
        .unwrap();

        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(
            read_frame(&mut peer).unwrap(),
            Some(Frame::Data(b"final-visible\r\n".to_vec()))
        );
    }

    #[test]
    fn terminal_state_can_normalize_before_parser_record() {
        let mut terminal_state = TerminalState::new(PtySize::default());

        let normalized = terminal_state.normalize_output(b"\x1b(0qx\x1b(B\n");

        assert_eq!(normalized, "─│\n".as_bytes());
        assert!(!terminal_state.parser.screen().contents().contains("─│"));
        terminal_state.record_normalized_output(&normalized);
        assert!(terminal_state.parser.screen().contents().contains("─│"));
    }

    #[test]
    fn terminal_state_record_output_preserves_combined_behavior() {
        let mut terminal_state = TerminalState::new(PtySize::default());

        let live_data = terminal_state.record_output(b"\x1b(0qx\x1b(B\n");

        assert_eq!(live_data, "─│\n".as_bytes());
        assert!(terminal_state.parser.screen().contents().contains("─│"));
    }

    #[test]
    fn pty_raw_passthrough_mode_requires_truthy_value() {
        assert_eq!(
            PtyForwardingMode::from_env_value(None),
            PtyForwardingMode::Normalized
        );
        assert_eq!(
            PtyForwardingMode::from_env_value(Some("0")),
            PtyForwardingMode::Normalized
        );
        assert_eq!(
            PtyForwardingMode::from_env_value(Some("summary")),
            PtyForwardingMode::Normalized
        );
        assert_eq!(
            PtyForwardingMode::from_env_value(Some("true")),
            PtyForwardingMode::RawPassthrough
        );
    }

    #[test]
    fn attached_output_is_written_before_parser_record_on_success() {
        let mut terminal_state = TerminalState::new(PtySize::default());
        let mut profiler = GuestAttachProfiler::new(true);
        let mut writer = Vec::new();

        let forwarded = normalize_and_write_pty_output(
            b"\x1b(0qx\x1b(B\n",
            &mut writer,
            &mut terminal_state,
            &mut profiler,
            PtyForwardingMode::Normalized,
        );

        assert!(forwarded.write_succeeded);
        assert_eq!(forwarded.parser_bytes.len(), "─│\n".len());
        assert!(!terminal_state.parser.screen().contents().contains("─│"));
        let mut cursor = std::io::Cursor::new(writer);
        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Some(Frame::Data("─│\n".as_bytes().to_vec()))
        );

        record_forwarded_pty_output(&forwarded, &mut terminal_state, &mut profiler);
        assert!(terminal_state.parser.screen().contents().contains("─│"));
    }

    #[test]
    fn raw_passthrough_writes_original_bytes_while_recording_normalized_parser_state() {
        let mut terminal_state = TerminalState::new(PtySize::default());
        let mut profiler = GuestAttachProfiler::new(true);
        let mut writer = Vec::new();
        let pty_bytes = b"\x1b(0qx\x1b(B\n";

        let forwarded = normalize_and_write_pty_output(
            pty_bytes,
            &mut writer,
            &mut terminal_state,
            &mut profiler,
            PtyForwardingMode::RawPassthrough,
        );

        assert!(forwarded.write_succeeded);
        assert_eq!(forwarded.parser_bytes, "─│\n".as_bytes());
        assert!(!terminal_state.parser.screen().contents().contains("─│"));
        let mut cursor = std::io::Cursor::new(writer);
        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Some(Frame::Data(pty_bytes.to_vec()))
        );

        record_forwarded_pty_output(&forwarded, &mut terminal_state, &mut profiler);
        assert!(terminal_state.parser.screen().contents().contains("─│"));

        let mut profile_output = Vec::new();
        profiler.report_to(&mut profile_output).unwrap();
        let profile_output = String::from_utf8(profile_output).unwrap();
        assert!(profile_output.contains("frames=1"));
        assert!(profile_output.contains("frame_bytes=9"));
        assert!(profile_output.contains("frame_max_bytes=9"));
    }

    #[test]
    fn attached_output_records_parser_state_after_write_failure() {
        let mut terminal_state = TerminalState::new(PtySize::default());
        let mut profiler = GuestAttachProfiler::new(true);
        let mut writer = FailAfterSuccessfulWrites::new(1);

        let forwarded = normalize_and_write_pty_output(
            b"\x1b(0qx\x1b(B\n",
            &mut writer,
            &mut terminal_state,
            &mut profiler,
            PtyForwardingMode::Normalized,
        );

        assert!(!forwarded.write_succeeded);
        assert!(!terminal_state.parser.screen().contents().contains("─│"));
        assert_eq!(writer.successful_writes(), 1);

        record_forwarded_pty_output(&forwarded, &mut terminal_state, &mut profiler);
        assert!(terminal_state.parser.screen().contents().contains("─│"));
    }

    #[test]
    fn attached_pty_burst_coalesces_ordered_chunks() {
        let mut reader = ScriptedReader::new([
            ReadStep::Data(b"\x1b("),
            ReadStep::Data(b"0q"),
            ReadStep::WouldBlock,
        ]);
        let mut buf = [0u8; IO_BUF_SIZE];
        let mut profiler = GuestAttachProfiler::new(true);

        let burst = read_attached_pty_burst(
            &mut reader,
            &mut buf,
            &mut profiler,
            AttachedPtyDrainLimits {
                max_reads: 4,
                max_bytes: 64,
                max_elapsed: Duration::from_secs(1),
            },
        )
        .unwrap();

        assert_eq!(
            burst,
            AttachedPtyBurst {
                bytes: b"\x1b(0q".to_vec(),
                reads: 2,
                stop_reason: AttachedPtyDrainStop::WouldBlock,
            }
        );

        let mut terminal_state = TerminalState::new(PtySize::default());
        let mut writer = Vec::new();
        assert!(forward_and_record_pty_output(
            &burst.bytes,
            &mut writer,
            &mut terminal_state,
            &mut profiler,
            PtyForwardingMode::Normalized,
        ));
        let mut cursor = std::io::Cursor::new(writer);
        assert_eq!(
            read_frame(&mut cursor).unwrap(),
            Some(Frame::Data("─".as_bytes().to_vec()))
        );
        assert!(terminal_state.parser.screen().contents().contains("─"));

        let mut profile_output = Vec::new();
        profiler.report_to(&mut profile_output).unwrap();
        let profile_output = String::from_utf8(profile_output).unwrap();
        assert!(profile_output.contains("pty_drain_events=1"));
        assert!(profile_output.contains("pty_drain_would_block_count=1"));
        assert!(profile_output.contains("pty_drain_coalesced_events=1"));
        assert!(profile_output.contains("pty_drain_coalesced_reads=1"));
        assert!(profile_output.contains("frames=1"));
    }

    #[test]
    fn attached_pty_burst_first_would_block_is_empty_success() {
        let mut reader = ScriptedReader::new([ReadStep::WouldBlock]);
        let mut buf = [0u8; IO_BUF_SIZE];
        let mut profiler = GuestAttachProfiler::new(true);

        let burst = read_attached_pty_burst(
            &mut reader,
            &mut buf,
            &mut profiler,
            AttachedPtyDrainLimits {
                max_reads: 4,
                max_bytes: 64,
                max_elapsed: Duration::from_secs(1),
            },
        )
        .unwrap();

        assert_eq!(
            burst,
            AttachedPtyBurst {
                bytes: Vec::new(),
                reads: 0,
                stop_reason: AttachedPtyDrainStop::WouldBlock,
            }
        );
    }

    #[test]
    fn attached_pty_burst_stops_at_read_bound() {
        let mut reader = ScriptedReader::new([
            ReadStep::Data(b"a"),
            ReadStep::Data(b"b"),
            ReadStep::Data(b"c"),
        ]);
        let mut buf = [0u8; IO_BUF_SIZE];
        let mut profiler = GuestAttachProfiler::new(true);

        let burst = read_attached_pty_burst(
            &mut reader,
            &mut buf,
            &mut profiler,
            AttachedPtyDrainLimits {
                max_reads: 2,
                max_bytes: 64,
                max_elapsed: Duration::from_secs(1),
            },
        )
        .unwrap();

        assert_eq!(
            burst,
            AttachedPtyBurst {
                bytes: b"ab".to_vec(),
                reads: 2,
                stop_reason: AttachedPtyDrainStop::ReadBound,
            }
        );
        let mut profile_output = Vec::new();
        profiler.report_to(&mut profile_output).unwrap();
        let profile_output = String::from_utf8(profile_output).unwrap();
        assert!(profile_output.contains("pty_drain_bound_hit_count=1"));
        assert!(profile_output.contains("pty_drain_reads_max=2"));
    }

    #[test]
    fn attached_pty_burst_stops_at_byte_bound() {
        let mut reader = ScriptedReader::new([
            ReadStep::Data(b"ab"),
            ReadStep::Data(b"cd"),
            ReadStep::WouldBlock,
        ]);
        let mut buf = [0u8; IO_BUF_SIZE];
        let mut profiler = GuestAttachProfiler::new(false);

        let burst = read_attached_pty_burst(
            &mut reader,
            &mut buf,
            &mut profiler,
            AttachedPtyDrainLimits {
                max_reads: 4,
                max_bytes: 3,
                max_elapsed: Duration::from_secs(1),
            },
        )
        .unwrap();

        assert_eq!(
            burst,
            AttachedPtyBurst {
                bytes: b"abc".to_vec(),
                reads: 2,
                stop_reason: AttachedPtyDrainStop::ByteBound,
            }
        );
    }

    #[test]
    fn pty_input_write_retries_transient_would_block() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let read_fd = fds[0];
        let write_fd = fds[1];
        set_nonblocking(write_fd, true).unwrap();
        let mut write_file = unsafe { File::from_raw_fd(write_fd) };
        let mut read_file = unsafe { File::from_raw_fd(read_fd) };
        let fill = [0u8; 4096];
        loop {
            match write_file.write(&fill) {
                Ok(0) => panic!("pipe write returned zero while filling"),
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to fill pipe: {err}"),
            }
        }

        let reader = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut drained = [0u8; 8192];
            let n = read_file.read(&mut drained).unwrap();
            thread::sleep(Duration::from_millis(100));
            n
        });

        write_all_retrying_would_block(&mut write_file, b"x").unwrap();

        assert!(reader.join().unwrap() > 0);
    }

    #[test]
    fn terminal_state_returns_normalized_output_for_live_proxy() {
        let mut terminal_state = TerminalState::new(PtySize::default());

        let live_data = terminal_state.record_output(b"\x1b(0qx\x1b(B\n");

        assert_eq!(live_data, "─│\n".as_bytes());
        assert!(terminal_state.parser.screen().contents().contains("─│"));
    }

    #[test]
    fn terminal_state_keeps_recording_after_resize_to_one_row() {
        let mut terminal_state = TerminalState::new(PtySize::default());
        terminal_state.record_output(b"\x1b[2;10r\x1b[?1049hhello\r\nworld\x1b[?1049l");

        terminal_state.resize(PtySize { rows: 1, cols: 2 });
        terminal_state.record_output(b"output\r\n\x1b[999;999H");
    }

    #[test]
    fn terminal_restore_repaints_visible_screen_and_cursor_state() {
        let mut terminal_state = TerminalState::new(PtySize { rows: 10, cols: 40 });
        terminal_state.record_output(b"prompt> abc");
        terminal_state.record_output(b"\x1b[3DXYZ");

        let restored = restored_screen(&terminal_state);

        assert!(restored.screen().contents().contains("prompt> XYZ"));
        assert_eq!(
            restored.screen().cursor_position(),
            terminal_state.parser.screen().cursor_position()
        );
    }

    #[test]
    fn terminal_restore_clears_stale_cells() {
        let mut terminal_state = TerminalState::new(PtySize { rows: 5, cols: 40 });
        terminal_state.record_output(b"short");

        let mut restored = vt100::Parser::new(5, 40, 0);
        restored.process(b"this line used to be much longer\r\nstale lower row");
        restored.process(&terminal_state.render_restore());

        let first_row = restored.screen().rows(0, 40).next().unwrap();
        assert_eq!(first_row.trim_end(), "short");
        assert!(!first_row.contains("longer"));
        assert!(!restored.screen().contents().contains("stale lower row"));
    }

    #[test]
    fn terminal_restore_renders_acs_borders_as_unicode() {
        let mut terminal_state = TerminalState::new(PtySize { rows: 5, cols: 20 });
        terminal_state.record_output(b"\x1b(0lqqk\r\nmqqj\x1b(B");

        let restored = restored_screen(&terminal_state);
        let contents = restored.screen().contents();

        assert!(contents.contains("┌──┐"), "{contents:?}");
        assert!(contents.contains("└──┘"), "{contents:?}");
        assert!(!contents.contains("lqqk"), "{contents:?}");
        assert!(!contents.contains("mqqj"), "{contents:?}");
    }

    #[test]
    fn terminal_restore_preserves_visual_and_input_state() {
        let mut terminal_state = TerminalState::new(PtySize { rows: 10, cols: 40 });
        terminal_state.record_output(
            b"\x1b[?1049h\x1b[?25l\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006hinside-tui",
        );

        let restore = terminal_state.render_restore();
        for input_mode in [
            b"\x1b[?1h".as_slice(),
            b"\x1b[?1002h",
            b"\x1b[?1006h",
            b"\x1b[?2004h",
        ] {
            assert!(
                restore
                    .windows(input_mode.len())
                    .any(|bytes| bytes == input_mode),
                "restore is missing cached input mode {input_mode:?}"
            );
        }

        let mut restored = vt100::Parser::new(10, 40, 0);
        restored.process(&restore);
        let screen = restored.screen();

        assert!(screen.alternate_screen());
        assert!(screen.hide_cursor());
        assert!(screen.application_cursor());
        assert!(screen.bracketed_paste());
        assert_eq!(
            screen.mouse_protocol_mode(),
            vt100::MouseProtocolMode::ButtonMotion
        );
        assert_eq!(
            screen.mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Sgr
        );
        assert!(screen.contents().contains("inside-tui"));
    }

    #[test]
    fn post_start_attach_resize_updates_restore_dimensions() {
        let pty = Pty::open().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let mut server_file = unsafe { File::from_raw_fd(server.into_raw_fd()) };

        let server = thread::spawn(move || {
            let mut terminal_state = TerminalState::new(PtySize::default());
            let attached =
                read_post_start_attach_request(&pty.master, &mut server_file, &mut terminal_state)
                    .unwrap();
            (attached, terminal_state.size())
        });

        write_frame(
            &mut client,
            &Frame::Resize {
                rows: 42,
                cols: 132,
            },
        )
        .unwrap();
        write_frame(&mut client, &Frame::Attach).unwrap();

        let (attached, size) = server.join().unwrap();
        assert!(attached);
        assert_eq!(
            size,
            PtySize {
                rows: 42,
                cols: 132
            }
        );
    }

    #[test]
    fn post_start_attach_sends_detached_restore_before_live_proxy() {
        let pty = Pty::open().unwrap();
        set_winsize(pty.master.as_raw_fd(), 24, 80).unwrap();
        let slave = pty_slave_cstring(&pty);
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            close_inherited_fds();
            let code = run_sigwinch_redraw_pty_child(&slave);
            // SAFETY: see `attached_client_receives_initial_pty_output`.
            unsafe { libc::_exit(code) }
        }
        thread::sleep(Duration::from_millis(50));

        let mut terminal_state = TerminalState::new(PtySize::default());
        drain_detached_pty_output(pty.master.as_raw_fd(), &mut terminal_state).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let unix_listener = UnixListener::bind(temp.path().join("listener.sock")).unwrap();
        let listener = VsockListener {
            fd: unix_listener.into_raw_fd(),
        };
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_file = unsafe { File::from_raw_fd(server.into_raw_fd()) };

        let server = thread::spawn(move || {
            serve_client(
                &pty.master,
                child,
                server_file,
                &listener,
                &mut terminal_state,
                false,
                PtyForwardingMode::Normalized,
            )
        });

        assert_eq!(
            read_frame(&mut client).unwrap(),
            Some(Frame::Hello {
                version: PROTOCOL_VERSION
            })
        );
        write_frame(&mut client, &Frame::Resize { rows: 24, cols: 80 }).unwrap();
        write_frame(&mut client, &Frame::Attach).unwrap();

        match read_frame(&mut client).unwrap() {
            Some(Frame::Data(data)) => {
                for mouse_mode in [b"\x1b[?1003h".as_slice(), b"\x1b[?1006h"] {
                    assert!(
                        data.windows(mouse_mode.len())
                            .any(|bytes| bytes == mouse_mode),
                        "restore is missing cached mouse mode {mouse_mode:?}"
                    );
                }
                let mut restored = vt100::Parser::new(24, 80, 0);
                restored.process(&data);
                assert!(restored.screen().contents().contains("detached-visible"));
                assert!(!restored.screen().contents().contains("redraw-visible"));
            }
            frame => panic!("expected restore frame, got {frame:?}"),
        }
        assert_data_frames_eq(&mut client, b"redraw-visible\r\n");
        thread::sleep(Duration::from_millis(550));
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) },
            0
        );

        write_frame(&mut client, &Frame::Detach).unwrap();
        assert_eq!(server.join().unwrap().unwrap(), ClientResult::Detached);
        assert_eq!(unsafe { libc::kill(child, libc::SIGTERM) }, 0);
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    }

    #[test]
    fn normalizer_maps_full_tmux_base_acs_table() {
        let mut normalizer = TerminalOutputNormalizer::default();
        let input = b"\x1b(0+,-.0`abcdefghijklmnopqrstuvwxyz{|}~";

        let normalized = normalizer.normalize(input);

        assert_eq!(
            String::from_utf8(normalized).unwrap(),
            "→←↑↓▮◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·"
        );
    }

    #[test]
    fn normalizer_preserves_pending_designation_across_reads() {
        let mut normalizer = TerminalOutputNormalizer::default();

        assert!(normalizer.normalize(b"\x1b(").is_empty());
        assert_eq!(normalizer.normalize(b"0q"), "─".as_bytes());
        assert!(normalizer.normalize(b"\x1b)").is_empty());
        assert!(normalizer.normalize(b"0").is_empty());
        assert_eq!(normalizer.normalize(b"\x0ex"), "│".as_bytes());
    }

    #[test]
    fn normalizer_tracks_g1_shift_in_and_shift_out() {
        let mut normalizer = TerminalOutputNormalizer::default();

        let normalized = normalizer.normalize(b"\x1b)0\x0ex\x0fx");

        assert_eq!(normalized, "│x".as_bytes());
    }

    #[test]
    fn normalizer_does_not_reset_charset_on_sgr() {
        let mut normalizer = TerminalOutputNormalizer::default();

        let normalized = normalizer.normalize(b"\x1b(0q\x1b[0mq\x1b(Bq");

        assert_eq!(normalized, "\u{2500}\x1b[0m\u{2500}q".as_bytes());
    }

    #[test]
    fn normalizer_passes_unrelated_escape_sequences_through() {
        let mut normalizer = TerminalOutputNormalizer::default();

        let normalized = normalizer.normalize(b"\x1b(0\x1b%Gq\x1b]0;x\x07q");

        assert_eq!(normalized, "\x1b%G─\x1b]0;x\x07─".as_bytes());
    }

    #[test]
    fn normalizer_passes_utf8_bytes_through() {
        let mut normalizer = TerminalOutputNormalizer::default();

        let normalized = normalizer.normalize("é\x1b(0qé".as_bytes());

        assert_eq!(normalized, "é─é".as_bytes());
    }

    struct FailAfterSuccessfulWrites {
        successful_writes_before_failure: usize,
        successful_writes: usize,
    }

    impl FailAfterSuccessfulWrites {
        fn new(successful_writes_before_failure: usize) -> Self {
            Self {
                successful_writes_before_failure,
                successful_writes: 0,
            }
        }

        fn successful_writes(&self) -> usize {
            self.successful_writes
        }
    }

    impl Write for FailAfterSuccessfulWrites {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.successful_writes >= self.successful_writes_before_failure {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "intentional test write failure",
                ));
            }
            self.successful_writes += 1;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum ReadStep {
        Data(&'static [u8]),
        WouldBlock,
    }

    struct ScriptedReader {
        steps: std::collections::VecDeque<ReadStep>,
    }

    impl ScriptedReader {
        fn new<const N: usize>(steps: [ReadStep; N]) -> Self {
            Self {
                steps: steps.into(),
            }
        }
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.steps.pop_front() {
                Some(ReadStep::Data(data)) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    if n < data.len() {
                        self.steps.push_front(ReadStep::Data(&data[n..]));
                    }
                    Ok(n)
                }
                Some(ReadStep::WouldBlock) | None => Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "scripted would block",
                )),
            }
        }
    }

    /// True when an error chain bottoms out in a nonblocking read.
    fn is_would_block(err: &anyhow::Error) -> bool {
        err.chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .any(|io| io.kind() == std::io::ErrorKind::WouldBlock)
    }

    fn assert_data_frames_eq<T>(client: &mut T, expected: &[u8])
    where
        T: Read,
    {
        let mut received = Vec::new();
        // The harness runs hundreds of tests in parallel, so a client read can
        // hit its timeout (`WouldBlock`) while the frame is still on its way.
        // Keep waiting until the deadline rather than reporting a slow machine
        // as a protocol failure.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while received.len() < expected.len() {
            match read_frame(client) {
                Ok(Some(Frame::Data(data))) => received.extend(data),
                Ok(frame) => panic!("expected PTY data frame, got {frame:?}"),
                Err(err) if is_would_block(&err) && std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("failed to read PTY data frame: {err:#}"),
            }
        }
        assert_eq!(received, expected);
    }

    fn restored_screen(terminal_state: &TerminalState) -> vt100::Parser {
        let size = terminal_state.size();
        let mut restored = vt100::Parser::new(size.rows, size.cols, 0);
        restored.process(&terminal_state.render_restore());
        restored
    }

    /// Drop every inherited descriptor above stdio in a forked test child.
    ///
    /// A forked child keeps the harness's own descriptors alive: a copy of another
    /// test's pipe write end delays that test's EOF, and a copy of a file another
    /// test is about to `exec` makes that `exec` fail with `ETXTBSY`. Run this
    /// first thing in the child, before it opens anything of its own.
    fn close_inherited_fds() {
        // SAFETY: `close_range` is a plain syscall on fd numbers 3.., which keeps
        // stdio and cannot race with the parent's own descriptor table.
        unsafe {
            libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, 0_u32);
        }
    }

    /// The PTY slave path as a C string.
    ///
    /// Built before the `fork()`: the forked child only calls raw syscalls, so it
    /// must not allocate a `CString` of its own.
    fn pty_slave_cstring(pty: &Pty) -> CString {
        CString::new(pty.slave_path.as_str()).expect("PTY slave path has no NUL byte")
    }

    /// `write(2)` until every byte of `data` is out, for use inside a forked test
    /// child where `std::io::Write` helpers are not safe to call.
    fn write_all_fd(fd: libc::c_int, data: &[u8]) -> bool {
        let mut written = 0;
        while written < data.len() {
            // SAFETY: writing an initialized byte slice to an open fd.
            let n =
                unsafe { libc::write(fd, data[written..].as_ptr().cast(), data.len() - written) };
            if n <= 0 {
                return false;
            }
            written += n as usize;
        }
        true
    }

    /// Write `data` to the PTY slave and then hold it open until the test signals
    /// this child.
    ///
    /// Holding it open until a signal - rather than for a fixed nap - keeps the
    /// client's detach landing while the session is still serving it on a loaded
    /// machine. Only a setup failure returns, exactly like
    /// `run_sigwinch_redraw_pty_child`; the parent reaps the child afterwards.
    ///
    /// This runs in a forked child of the multi-threaded test harness, so it stays
    /// syscall-only. `fork()` copies every lock the other harness threads held at
    /// that instant - including glibc's malloc arenas - so an allocation here can
    /// block forever and hang the parent's `waitpid`.
    fn write_then_wait_on_pty_slave(slave: &CStr, data: &[u8]) -> i32 {
        // SAFETY: opening the PTY slave path read-write for this child only.
        let fd = unsafe { libc::open(slave.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return 1;
        }
        if !write_all_fd(fd, data) {
            return 1;
        }
        loop {
            unsafe { libc::pause() };
        }
    }

    extern "C" fn write_redraw_marker(_signal: libc::c_int) {
        let marker = b"redraw-visible\n";
        unsafe {
            libc::write(libc::STDOUT_FILENO, marker.as_ptr().cast(), marker.len());
        }
    }

    /// Make the PTY this child's controlling terminal, report every window resize
    /// on it, and then idle until the parent kills the child.
    ///
    /// Syscall-only for the same reason as `write_then_sleep_on_pty_slave`; success
    /// never returns, and a non-zero return is a setup failure.
    fn run_sigwinch_redraw_pty_child(slave: &CStr) -> i32 {
        if unsafe { libc::setsid() } < 0 {
            return 1;
        }
        // SAFETY: opening the PTY slave path read-write for this child only.
        let fd = unsafe { libc::open(slave.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return 1;
        }
        if unsafe { libc::ioctl(fd, libc::TIOCSCTTY, 0) } != 0 {
            return 1;
        }
        let pgid = unsafe { libc::getpgrp() };
        if unsafe { libc::tcsetpgrp(fd, pgid) } != 0 {
            return 1;
        }
        if unsafe { libc::dup2(fd, libc::STDOUT_FILENO) } < 0 {
            return 1;
        }

        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = write_redraw_marker as *const () as usize;
        if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0
            || unsafe { libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut()) } != 0
        {
            return 1;
        }

        if !write_all_fd(
            fd,
            b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006hdetached-visible\n",
        ) {
            return 1;
        }
        loop {
            unsafe { libc::pause() };
        }
    }

    #[test]
    fn managed_session_config_carries_attach_profile_flag() {
        assert_ne!(
            ManagedSessionConfig {
                port: 1,
                protocol_version: PROTOCOL_VERSION,
                attach_profile: true,
            },
            ManagedSessionConfig {
                port: 1,
                protocol_version: PROTOCOL_VERSION,
                attach_profile: false,
            }
        );
    }

    #[test]
    fn attach_profile_env_cleanup_removes_child_leak_flag() {
        // SAFETY: this test mutates one process environment key and restores the
        // disabled state before returning.
        unsafe { std::env::set_var(attach_profile::ATTACH_PROFILE_ENV, "1") };
        assert_eq!(
            std::env::var(attach_profile::ATTACH_PROFILE_ENV).as_deref(),
            Ok("1")
        );

        attach_profile::clear_process_env();

        assert!(std::env::var(attach_profile::ATTACH_PROFILE_ENV).is_err());
    }
}
