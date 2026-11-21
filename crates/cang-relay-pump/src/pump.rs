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

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

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

enum FuseRead {
    Request(usize),
    Closed,
}

enum Ready {
    Request,
    Stop,
    Closed,
}

/// Block until `/dev/fuse` has a request or a stop is requested.
fn wait_for_request(fuse_fd: RawFd) -> Result<Ready> {
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
    ];
    let count = if stop_fd >= 0 { 2 } else { 1 };
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
        if count == 2 && fds[1].revents & libc::POLLIN != 0 {
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
    }
}

/// Relay frames until a stop is requested or `/dev/fuse` goes away.
pub fn relay<T: Read + Write>(
    fuse_fd: RawFd,
    transport: &mut T,
    stats: &mut Stats,
    mut log: impl FnMut(&str),
) -> Result<RelayOutcome> {
    let mut request = vec![0u8; MAX_MSG];
    let mut reply = vec![0u8; MAX_MSG];
    let mut outcome = RelayOutcome {
        requests: 0,
        dead: false,
        stopped: false,
        fuse_closed: false,
    };
    loop {
        if stop_requested() {
            outcome.stopped = true;
            break;
        }
        match wait_for_request(fuse_fd)? {
            Ready::Request => {}
            Ready::Stop => {
                outcome.stopped = true;
                break;
            }
            Ready::Closed => {
                outcome.fuse_closed = true;
                break;
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

        let reply_len = match read_transport_header(transport, &mut reply) {
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
        if !(OUT_HEADER_LEN..=MAX_MSG).contains(&reply_len) {
            log(&format!(
                "relay-pump: guest replied with an out-of-bounds length ({reply_len})"
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
        if reply_len > OUT_HEADER_LEN
            && let Err(err) = transport.read_exact(&mut reply[OUT_HEADER_LEN..reply_len])
        {
            log(&format!(
                "relay-pump: guest died mid-reply: {err} -> EIO mode"
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

fn write_fd_all(fd: RawFd, bytes: &[u8]) -> Result<()> {
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
                bail!("the /dev/fuse connection is gone");
            }
            return Err(err).context("failed to write a reply to /dev/fuse");
        }
        written += put as usize;
    }
    Ok(())
}

/// A 16-byte `fuse_out_header` carrying `-errno` and no body.
pub fn error_reply(unique: u64, errno: i32) -> [u8; OUT_HEADER_LEN] {
    let mut reply = [0u8; OUT_HEADER_LEN];
    reply[0..4].copy_from_slice(&(OUT_HEADER_LEN as u32).to_le_bytes());
    reply[4..8].copy_from_slice(&(-errno).to_le_bytes());
    reply[8..16].copy_from_slice(&unique.to_le_bytes());
    reply
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
        let handle = thread::spawn(move || {
            let mut stats = Stats::disabled();
            let mut logs = Vec::new();
            let outcome = relay(pump_fd, &mut pump_guest, &mut stats, |line| {
                logs.push(line.to_owned())
            })
            .expect("relay should run");
            (outcome, logs)
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
        let (outcome, logs) = handle.join().expect("pump thread");
        assert_eq!(outcome.requests, 4);
        assert!(
            outcome.dead,
            "the relay must have noticed the transport death"
        );
        assert!(outcome.fuse_closed, "/dev/fuse closing must end the run");
        assert!(!outcome.stopped);
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
            relay(pump_fd, &mut pump_guest, &mut stats, |_| {})
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
            relay(pump_fd, &mut pump_guest, &mut stats, |_| {})
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
}
