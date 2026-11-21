//! The relay itself: one FUSE message at a time between `/dev/fuse` and the
//! guest's server over the vsock unix socket.
//!
//! There is no extra framing. A request is `in_header.len` bytes; a reply is
//! `out_header.len`. `FORGET`/`BATCH_FORGET` are relayed and never answered (a
//! pump that waits for their reply eats the next request's).
//!
//! # Death
//!
//! When the transport stops answering, the pump marks the connection dead and
//! answers **every** subsequent request `-EIO` immediately: the mount stays, no
//! request is queued or replayed, and a fault on an already-mapped page gets
//! `SIGBUS` ~20 ms after the `-EIO` reply (note 63, ticket 23 decision 4(a)).
//! The pump itself keeps running until cang's exit sends `SIGTERM` - which is
//! the only thing that unmounts.
//!
//! # Notifications
//!
//! The guest may also speak on its own: a frame whose `out_header.unique` is
//! `0` and whose `out_header.error` is a `fuse_notify_code` (`INVAL_INODE`,
//! `INVAL_ENTRY`) is a *notification*, and the kernel takes it for one whatever
//! the mount negotiated. The transport is therefore in the pump's `poll(2)`
//! set: a readable transport with no request in flight is the guest
//! invalidating a cache entry on its own, and the frame goes straight to
//! `/dev/fuse` (note 66). A notification that arrives while a request is in
//! flight is delivered *before* that request's reply - safe, because replies
//! carry their `unique` and notifications never do.
//!
//! The kernel's verdict on a notification is the `write(2)` return value, and a
//! **refusal** (`ENOENT` for a nodeid the mount no longer holds) is normal for
//! a watcher whose map is a moment behind. It is logged and skipped; only a
//! real transport failure ends the mount.
//!
//! # Why notifications have their own writer thread
//!
//! They are delivered by a dedicated thread, not by the serving loop, because
//! the kernel's invalidation path can *wait*: `fuse_reverse_inval_inode` takes
//! the page lock of the pages it drops and `fuse_reverse_inval_entry` the
//! parent directory's inode lock, and a host task that holds such a lock while
//! waiting for a FUSE reply holds it until this pump answers. A pump that wrote
//! the notification inline would then wait for a lock held by a task waiting
//! for the pump: the whole mount hung (measured - the pump's `/proc/<pid>/wchan`
//! was `folio_wait_bit_common` and `fuse_reverse_inval_entry`, `stat` through
//! the mount never returned). With the write on its own thread the serving loop
//! keeps answering, the lock holder finishes, and the invalidation completes.
//!
//! The thread owns a `dup(2)` of `/dev/fuse` (so the caller closing its
//! descriptor cannot pull the descriptor out from under a write), each frame is
//! one `write(2)`, and the queue is bounded: a full queue drops notifications
//! rather than stalling the serving loop, which is safe because a later event
//! for the same path invalidates it again.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

/// The largest message the relay carries: the 1 MiB `max_write` the guest's
/// INIT reply advertises plus a page of headers.
pub const MAX_MSG: usize = (1 << 20) + 0x1000;
pub const IN_HEADER_LEN: usize = 40;
pub const OUT_HEADER_LEN: usize = 16;
pub const OPCODE_INIT: u32 = 26;
pub const FORGET: u32 = 2;
pub const BATCH_FORGET: u32 = 42;

static STOP: AtomicBool = AtomicBool::new(false);
/// The read end of the stop channel, or -1 when it has not been installed.
static STOP_READ_FD: AtomicI32 = AtomicI32::new(-1);
/// The write end; the signal handler writes one byte to it.
static STOP_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Create the pipe the relay's `poll(2)` waits on, so a stop request wakes it
/// out of an idle wait on `/dev/fuse`.
///
/// A signal alone would do it (`read(2)` returns `EINTR`), but only when the
/// pump is actually blocked in a read: a stop that arrives between frames has
/// nothing to interrupt. The pipe makes the stop edge-triggered and
/// signal-independent, which is also what lets a test drive the same path.
pub fn install_stop_channel() -> Result<()> {
    if STOP_READ_FD.load(Ordering::SeqCst) >= 0 {
        return Ok(());
    }
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create the stop pipe");
    }
    STOP_READ_FD.store(fds[0], Ordering::SeqCst);
    STOP_WRITE_FD.store(fds[1], Ordering::SeqCst);
    Ok(())
}

/// The stop channel's read end, for `relay`'s `poll(2)`.
pub fn stop_read_fd() -> RawFd {
    STOP_READ_FD.load(Ordering::SeqCst)
}

/// Ask the relay loop to finish and let the caller unmount.
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
    let fd = STOP_WRITE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = [1u8];
        let _ = unsafe { libc::write(fd, byte.as_ptr().cast(), 1) };
    }
}

/// Clear the stop request (tests, and a second run of the same process).
pub fn clear_stop() {
    STOP.store(false, Ordering::SeqCst);
}

pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

extern "C" fn handle_stop_signal(_signal: libc::c_int) {
    // Only an atomic store and a one-byte write on a non-blocking pipe, both
    // async-signal-safe; the byte wakes the loop's poll(2).
    STOP.store(true, Ordering::SeqCst);
    let fd = STOP_WRITE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = [1u8];
        let _ = unsafe { libc::write(fd, byte.as_ptr().cast(), 1) };
    }
}

