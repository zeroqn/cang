//! The guest half of cang's workspace reverse share.
//!
//! `cang --workspace-disk --workspace-relay` mounts the launch directory on the
//! host through a FUSE relay whose server runs *in the guest*, over the btrfs
//! tree the guest mounted at `/workspace`. This crate is that server:
//!
//! 1. bind an `AF_VSOCK` stream listener inside the guest (libkrun's vsock
//!    device carries it to a host unix socket, `listen = true`);
//! 2. accept one connection at a time (a second concurrent stream is out of
//!    scope for stage 1);
//! 3. serve the FUSE protocol over that connection with
//!    `fuse_backend_rs::passthrough::PassthroughFs` rooted at `/workspace`.
//!
//! The wire shape carries no extra framing: one FUSE message is one contiguous
//! write, sized by its own header (`in_header.len`, then `out_header.len`).
//!
//! Two protocol facts cost the spike (note 62) most of its debugging time and
//! are encoded here:
//!
//! * `FORGET`/`BATCH_FORGET` carry **no reply**, so nothing may wait for one -
//!   but they must still be dispatched: `PassthroughFs` keeps an `O_PATH`
//!   descriptor per looked-up inode, and only `forget` drops it (a relay that
//!   merely forwards the bytes leaks one guest descriptor per path the host
//!   ever touched, which ends in `EMFILE` on a long-lived mount);
//! * `FuseDevWriter::commit()` writes to its file descriptor, which
//!   `READDIRPLUS` does while composing a split reply. The writer is given
//!   `/dev/null`: the bytes are already aliased into the reply buffer, and the
//!   commit write is discarded. (With `fd = -1` every directory listing failed
//!   `EBADF` and the whole reply stream shifted.)
//!
//! A guest-side write (`cargo fmt`, a checkout) is invisible to the host's page
//! cache, so the relay can also tell the host's kernel to drop what it cached:
//! `inval` composes the `INVAL_INODE`/`INVAL_ENTRY` notifications, `nodeids`
//! remembers the nodeids its own replies handed out, and `watch` turns the
//! guest's `inotify` events into them (`CANG_WORKSPACE_RELAY_WATCH=1` or
//! `--watch`; off by default).
//!
//! The INIT reply advertises `FUSE_BIG_WRITES | FUSE_MAX_PAGES`
//! (`max_write = 1 MiB`, `max_pages = 256`). `fuse-backend-rs` composes that
//! reply from what the *filesystem driver* asks for, and its `passthrough`
//! driver asks for neither flag, so the reply would cap `max_write` at 4 KiB and
//! a 4 MiB write would be 1 024 frames at ~10 MB/s (note 63). The relay states
//! those limits on the reply it forwards ([`server::advertise_big_writes`]),
//! which is the same negotiation without a patch to a third-party tree.

pub mod inval;
pub mod nodeids;
pub mod server;
pub mod vsock;
pub mod watch;

/// Marker the relay prints once its listener is bound, so `cang-guest-init`
/// can hold the task command until the host's mount has something to talk to.
pub const READY_MARKER: &str = "CANG_WORKSPACE_RELAY_READY";

#[cfg(test)]
mod marker_tests {
    use super::READY_MARKER;

    /// `cang-guest-init` holds the task command until it reads this line, and it
    /// carries the same literal rather than a dependency on this crate (the
    /// bootstrap must not link a FUSE server). A drift here would make every
    /// `--workspace-relay` launch wait out its 10 s readiness timeout, so this
    /// test pins the protocol word.
    #[test]
    fn the_ready_marker_is_the_agreed_protocol_word() {
        assert_eq!(READY_MARKER, "CANG_WORKSPACE_RELAY_READY");
    }
}
