---
label: wayfinder:research
title: Can the serving guest keep the host's cache fresh for its own tree writes?
status: open
blocked_by: []
claimed_by: ""
---

## Question

Ticket 23 wrote down that a host reader may see a stale file until it reopens it,
because a guest-side write (`cargo fmt`, a codegen step, `git checkout` inside the
VM) is invisible to the host's page cache. The FUSE protocol has the lever -
`FUSE_NOTIFY_INVAL_INODE`/`FUSE_NOTIFY_INVAL_ENTRY` (this host's `linux/fuse.h`
lists them, plus `DELETE`/`RESEND`/`EPOCH`/`PRUNE`) - so the question is what it
costs to use them.

Resolve, read-only plus a bounded measurement if a mount exists by then:

1. **The watcher.** Watching a build-heavy btrfs tree with `inotify` costs one watch
   per directory (`fs.inotify.max_user_watches` on this host is ...) and fires on
   every `target/` write. Is `fsnotify`/`inotify` the right tool, is there a btrfs
   generation/`BTRFS_IOC_SYNC`-based alternative, or should the server instead
   invalidate on its *own* writes only and leave direct guest writes stale?
2. **The notification.** Does the kernel accept `INVAL_INODE` on a mount whose server
   is a relay (the frames go guest -> host pump -> `/dev/fuse`), and which of
   `INVAL_INODE`/`INVAL_ENTRY`/`DELETE` does this kernel version honour (it lists
   `FUSE_NOTIFY_RESEND`, so it is a recent one - check the version gates in
   `fs/fuse/dev.c`).
3. **What `fuse-backend-rs` already gives**: does its server have a notification
   handle, and is it reachable when the transport is a socket rather than a
   virtqueue (the risk note 58 §5.3 named)?
4. **Verdict**: an ordered recommendation with a measured cost (watches, events/s on
   a `cargo build`, per-invalidation latency) or a justified "not worth it, keep the
   contract as written".

## Resolution

(to follow)