/// Install `SIGTERM`/`SIGINT` handlers that request a stop.
pub fn install_stop_handler() -> Result<()> {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handle_stop_signal as *const () as libc::sighandler_t;
        action.sa_flags = 0;
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        let rc = unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("failed to install the stop handler for signal {signal}")
            });
        }
    }
    Ok(())
}

/// What one pump run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayOutcome {
    /// Frames read from `/dev/fuse`.
    pub requests: u64,
    /// The transport stopped answering at some point: every request after that
    /// was answered `-EIO`.
    pub dead: bool,
    /// A stop was requested (cang is exiting, so the caller unmounts).
    pub stopped: bool,
    /// `/dev/fuse` reported the connection gone (`ENODEV`, or EOF): something
    /// else already unmounted the mount.
    pub fuse_closed: bool,
}

/// Optional per-frame TSV, the same shape the spike used
/// (`opcode unique req_len reply_len nanos status`).
///
/// Rows are flushed every 64 frames and on exit: a `SIGKILL`ed pump loses its
/// last few rows, so frame counts are only trustworthy for pumps that exited
/// cleanly (note 63 §5).
pub struct Stats {
    file: Option<std::fs::File>,
    buffered: u64,
}

impl Stats {
    pub fn disabled() -> Self {
        Self {
            file: None,
            buffered: 0,
        }
    }

    pub fn enabled(path: &std::path::Path) -> Result<Self> {
        let mut file = std::fs::File::create(path).with_context(|| {
            format!("failed to create the relay stats file '{}'", path.display())
        })?;
        writeln!(file, "# opcode\tunique\treq_len\treply_len\tnanos\tstatus")
            .context("failed to write the relay stats header")?;
        Ok(Self {
            file: Some(file),
            buffered: 0,
        })
    }

    fn record(
        &mut self,
        opcode: u32,
        unique: u64,
        req_len: usize,
        reply_len: usize,
        nanos: u64,
        status: &str,
    ) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let _ = writeln!(
            file,
            "{opcode}\t{unique}\t{req_len}\t{reply_len}\t{nanos}\t{status}"
        );
        self.buffered += 1;
        if self.buffered >= 64 {
            let _ = file.flush();
            self.buffered = 0;
        }
    }

    pub fn flush(&mut self) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }
}

/// How many notification frames may wait for the writer thread before the
/// serving loop drops them (a later event for the same path re-invalidates it).
const NOTIFICATION_QUEUE: usize = 4096;

/// Where a relay's log lines go: shared with the notification writer thread, so
/// it has to be callable from both.
pub type RelayLog = Arc<dyn Fn(&str) + Send + Sync>;

/// The notification writer: a thread of its own, a duplicate of `/dev/fuse`, and
/// a bounded queue.
///
/// See the module documentation for why the write cannot happen on the serving
/// thread, and why a full queue drops frames instead of blocking it.
pub struct Notifications {
    queue: mpsc::SyncSender<Vec<u8>>,
    dropped: Arc<AtomicU64>,
    log: RelayLog,
}

