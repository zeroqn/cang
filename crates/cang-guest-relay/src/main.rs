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

use anyhow::{Context, Result};
use clap::Parser;
use std::io::Write;

use cang_guest_relay::READY_MARKER;
use cang_guest_relay::server::RelayServer;
use cang_guest_relay::vsock::VsockListener;

#[global_allocator]
static ALLOC: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;

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
}

fn main() -> Result<()> {
    let args = Args::parse();
    let listener = VsockListener::bind(args.port)?;
    println!("{READY_MARKER} port={} root={}", args.port, args.root);
    let _ = std::io::stdout().flush();

    loop {
        let mut connection = listener
            .accept()
            .context("the workspace relay's listener stopped accepting")?;
        // A fresh server per connection: the passthrough's inode table belongs to
        // the connection that opened its handles, and the host pump opening a new
        // one (after a restart) simply gets a fresh view. The host's own cache
        // timeouts make that invisible (note 62 §10).
        let server = RelayServer::new(&args.root, fuse_backend_rs::passthrough::Config::default())?;
        let mut reader = &connection;
        let mut writer = &connection;
        let served = server.serve(&mut reader, &mut writer, |line| {
            println!("{line}");
        });
        let _ = connection.flush();
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
