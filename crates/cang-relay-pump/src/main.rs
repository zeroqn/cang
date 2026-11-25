//! `relay-pump`: own a FUSE mount and relay its frames to the guest's workspace
//! relay over libkrun's vsock unix port.
//!
//! ```text
//! relay-pump --socket /tmp/cang-1000/wr-<task>.sock \
//!            --mountpoint /home/dev/work \
//!            --state-file <task dir>/relay-pump.state \
//!            [--options fsname=cang-workspace] [--stats <task dir>/relay.stats.tsv]
//! ```
//!
//! Order matters and is enforced: the transport is connected (and proven live)
//! **before** the mount, because `mount(2)` blocks until the server answers the
//! kernel's INIT request - a mount taken before the guest is listening hangs
//! `fusermount3` instead of failing.
//!
//! See the crate documentation for why this runs outside cang's
//! Landlock/seccomp and for the death contract (`-EIO`, never an unmount).

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cang_relay_pump::fusermount;
use cang_relay_pump::mount;
use cang_relay_pump::pump::{self, Stats};

#[global_allocator]
static ALLOC: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;

/// The switch for the pump's own progress lines.
///
/// They are diagnostics - the mount, the negotiated INIT shape, the connection's
/// death - and the pump's stderr *is* cang's launch output whenever cang starts
/// it, so a plain launch prints none of them: the same line in the middle of a
/// launch's output reads as a fault even when the mount is working. cang sets
/// this from its own `--log-level` (`debug` and `trace`), and exporting it in the
/// environment reaches the pump by inheritance, which is how one launch asks for
/// the relay's side alone without turning on everything cang logs.
const DEBUG_ENV: &str = "CANG_RELAY_PUMP_DEBUG";

/// How long the transport gets to become usable. The guest's relay binds its
/// vsock port during guest-init, a second or so after the VM starts; the
/// deadline only has to be longer than a slow boot.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 120;
/// How long the "connected" probe waits for the peer to close before deciding
/// the guest is actually listening.
const PROBE_WAIT: Duration = Duration::from_millis(250);
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Parser)]
#[command(
    name = "relay-pump",
    version,
    about = "Relay a FUSE mount's frames to the guest serving the workspace tree"
)]
struct Args {
    /// The unix socket libkrun bound for the guest's vsock listener.
    #[arg(long)]
    socket: PathBuf,
    /// The host directory to mount the guest's tree at.
    #[arg(long)]
    mountpoint: PathBuf,
    /// The `fusermount3 -o` options for this mount.
    #[arg(long, default_value = "fsname=cang-workspace")]
    options: String,
    /// How long to wait for the guest to accept the connection.
    #[arg(long, default_value_t = DEFAULT_CONNECT_TIMEOUT_SECS)]
    connect_timeout_secs: u64,
    /// Where to report progress (`starting`, `ready`, `error`, `unmounted`).
    #[arg(long)]
    state_file: Option<PathBuf>,
    /// Per-frame TSV, off unless requested.
    #[arg(long)]
    stats: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    pump::install_stop_handler()?;
    let diagnostics = diagnostics_enabled(std::env::var(DEBUG_ENV).ok().as_deref());
    report(&args.state_file, "starting");

    let fusermount_program = fusermount::program();
    let mut transport =
        match connect_transport(&args.socket, Duration::from_secs(args.connect_timeout_secs)) {
            Ok(transport) => transport,
            Err(err) => {
                report(&args.state_file, &format!("error connect {err:#}"));
                eprintln!(
                    "relay-pump: no workspace relay answered on '{}' within {}s: {err:#}",
                    args.socket.display(),
                    args.connect_timeout_secs
                );
                bail!("failed to reach the guest's workspace relay");
            }
        };

    let mount = match mount::mount(&fusermount_program, &args.mountpoint, &args.options) {
        Ok(mount) => mount,
        Err(err) => {
            report(&args.state_file, &format!("error mount {err:#}"));
            eprintln!(
                "relay-pump: failed to mount '{}' through {:?}: {err:#}",
                args.mountpoint.display(),
                fusermount_program
            );
            bail!("failed to mount the workspace relay");
        }
    };
    report(
        &args.state_file,
        &format!("ready {}", args.mountpoint.display()),
    );
    if diagnostics {
        eprintln!(
            "relay-pump: mounted '{}' (fuse_fd={}) through {:?}; relaying to '{}'",
            args.mountpoint.display(),
            mount.fd(),
            fusermount_program,
            args.socket.display()
        );
    }

    let mut stats = match &args.stats {
        Some(path) => Stats::enabled(path)?,
        None => Stats::disabled(),
    };
    let fuse_fd = mount.into_fd();
    let outcome = {
        // The relay's own lines (INIT, notifications, the death of the
        // transport, protocol faults) are per-frame diagnostics: they reach the
        // terminal only when they were asked for.
        let log: std::sync::Arc<dyn Fn(&str) + Send + Sync> =
            std::sync::Arc::new(move |line: &str| {
                if diagnostics {
                    eprintln!("{line}");
                }
            });
        pump::relay(fuse_fd, &mut transport, &mut stats, log).context("workspace relay stopped")?
    };
    stats.flush();
    let _ = transport.flush();
    drop(transport);