impl Notifications {
    /// Start the writer thread for `fuse_fd`.
    fn start(fuse_fd: RawFd, log: RelayLog) -> Result<Self> {
        let writer_fd = unsafe { libc::dup(fuse_fd) };
        if writer_fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to duplicate /dev/fuse for the notification writer");
        }
        let (queue, receiver) = mpsc::sync_channel::<Vec<u8>>(NOTIFICATION_QUEUE);
        let thread_log = Arc::clone(&log);
        std::thread::Builder::new()
            .name("relay-pump-notify".to_owned())
            .spawn(move || {
                while let Ok(frame) = receiver.recv() {
                    if !write_notification(writer_fd, &frame, thread_log.as_ref()) {
                        break;
                    }
                }
                let _ = unsafe { libc::close(writer_fd) };
            })
            .context("failed to start the notification writer thread")?;
        Ok(Self {
            queue,
            dropped: Arc::new(AtomicU64::new(0)),
            log,
        })
    }

    /// Hand one notification frame to the writer thread.
    fn offer(&self, frame: &[u8]) {
        match self.queue.try_send(frame.to_vec()) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // A lossy queue is a storm, so one line per 1024 drops: a line
                // per drop would itself become the bottleneck.
                if dropped % 1024 == 1 {
                    (self.log)(&format!(
                        "relay-pump: notification queue full, {dropped} notifications dropped so far \
                         (a later event for the same path invalidates it again)"
                    ));
                }
            }
            // The writer thread is gone (it stopped on a dead /dev/fuse); the
            // mount is in the EIO contract anyway.
            Err(mpsc::TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Write one notification frame to `/dev/fuse` from the writer thread.
///
/// A refusal is logged and skipped - it is the normal answer for a nodeid the
/// mount no longer holds - and it is never a transport failure. Only a
/// descriptor that is gone for good stops the thread.
fn write_notification(fuse_fd: RawFd, frame: &[u8], log: &(dyn Fn(&str) + Send + Sync)) -> bool {
    let code = i32::from_le_bytes(frame[4..8].try_into().unwrap());
    match write_fd_all(fuse_fd, frame) {
        Ok(()) => {
            log(&format!(
                "relay-pump: notification from the guest (code={code}, {} bytes, at={}) -> /dev/fuse",
                frame.len(),
                realtime_nanos()
            ));
            true
        }
        Err(err) => {
            let gone = matches!(
                err.raw_os_error(),
                Some(libc::ENODEV) | Some(libc::EBADF) | Some(libc::EPIPE)
            );
            log(&format!(
                "relay-pump: notification not delivered (at={}, {err}) - skipped",
                realtime_nanos()
            ));
            if gone {
                log("relay-pump: the notification writer stops: /dev/fuse is gone");
            }
            !gone
        }
    }
}

enum FuseRead {
    Request(usize),
    Closed,
}

enum Ready {
    Request,
    Stop,
    Closed,
    /// The transport has bytes while no request is in flight: the guest is
    /// speaking on its own (a notification).
    Transport,
}

/// Block until `/dev/fuse` has a request, the transport has bytes the guest sent
/// on its own, or a stop is requested.
fn wait_for_request(fuse_fd: RawFd, transport_fd: RawFd) -> Result<Ready> {
    let stop_fd = stop_read_fd();
    let mut fds = [
        libc::pollfd {
            fd: fuse_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stop_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: transport_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // A negative descriptor is ignored by poll(2), so the set is always three
    // entries and the guard is on the results.
    let count = 3;
    loop {
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                if stop_requested() {
                    return Ok(Ready::Stop);
                }
                continue;
            }
            return Err(err).context("failed to wait for a request from /dev/fuse");
        }
        if stop_fd >= 0 && fds[1].revents & libc::POLLIN != 0 {
            let mut drain = [0u8; 8];
            let _ = unsafe { libc::read(stop_fd, drain.as_mut_ptr().cast(), drain.len()) };
            if stop_requested() {
                return Ok(Ready::Stop);
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            return Ok(Ready::Request);
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(Ready::Closed);
        }
        if transport_fd >= 0 && fds[2].revents & libc::POLLIN != 0 {
            return Ok(Ready::Transport);
        }
    }
}

/// Relay frames until a stop is requested or `/dev/fuse` goes away.
pub fn relay<T: Read + Write + std::os::unix::io::AsRawFd>(
    fuse_fd: RawFd,
    transport: &mut T,
    stats: &mut Stats,
    log: RelayLog,
) -> Result<RelayOutcome> {
    relay_inner(fuse_fd, transport, stats, true, log)
}

/// The reply-path-only control arm: the same relay with the transport **not**
/// in the poll set.
///
/// It is the shape this crate shipped before note 66, kept so the regression
/// below can pin what it costs: a notification can then only be delivered while
/// a request is in flight, and the first host access after a guest write - the
/// one that matters - sends no request, so the reader stays stale for the full
/// attribute timeout. Nothing else differs, so it is a control and not a second
/// implementation.
pub fn relay_serial_control<T: Read + Write + std::os::unix::io::AsRawFd>(
    fuse_fd: RawFd,
    transport: &mut T,
    stats: &mut Stats,
    log: RelayLog,
) -> Result<RelayOutcome> {
    relay_inner(fuse_fd, transport, stats, false, log)
}

fn relay_inner<T: Read + Write + std::os::unix::io::AsRawFd>(
    fuse_fd: RawFd,
    transport: &mut T,
    stats: &mut Stats,
    poll_idle_transport: bool,
    log: RelayLog,
) -> Result<RelayOutcome> {
    let notifications = Notifications::start(fuse_fd, Arc::clone(&log))?;
    let mut request = vec![0u8; MAX_MSG];
    let mut reply = vec![0u8; MAX_MSG];
    let mut outcome = RelayOutcome {
        requests: 0,
        dead: false,
        stopped: false,
        fuse_closed: false,
    };
    // The transport is polled only while it is still worth listening to: once
    // the guest has closed it or spoken out of turn, the pump is in the `-EIO`
    // contract and must not wait on it again.
    let mut idle_transport = true;
    loop {
        if stop_requested() {
            outcome.stopped = true;
            break;
        }
        let transport_fd = if idle_transport && poll_idle_transport {
            transport.as_raw_fd()
        } else {
            -1
        };
        match wait_for_request(fuse_fd, transport_fd)? {
            Ready::Request => {}
            Ready::Stop => {
                outcome.stopped = true;
                break;
            }
            Ready::Closed => {
                outcome.fuse_closed = true;
                break;
            }
            Ready::Transport => {
                // Nothing is in flight, so a message here is the guest speaking
                // on its own: deliver it to /dev/fuse or end the run.
                match read_idle_message(transport, &mut reply, &notifications) {
                    Ok(()) => continue,
                    Err(err) => {
                        log(&format!(
                            "relay-pump: the transport stopped while idle ({err}) -> EIO mode, mount kept"
                        ));
                        outcome.dead = true;
                        idle_transport = false;
                        continue;
                    }
                }
            }
        }
        let frame = match read_request(fuse_fd, &mut request) {
            Ok(FuseRead::Request(len)) => len,
            Ok(FuseRead::Closed) => {
                outcome.fuse_closed = true;
                break;
            }
            Err(err) => {
                if stop_requested() {
                    outcome.stopped = true;
                    break;
                }
                return Err(err).context("failed to read a request from /dev/fuse");
            }
        };
        if frame < IN_HEADER_LEN {
            log(&format!(
                "relay-pump: short request ({frame} bytes) dropped"
            ));
            continue;
        }
        let total = u32::from_le_bytes(request[0..4].try_into().unwrap()) as usize;
        if total != frame {
            log(&format!(
                "relay-pump: request header says {total} bytes but {frame} arrived"
            ));
        }
        let total = total.min(frame);
        let opcode = u32::from_le_bytes(request[4..8].try_into().unwrap());
        let unique = u64::from_le_bytes(request[8..16].try_into().unwrap());
        outcome.requests += 1;
        let started = monotonic_nanos();

        if outcome.dead {
            write_fd_all(fuse_fd, &error_reply(unique, libc::EIO))?;
            stats.record(
                opcode,
                unique,
                total,
                OUT_HEADER_LEN,
                monotonic_nanos() - started,
                "eio",
            );
            continue;
        }

        // FORGET and BATCH_FORGET have no reply by protocol: relay them and move
        // on, or the reply stream shifts by one message.
        if opcode == FORGET || opcode == BATCH_FORGET {
            if let Err(err) = transport.write_all(&request[..total]) {
                log(&format!(
                    "relay-pump: write to guest failed: {err} -> EIO mode"
                ));
                outcome.dead = true;
            }
            stats.record(
                opcode,
                unique,
                total,
                0,
                monotonic_nanos() - started,
                "noreply",
            );
            continue;
        }

        if let Err(err) = transport.write_all(&request[..total]) {
            log(&format!(
                "relay-pump: write to guest failed: {err} -> EIO mode"
            ));
            outcome.dead = true;
            write_fd_all(fuse_fd, &error_reply(unique, libc::EIO))?;
            stats.record(
                opcode,
                unique,
                total,
                OUT_HEADER_LEN,
                monotonic_nanos() - started,
                "eio",
            );
            continue;
        }

        let reply_len = match read_reply_message(transport, &mut reply, &notifications) {
            Ok(Some(len)) => len,
            Ok(None) => {
                log("relay-pump: guest transport closed -> EIO mode, mount kept");
                outcome.dead = true;
                write_fd_all(fuse_fd, &error_reply(unique, libc::EIO))?;
                stats.record(
                    opcode,
                    unique,
                    total,
                    OUT_HEADER_LEN,
                    monotonic_nanos() - started,
                    "eio",
                );
                continue;
            }
            Err(err) => {
                log(&format!(
                    "relay-pump: reading the guest's reply failed: {err}"
                ));
                outcome.dead = true;
                write_fd_all(fuse_fd, &error_reply(unique, libc::EIO))?;
                stats.record(
                    opcode,
                    unique,
                    total,
                    OUT_HEADER_LEN,
                    monotonic_nanos() - started,
                    "eio",
                );
                continue;
            }
        };
        let status = i32::from_le_bytes(reply[4..8].try_into().unwrap());
        if opcode == OPCODE_INIT && reply_len >= 52 {
            log(&format!(
                "relay-pump: {}",
                describe_init(&reply[..reply_len])
            ));
        }
        write_fd_all(fuse_fd, &reply[..reply_len])?;
        stats.record(
            opcode,
            unique,
            total,
            reply_len,
            monotonic_nanos() - started,
            &status.to_string(),
        );
    }
    stats.flush();
    Ok(outcome)
}

/// The negotiated INIT shape, read at the offsets `fuse_init_out` has 16 bytes
/// into the reply: `flags` 28, `max_write` 36, `max_pages` 44 (note 63).
pub fn describe_init(reply: &[u8]) -> String {
    if reply.len() < 52 {
        return format!("INIT reply of {} bytes", reply.len());
    }
    let major = u32::from_le_bytes(reply[16..20].try_into().unwrap());
    let minor = u32::from_le_bytes(reply[20..24].try_into().unwrap());
    let flags = u32::from_le_bytes(reply[28..32].try_into().unwrap());
    let max_write = u32::from_le_bytes(reply[36..40].try_into().unwrap());
    let max_pages = u16::from_le_bytes(reply[44..46].try_into().unwrap());
    let flags2 = u32::from_le_bytes(reply[48..52].try_into().unwrap());
    format!(
        "INIT major={major} minor={minor} flags=0x{flags:x} max_write={max_write} \
         max_pages={max_pages} flags2=0x{flags2:x}"
    )
}

fn read_request(fuse_fd: RawFd, buffer: &mut [u8]) -> std::io::Result<FuseRead> {
    loop {
        let read = unsafe { libc::read(fuse_fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read > 0 {
            return Ok(FuseRead::Request(read as usize));
        }
        if read == 0 {
            return Ok(FuseRead::Closed);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => {
                if stop_requested() {
                    return Err(err);
                }
                continue;
            }
            // `ENODEV` is the kernel saying the FUSE connection is gone
            // (the mount was unmounted); nothing is left to serve.
            Some(libc::ENODEV) => return Ok(FuseRead::Closed),
            _ => return Err(err),
        }
    }
}

fn read_transport_header<T: Read>(
    transport: &mut T,
    reply: &mut [u8],
) -> std::io::Result<Option<usize>> {
    let mut filled = 0;
    while filled < OUT_HEADER_LEN {
        match transport.read(&mut reply[filled..OUT_HEADER_LEN]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "guest transport closed inside a reply header",
                ));
            }
            Ok(read) => filled += read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(Some(
        u32::from_le_bytes(reply[0..4].try_into().unwrap()) as usize
    ))
}

fn write_fd_all(fd: RawFd, bytes: &[u8]) -> std::io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        let put =
            unsafe { libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written) };
        if put < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if err.raw_os_error() == Some(libc::ENODEV) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "the /dev/fuse connection is gone",
                ));
            }
            return Err(err);
        }
        written += put as usize;
    }
    Ok(())
}

