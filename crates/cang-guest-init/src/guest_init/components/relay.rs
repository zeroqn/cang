//! `cang-guest-relay`: the guest half of `--workspace-relay`.
//!
//! The launch sets `CANG_WORKSPACE_RELAY_PORT` (and, in principle,
//! `CANG_WORKSPACE_RELAY_ROOT`) when the host intends to mount the workspace
//! tree; this component starts the guest's FUSE server on that vsock port and
//! waits for its `READY` line, so the task command cannot run before the tree is
//! being served. The host pump retries its connection, so "ready" costs a retry
//! and "not ready" costs a stall - hence the wait rather than a fire-and-forget.
//!
//! The binary is a sibling of this executable: both come from the same static
//! musl package (`cang-guest-relay` beside `cang-guest-init`), so no store path
//! has to be baked into the launch.
//!
//! A relay that dies after startup is reported and NOT restarted in stage 1: the
//! host pump answers `EIO` for a dead guest (ticket 23 decision 4(a)), which is
//! the documented behaviour, and a restart would race the pump's reconnect.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The line the relay prints once its vsock listener is bound.
/// Must match `cang_guest_relay::READY_MARKER`; the relay crate's own test pins
/// the literal, and this constant is the guest-init side of that pair (the two
/// crates do not depend on each other: guest-init must not gain a FUSE server).
const READY_MARKER: &str = "CANG_WORKSPACE_RELAY_READY";
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// The relay binary, resolved beside this executable.
pub(in crate::guest_init) fn sibling_binary() -> Result<PathBuf> {
    let current = std::env::current_exe().context("failed to resolve guest-init executable")?;
    let dir = current
        .parent()
        .context("guest-init executable has no parent directory")?;
    let relay = dir.join("cang-guest-relay");
    if !relay.exists() {
        bail!(
            "the guest relay '{}' is missing: this image predates --workspace-relay",
            relay.display()
        );
    }
    Ok(relay)
}

/// Start the guest relay for `root` on `port` and wait for its ready line.
pub(in crate::guest_init) fn start(port: u32, root: &str) -> Result<()> {
    if port == 0 {
        bail!("CANG_WORKSPACE_RELAY_PORT is zero: the guest relay needs a vsock port");
    }
    let relay = sibling_binary()?;
    let mut child = Command::new(&relay)
        .arg("--port")
        .arg(port.to_string())
        .arg("--root")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn the guest relay '{}'", relay.display()))?;

    let stdout = child
        .stdout
        .take()
        .context("the guest relay has no stdout to read its ready line from")?;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
    thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let mut signalled = false;
        for line in lines.by_ref() {
            let Ok(line) = line else { break };
            if !signalled && line.starts_with(READY_MARKER) {
                signalled = true;
                let _ = ready_tx.send(true);
            }
            eprintln!("cang-guest-relay: {line}");
        }
        if !signalled {
            let _ = ready_tx.send(false);
        }
    });

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match ready_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(true) => return Ok(()),
            Ok(false) => bail!(
                "the guest relay '{}' exited before its ready line on port {port}",
                relay.display()
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    bail!("the guest relay did not become ready within {READY_TIMEOUT:?}");
                }
                if let Ok(Some(status)) = child.try_wait() {
                    bail!("the guest relay exited at startup: {status}");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("the guest relay's readiness channel closed unexpectedly")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_port_is_refused_by_name() {
        let err = start(0, "/workspace").expect_err("zero port");
        assert!(format!("{err:#}").contains("CANG_WORKSPACE_RELAY_PORT is zero"));
    }
}
