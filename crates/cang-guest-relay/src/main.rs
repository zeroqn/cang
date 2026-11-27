//! `cang-guest-relay`: the guest half of cang's workspace reverse share.
//!
//! Started by `cang-guest-init` (after it mounted `/workspace` from the
//! workspace disk) when the launch set `CANG_WORKSPACE_RELAY_PORT`:
//!
//! ```text
//! cang-guest-relay --port 50430 --root /workspace
//! ```
//!
//! It binds the vsock port first, prints [`READY_MARKER`] (guest-init holds the
//! task command until it sees that line), then serves one host connection at a
//! time with a fresh `PassthroughFs` per connection - the host pump is the only
//! client, and a second concurrent stream is out of scope for stage 1.
//!
//! With `--watch` (or `CANG_WORKSPACE_RELAY_WATCH=1` in the guest environment,
//! which is what the launch forwards) it also watches the served tree and sends
//! the host's kernel `INVAL_INODE`/`INVAL_ENTRY` notifications for guest-side
//! writes, so a host reader that keeps a descriptor open sees a fresh file in
//! about a millisecond instead of after its 5 s attribute timeout (or never, for
//! a same-size rewrite). Off by default: it is the A/B's ON arm, and a launch
//! that never reads the tree through the mount pays nothing for it.

use anyhow::{Context, Result};
use clap::Parser;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use cang_guest_relay::READY_MARKER;
use cang_guest_relay::inval::{Notifier, SharedWriter};
use cang_guest_relay::server::RelayServer;
use cang_guest_relay::vsock::VsockListener;
use cang_guest_relay::watch::{self, WatchThread};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(
    name = "cang-guest-relay",
    version,
    about = "Serve the guest's /workspace to the host over a FUSE relay"
)]
struct Args {
    /// The vsock port to listen on inside the guest.
    #[arg(long)]
    port: u32,
    /// The tree to serve. The workspace disk is mounted here by guest-init.
    #[arg(long, default_value = "/workspace")]
    root: String,
    /// Watch the served tree and invalidate the host's cache for guest-side
    /// writes. `CANG_WORKSPACE_RELAY_WATCH=1` in the guest environment selects
    /// it too, which is how a launch turns it on without changing this command
    /// line (clap's own `env` support would read that value as clap's `bool`,
    /// so the variable is read directly).
    #[arg(long)]
    watch: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let listener = VsockListener::bind(args.port)?;
    println!("{READY_MARKER} port={} root={}", args.port, args.root);
    let _ = std::io::stdout().flush();

    let watch = watch::watch_enabled(args.watch, std::env::var(watch::WATCH_ENV).ok().as_deref());
    // The policy is what decides whether the mount's descriptor count tracks the
    // host's dcache or only its in-flight requests, so it is named in the log
    // rather than left to be inferred from a measurement.
    let config = cang_guest_relay::server::passthrough_config();
    println!(
        "relay: inodes stored as {}",
        if config.inode_file_handles {
            "file handles"
        } else {
            "O_PATH descriptors (no CAP_DAC_READ_SEARCH)"
        }
    );
    loop {
        let connection = listener
            .accept()
            .context("the workspace relay's listener stopped accepting")?;
        // A fresh server per connection: the passthrough's inode table belongs to
        // the connection that opened its handles, and the host pump opening a new
        // one (after a restart) simply gets a fresh view. The host's own cache
        // timeouts make that invisible (note 62 §10).
        let server = RelayServer::new(&args.root, config.clone())?;
        // Reading and writing are separate handles onto one connection: a
        // notification must be able to go out while the serve loop is blocked
        // reading the next request, so the reader must not hold the writer's
        // lock.
        let mut reader = connection
            .try_clone()
            .context("failed to duplicate the relay connection for reading")?;
        let writer = SharedWriter::new(connection);
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line: &str| println!("{line}"));

        let watcher = if watch {
            match WatchThread::start(
                Path::new(&args.root),
                server.node_ids(),
                Notifier::new(writer.clone()),
                Arc::clone(&log),
            ) {
                Ok(watcher) => Some(watcher),
                // The mount is still correct without it; only freshness is lost,
                // so it is named and the connection is served anyway.
                Err(err) => {
                    println!("relay: the invalidation watcher did not start: {err}");
                    None
                }
            }
        } else {
            println!(
                "relay: invalidation watcher off (--watch or CANG_WORKSPACE_RELAY_WATCH=1 selects it)"
            );
            None
        };

        let mut output = writer.clone();
        let served = server.serve(&mut reader, &mut output, |line| println!("{line}"));
        let _ = output.flush();
        drop(output);
        if let Some(watcher) = watcher {
            watcher.stop();
        }
        match served {
            Ok(summary) => println!(
                "relay: connection closed after {} frames{}",
                summary.frames,
                match summary.init {
                    Some(shape) => format!(" ({})", shape.log_line()),
                    None => String::new(),
                }
            ),
            Err(err) => println!("relay: connection ended: {err:#}"),
        }
        println!("relay: ops{}", server.ops.render());
        let _ = std::io::stdout().flush();
    }
}