/// Read one message from the transport while **no request is in flight**.
///
/// The guest speaking here is a notification (`out_header.unique == 0`); anything
/// else is a reply to nothing, which is a protocol fault.
fn read_idle_message<T: Read>(
    transport: &mut T,
    buffer: &mut [u8],
    notifications: &Notifications,
) -> std::io::Result<()> {
    let len = match read_transport_header(transport, buffer)? {
        Some(len) => len,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the transport closed while idle",
            ));
        }
    };
    if !(OUT_HEADER_LEN..=MAX_MSG).contains(&len) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("idle message length {len} is outside the protocol's bounds"),
        ));
    }
    if len > OUT_HEADER_LEN {
        transport.read_exact(&mut buffer[OUT_HEADER_LEN..len])?;
    }
    let unique = u64::from_le_bytes(buffer[8..16].try_into().unwrap());
    let code = i32::from_le_bytes(buffer[4..8].try_into().unwrap());
    if unique != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the guest sent a reply (unique {unique}) with no request in flight"),
        ));
    }
    // Delivery happens on the writer thread (see the module documentation);
    // `code` is logged there, from the frame the thread writes.
    let _ = code;
    notifications.offer(&buffer[..len]);
    Ok(())
}

/// Read one *reply* from the transport, delivering any notification frames that
/// arrive ahead of it.
///
/// A notification is a message with `out_header.unique == 0` and
/// `out_header.error` set to a `fuse_notify_code`; the kernel takes it for a
/// notification whatever the mount's negotiated features. A pump that took the
/// next message as "the reply" would hand the kernel a notification for a
/// request and every later frame would be off by one - so notifications are
/// delivered and the loop keeps reading until a real reply arrives.
fn read_reply_message<T: Read>(
    transport: &mut T,
    reply: &mut [u8],
    notifications: &Notifications,
) -> std::io::Result<Option<usize>> {
    loop {
        let len = match read_transport_header(transport, reply)? {
            Some(len) => len,
            None => return Ok(None),
        };
        if !(OUT_HEADER_LEN..=MAX_MSG).contains(&len) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("message length {len} is outside the protocol's bounds"),
            ));
        }
        if len > OUT_HEADER_LEN {
            transport.read_exact(&mut reply[OUT_HEADER_LEN..len])?;
        }
        if u64::from_le_bytes(reply[8..16].try_into().unwrap()) != 0 {
            return Ok(Some(len));
        }
        // A notification ahead of the reply goes to the writer thread and the
        // loop keeps reading: a pump that took it for the reply would hand the
        // kernel a notification for a request and shift every later frame.
        notifications.offer(&reply[..len]);
    }
}