    if outcome.stopped {
        // The only unmount path: cang is exiting and the mount was its.
        match fusermount::unmount(&fusermount_program, &args.mountpoint) {
            Ok(()) => {
                report(&args.state_file, "unmounted");
                if diagnostics {
                    eprintln!(
                        "relay-pump: unmounted '{}' after {} requests",
                        args.mountpoint.display(),
                        outcome.requests
                    );
                }
            }
            Err(err) => {
                report(&args.state_file, &format!("error unmount {err:#}"));
                eprintln!(
                    "relay-pump: could not unmount '{}' (a host process still holds it): {err:#}",
                    args.mountpoint.display()
                );
                let _ = unsafe { libc::close(fuse_fd) };
                std::process::exit(1);
            }
        }
    } else if outcome.fuse_closed {
        report(&args.state_file, "closed");
        eprintln!(
            "relay-pump: /dev/fuse closed after {} requests (the mount was removed elsewhere)",
            outcome.requests
        );
    }
    if fuse_fd >= 0 {
        let _ = unsafe { libc::close(fuse_fd) };
    }
    Ok(())
}

/// Whether the pump's progress lines were asked for: `CANG_RELAY_PUMP_DEBUG`
/// holds a truthy value (`1`, `true`, `yes` or `on`).
fn diagnostics_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn report(path: &Option<PathBuf>, line: &str) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{line}\n"));
}

/// Connect to the guest's relay, retrying until the VM has bound the socket and
/// the guest has started listening.
///
/// A successful `connect(2)` is not enough: libkrun's acceptor accepts the host
/// connection immediately and only then dials the guest port, so a guest that is
/// not listening yet shows up as a connection that closes at once. The probe
/// below sends nothing and waits for that close, which is the only signal the
/// proxy gives.
pub fn connect_transport(socket: &Path, deadline: Duration) -> Result<UnixStream> {
    let started = Instant::now();
    let mut attempts = 0u32;
    let mut last_error;
    loop {
        attempts += 1;
        match UnixStream::connect(socket) {
            Ok(stream) => match probe_listening(&stream) {
                Ok(()) => return Ok(stream),
                Err(err) => last_error = err.to_string(),
            },
            Err(err) => last_error = err.to_string(),
        }
        if started.elapsed() >= deadline {
            bail!(
                "gave up after {attempts} attempts and {:.1}s; last error: {last_error}",
                started.elapsed().as_secs_f64()
            );
        }
        std::thread::sleep(RETRY_INTERVAL);
    }
}

/// `Ok` means the peer stayed connected across a short grace period, i.e. the
/// guest relay accepted and is waiting for a request.
fn probe_listening(stream: &UnixStream) -> Result<()> {
    stream
        .set_read_timeout(Some(PROBE_WAIT))
        .context("failed to set the transport probe timeout")?;
    let mut byte = [0u8; 1];
    let mut reader = stream;
    let verdict = match reader.read(&mut byte) {
        Ok(0) => Err(anyhow::anyhow!(
            "the guest closed the connection immediately (its relay is not listening yet)"
        )),
        Ok(read) => {
            // The guest never speaks first; a byte here is a protocol fault, and
            // the connection is still usable, so it is reported rather than
            // thrown away.
            eprintln!("relay-pump: the guest sent {read} byte(s) before any request");
            Ok(())
        }
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Err(err) => Err(anyhow::anyhow!("probe read failed: {err}")),
    };
    stream
        .set_read_timeout(None)
        .context("failed to clear the transport probe timeout")?;
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A listener that accepts and holds the connection: the pump must accept it
    /// as "the guest is listening" and hand the same stream back.
    #[test]
    fn a_listening_peer_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("relay.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let accepted = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });

        let stream = connect_transport(&path, Duration::from_secs(5)).expect("should connect");
        assert!(stream.peer_addr().is_ok());
        accepted.join().expect("accept thread");
    }

    /// A listener that accepts and closes at once is libkrun's shape when the
    /// guest has no listener yet: the pump must not mistake it for success.
    #[test]
    fn a_peer_that_closes_immediately_is_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("relay.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let handle = std::thread::spawn(move || {
            // Keep accepting and dropping, the way libkrun's proxy does while the
            // guest's listener does not exist yet.
            listener.set_nonblocking(true).expect("nonblocking");
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((_stream, _)) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        let err = connect_transport(&path, Duration::from_millis(400))
            .expect_err("a closing peer must not be accepted");
        assert!(format!("{err:#}").contains("gave up"), "{err:#}");
        handle.join().expect("listener thread");
    }

    /// The progress lines are opt-in, so a launch that sets nothing prints
    /// nothing: this is the gate the two mount lines and the relay's own lines
    /// go through.
    #[test]
    fn the_diagnostic_switch_is_opt_in() {
        assert!(!diagnostics_enabled(None));
        assert!(!diagnostics_enabled(Some("")));
        assert!(!diagnostics_enabled(Some("0")));
        assert!(!diagnostics_enabled(Some("false")));
        assert!(diagnostics_enabled(Some("1")));
        assert!(diagnostics_enabled(Some(" ON ")));
    }

    #[test]
    fn an_absent_socket_is_retried_until_the_deadline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.sock");
        let err = connect_transport(&path, Duration::from_millis(250))
            .expect_err("an absent socket must not connect");
        assert!(format!("{err:#}").contains("gave up"), "{err:#}");
    }
}