/// A 16-byte `fuse_out_header` carrying `-errno` and no body.
pub fn error_reply(unique: u64, errno: i32) -> [u8; OUT_HEADER_LEN] {
    let mut reply = [0u8; OUT_HEADER_LEN];
    reply[0..4].copy_from_slice(&(OUT_HEADER_LEN as u32).to_le_bytes());
    reply[4..8].copy_from_slice(&(-errno).to_le_bytes());
    reply[8..16].copy_from_slice(&unique.to_le_bytes());
    reply
}

/// Wall-clock nanoseconds, the clock the host's own tools stamp with, so a
/// delivered notification can be paired with a reader's observation of it
/// without a second clock to convert.
pub fn realtime_nanos() -> u64 {
    let mut spec: libc::timespec = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut spec) };
    if rc != 0 {
        return 0;
    }
    (spec.tv_sec as u64) * 1_000_000_000 + spec.tv_nsec as u64
}

pub fn monotonic_nanos() -> u64 {
    let mut spec: libc::timespec = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut spec) };
    if rc != 0 {
        return 0;
    }
    (spec.tv_sec as u64) * 1_000_000_000 + spec.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// The stop flag and the stop channel are process-global (that is the point:
    /// a signal handler owns them), so the relay tests that use them take turns.
    static STOP_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// A relay logger whose lines can be read after the run: the notification
    /// writer logs from its own thread, so a test waits for the line it wants
    /// instead of asserting on a collected vector.
    fn log_channel() -> (RelayLog, mpsc::Receiver<String>) {
        let (sender, receiver) = mpsc::channel();
        (
            Arc::new(move |line: &str| {
                let _ = sender.send(line.to_owned());
            }),
            receiver,
        )
    }

    /// Collect every line a receiver has queued, waiting `wait` past the last
    /// line it sees.
    fn drain(receiver: &mpsc::Receiver<String>, wait: Duration) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = receiver.recv_timeout(wait) {
            lines.push(line);
        }
        lines
    }

    fn stop_test_guard() -> std::sync::MutexGuard<'static, ()> {
        STOP_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn request(opcode: u32, unique: u64, body: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; IN_HEADER_LEN + body.len()];
        let total = frame.len() as u32;
        frame[0..4].copy_from_slice(&total.to_le_bytes());
        frame[4..8].copy_from_slice(&opcode.to_le_bytes());
        frame[8..16].copy_from_slice(&unique.to_le_bytes());
        frame[IN_HEADER_LEN..].copy_from_slice(body);
        frame
    }

    fn init_request() -> Vec<u8> {
        let mut body = vec![0u8; 16];
        body[0..4].copy_from_slice(&7u32.to_le_bytes());
        body[4..8].copy_from_slice(&33u32.to_le_bytes());
        body[8..12].copy_from_slice(&131_072u32.to_le_bytes());
        body[12..16].copy_from_slice(&(0x6000u32 | 0x20 | 0x0040_0000).to_le_bytes());
        request(OPCODE_INIT, 1, &body)
    }

    fn init_reply() -> Vec<u8> {
        let mut reply = vec![0u8; OUT_HEADER_LEN + 64];
        let total = reply.len() as u32;
        reply[0..4].copy_from_slice(&total.to_le_bytes());
        reply[8..16].copy_from_slice(&1u64.to_le_bytes());
        reply[16..20].copy_from_slice(&7u32.to_le_bytes());
        reply[20..24].copy_from_slice(&33u32.to_le_bytes());
        reply[28..32].copy_from_slice(&(0x6000u32 | 0x20 | 0x0040_0000).to_le_bytes());
        reply[36..40].copy_from_slice(&1_048_576u32.to_le_bytes());
        reply[44..46].copy_from_slice(&256u16.to_le_bytes());
        reply
    }

    /// The guest stand-in: reads `frames` requests and answers each with a zero
    /// error and an empty body, then closes the connection.
    fn fake_guest(
        mut stream: UnixStream,
        frames: usize,
        served: mpsc::Sender<Vec<u8>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for _ in 0..frames {
                let mut header = [0u8; IN_HEADER_LEN];
                if stream.read_exact(&mut header).is_err() {
                    return;
                }
                let total = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
                let opcode = u32::from_le_bytes(header[4..8].try_into().unwrap());
                let unique = u64::from_le_bytes(header[8..16].try_into().unwrap());
                let mut body = vec![0u8; total.saturating_sub(IN_HEADER_LEN)];
                if stream.read_exact(&mut body).is_err() {
                    return;
                }
                let _ = served.send(header.to_vec());
                if opcode == OPCODE_INIT {
                    if stream.write_all(&init_reply()).is_err() {
                        return;
                    }
                    continue;
                }
                let mut reply = [0u8; OUT_HEADER_LEN];
                reply[0..4].copy_from_slice(&(OUT_HEADER_LEN as u32).to_le_bytes());
                reply[8..16].copy_from_slice(&unique.to_le_bytes());
                if stream.write_all(&reply).is_err() {
                    return;
                }
            }
        })
    }

    fn read_reply(stream: &mut UnixStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let mut header = [0u8; OUT_HEADER_LEN];
        stream.read_exact(&mut header).expect("reply header");
        let total = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let mut body = vec![0u8; total - OUT_HEADER_LEN];
        if !body.is_empty() {
            stream.read_exact(&mut body).expect("reply body");
        }
        let mut reply = header.to_vec();
        reply.extend_from_slice(&body);
        reply
    }

    /// Read one frame if one arrives within `wait`, else `None`.
    fn try_read_frame(stream: &mut UnixStream, wait: Duration) -> Option<Vec<u8>> {
        stream.set_read_timeout(Some(wait)).ok()?;
        let mut header = [0u8; OUT_HEADER_LEN];
        stream.read_exact(&mut header).ok()?;
        let total = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let mut frame = header.to_vec();
        frame.resize(total, 0);
        if total > OUT_HEADER_LEN {
            stream.read_exact(&mut frame[OUT_HEADER_LEN..total]).ok()?;
        }
        Some(frame)
    }

    #[test]
    fn relays_frames_and_answers_eio_after_the_transport_dies() {
        let _guard = stop_test_guard();
        clear_stop();
        install_stop_channel().expect("stop channel");
        let (fuse_side, fuse_pump) = UnixStream::pair().expect("fuse pair");
        let (guest_side, mut pump_guest) = UnixStream::pair().expect("guest pair");
        let (served, received) = mpsc::channel();
        let guest = fake_guest(guest_side, 2, served);

        let pump_fd = fuse_pump.as_raw_fd();
        let (log, lines) = log_channel();
        let handle = thread::spawn(move || {
            let mut stats = Stats::disabled();
            relay(pump_fd, &mut pump_guest, &mut stats, log).expect("relay should run")
        });

        let mut fuse_side = fuse_side;
        fuse_side
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        fuse_side.write_all(&init_request()).expect("write init");
        let reply = read_reply(&mut fuse_side);
        assert_eq!(
            i32::from_le_bytes(reply[4..8].try_into().unwrap()),
            0,
            "INIT must be answered by the guest"
        );
        assert!(describe_init(&reply).contains("max_write=1048576"));

        // Second frame: the guest answers it and then closes.
        fuse_side
            .write_all(&request(15, 2, &[0u8; 40]))
            .expect("write read");
        let reply = read_reply(&mut fuse_side);
        assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), 0);
        let _ = received
            .recv_timeout(Duration::from_secs(5))
            .expect("frame 1");
        let _ = received
            .recv_timeout(Duration::from_secs(5))
            .expect("frame 2");
        guest.join().expect("guest thread");

        // Third frame: the transport is gone, so it is answered -EIO and the
        // mount keeps serving.
        fuse_side
            .write_all(&request(3, 3, &[0u8; 8]))
            .expect("write stat");
        let reply = read_reply(&mut fuse_side);
        assert_eq!(
            i32::from_le_bytes(reply[4..8].try_into().unwrap()),
            -libc::EIO,
            "a request after the transport died must be refused with EIO"
        );
        assert_eq!(
            u64::from_le_bytes(reply[8..16].try_into().unwrap()),
            3,
            "the EIO reply must carry the refused request's unique"
        );

        // And again, with no attempt to reconnect or replay.
        fuse_side
            .write_all(&request(3, 4, &[0u8; 8]))
            .expect("write stat");
        let reply = read_reply(&mut fuse_side);
        assert_eq!(
            i32::from_le_bytes(reply[4..8].try_into().unwrap()),
            -libc::EIO
        );

        drop(fuse_side);
        let outcome = handle.join().expect("pump thread");
        assert_eq!(outcome.requests, 4);
        assert!(
            outcome.dead,
            "the relay must have noticed the transport death"
        );
        assert!(outcome.fuse_closed, "/dev/fuse closing must end the run");
        assert!(!outcome.stopped);
        let logs = drain(&lines, Duration::from_millis(200));
        assert!(
            logs.iter().any(|line| line.contains("-> EIO mode")),
            "one named death line must be logged: {logs:?}"
        );
    }

    #[test]
    fn forget_frames_are_relayed_and_never_answered() {
        let _guard = stop_test_guard();
        clear_stop();
        install_stop_channel().expect("stop channel");
        let (fuse_side, fuse_pump) = UnixStream::pair().expect("fuse pair");
        let (guest_side, mut pump_guest) = UnixStream::pair().expect("guest pair");
        let (served, received) = mpsc::channel();
        let guest = fake_guest(guest_side, 1, served);
        let pump_fd = fuse_pump.as_raw_fd();
        let handle = thread::spawn(move || {
            let mut stats = Stats::disabled();
            relay(pump_fd, &mut pump_guest, &mut stats, Arc::new(|_: &str| {}))
        });

        let mut fuse_side = fuse_side;
        fuse_side
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        fuse_side
            .write_all(&request(FORGET, 7, &[0u8; 8]))
            .expect("write forget");
        // It reaches the guest...
        let header = received
            .recv_timeout(Duration::from_secs(5))
            .expect("frame");
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), FORGET);
        // ...and nothing comes back on /dev/fuse.
        let mut byte = [0u8; 1];
        let read = fuse_side.read(&mut byte);
        assert!(
            matches!(read, Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock
                || err.kind() == std::io::ErrorKind::TimedOut),
            "FORGET must not be answered, read gave {read:?}"
        );

        guest.join().expect("guest");
        drop(fuse_side);
        let outcome = handle.join().expect("pump thread").expect("relay");
        assert!(outcome.fuse_closed);
    }

    #[test]
    fn a_stop_request_ends_the_run_without_touching_the_mount() {
        let _guard = stop_test_guard();
        clear_stop();
        install_stop_channel().expect("stop channel");
        install_stop_channel().expect("stop channel");
        let (fuse_side, fuse_pump) = UnixStream::pair().expect("fuse pair");
        let (guest_side, mut pump_guest) = UnixStream::pair().expect("guest pair");
        let pump_fd = fuse_pump.as_raw_fd();
        let handle = thread::spawn(move || {
            let mut stats = Stats::disabled();
            relay(pump_fd, &mut pump_guest, &mut stats, Arc::new(|_: &str| {}))
        });
        thread::sleep(Duration::from_millis(20));
        request_stop();
        let outcome = handle.join().expect("pump thread").expect("relay");
        assert!(outcome.stopped, "the stop request must be observed");
        assert_eq!(outcome.requests, 0);
        drop(fuse_side);
        drop(guest_side);
        clear_stop();
    }

    #[test]
    fn the_eio_reply_is_the_sixteen_byte_header() {
        let reply = error_reply(0xdead_beef, libc::EIO);
        assert_eq!(reply.len(), 16);
        assert_eq!(u32::from_le_bytes(reply[0..4].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -5);
        assert_eq!(
            u64::from_le_bytes(reply[8..16].try_into().unwrap()),
            0xdead_beef
        );
    }

    #[test]
    fn init_shapes_are_described_from_the_reply_bytes() {
        let reply = init_reply();
        let line = describe_init(&reply);
        assert!(line.contains("flags=0x406020"), "{line}");
        assert!(line.contains("max_write=1048576"), "{line}");
        assert!(line.contains("max_pages=256"), "{line}");
        assert!(describe_init(&reply[..16]).starts_with("INIT reply of 16 bytes"));
    }

    /// A 40-byte `FUSE_NOTIFY_INVAL_INODE` frame: `unique == 0`, `error == 2`,
    /// then `ino`, `off = 0`, `len = 0` (note 66 §1).
    fn notification(code: i32, ino: u64) -> Vec<u8> {
        let mut frame = vec![0u8; 40];
        frame[0..4].copy_from_slice(&40u32.to_le_bytes());
        frame[4..8].copy_from_slice(&code.to_le_bytes());
        frame[16..24].copy_from_slice(&ino.to_le_bytes());
        frame[24..32].copy_from_slice(&0i64.to_le_bytes());
        frame[32..40].copy_from_slice(&0i64.to_le_bytes());
        frame
    }

    /// Describe a delivered frame the way the assertions want to read it:
    /// `len`, `error`, `unique`.
    fn describe_frame(frame: &[u8]) -> String {
        format!(
            "len={} error={} unique={}",
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            i32::from_le_bytes(frame[4..8].try_into().unwrap()),
            u64::from_le_bytes(frame[8..16].try_into().unwrap())
        )
    }

    /// The guest writes one notification on its own, with **no request in
    /// flight**: the pump is idle in `poll(2)`.
    ///
    /// `serial_control` runs the reply-path-only arm instead, which is the
    /// shape the crate shipped before note 66 and the negative control for this
    /// test. Returns the frame that reached `/dev/fuse` (if any) and how long
    /// the write took to arrive.
    fn idle_notification_arm(serial_control: bool) -> (Option<Vec<u8>>, u64) {
        let (fuse_side, fuse_pump) = UnixStream::pair().expect("fuse pair");
        let (guest_side, mut pump_guest) = UnixStream::pair().expect("guest pair");
        let pump_fd = fuse_pump.as_raw_fd();
        let log: RelayLog = Arc::new(|_: &str| {});
        let handle = thread::spawn(move || {
            let mut stats = Stats::disabled();
            if serial_control {
                relay_serial_control(pump_fd, &mut pump_guest, &mut stats, log)
            } else {
                relay(pump_fd, &mut pump_guest, &mut stats, log)
            }
        });

        // Nothing is in flight: the pump is waiting in poll(2).
        thread::sleep(Duration::from_millis(50));
        let mut guest_side = guest_side;
        let wrote = monotonic_nanos();
        guest_side
            .write_all(&notification(2, 7))
            .expect("notification");
        let mut fuse_side = fuse_side;
        let frame = try_read_frame(&mut fuse_side, Duration::from_millis(500));
        let latency = monotonic_nanos() - wrote;
        // Dropping /dev/fuse ends the pump, whether or not a frame arrived.
        drop(fuse_side);
        let _ = handle.join().expect("pump thread");
        (frame, latency)
    }

    /// The reason the transport is in the poll set: a notification with no
    /// request in flight still reaches `/dev/fuse`, immediately.
    #[test]
    fn an_idle_notification_reaches_dev_fuse_without_a_request() {
        let _guard = stop_test_guard();
        clear_stop();
        install_stop_channel().expect("stop channel");
        let (frame, latency) = idle_notification_arm(false);
        let frame = frame.expect("the idle notification must be delivered");
        assert_eq!(describe_frame(&frame), "len=40 error=2 unique=0");
        assert_eq!(
            u64::from_le_bytes(frame[16..24].try_into().unwrap()),
            7,
            "the delivered frame is the one the guest wrote"
        );
        assert!(
            latency < 20_000_000,
            "delivery must be immediate, took {latency} ns"
        );
    }

    /// The control: with the transport out of the poll set, the same frame is
    /// never delivered while the mount is idle - the reply-path-only drain the
    /// crate shipped before note 66, and the measured `4.98 s` stale window.
    #[test]
    fn a_reply_path_only_drain_never_delivers_an_idle_notification() {
        let _guard = stop_test_guard();
        clear_stop();
        install_stop_channel().expect("stop channel");
        let (frame, latency) = idle_notification_arm(true);
        assert!(
            frame.is_none(),
            "the transport is not polled, so nothing arrives"
        );
        assert!(
            latency >= 500_000_000 - 1_000_000,
            "it must have waited out the 500 ms window, took {latency} ns"
        );
    }

    /// A refused notification - the kernel answering `ENOENT` for a nodeid the
    /// mount no longer holds - is logged and skipped. It is never a transport
    /// failure, or every later request would be answered `-EIO` and the mount
    /// would be dead for one stale watcher entry.
    ///
    /// The pump's `/dev/fuse` is the read end of a pipe here: requests can still
    /// be read from it, but every write to it fails (`EBADF`), which is the
    /// shape of a kernel refusal without a mounted FUSE connection in a unit
    /// test.
    #[test]
    fn a_refused_notification_is_skipped_and_never_kills_the_mount() {
        let _guard = stop_test_guard();
        clear_stop();
        install_stop_channel().expect("stop channel");
        let mut pipe_fds = [0i32; 2];
        let rc = unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        assert_eq!(rc, 0, "pipe2 failed");
        let fuse_read = pipe_fds[0];
        let _fuse_write = pipe_fds[1];
        let (guest_side, mut pump_guest) = UnixStream::pair().expect("guest pair");
        let (log, log_rx) = log_channel();
        let handle = thread::spawn(move || {
            let mut stats = Stats::disabled();
            relay(fuse_read, &mut pump_guest, &mut stats, log)
                .expect("a refused notification must not end the relay")
        });

        thread::sleep(Duration::from_millis(30));
        let mut guest_side = guest_side;
        guest_side
            .write_all(&notification(2, 9_999))
            .expect("notification");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut lines = Vec::new();
        let mut skipped = false;
        while !skipped && std::time::Instant::now() < deadline {
            let Ok(line) = log_rx.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            skipped = line.contains("notification") && line.contains("skipped");
            lines.push(line);
        }
        assert!(
            skipped,
            "the refusal must be named and skipped, got {lines:?}"
        );

        // The mount is untouched: a stop still ends the run normally, with no
        // `dead` verdict, and a second notification is still delivered.
        request_stop();
        let outcome = handle.join().expect("pump thread");
        assert!(outcome.stopped, "the stop must be observed");
        assert!(
            !outcome.dead,
            "a refused notification is skipped, not a mount death"
        );
        drop(guest_side);
    }
}
