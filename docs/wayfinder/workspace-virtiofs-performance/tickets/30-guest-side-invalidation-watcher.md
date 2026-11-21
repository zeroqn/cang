---
label: wayfinder:research
title: Can the serving guest keep the host's cache fresh for its own tree writes?
status: closed
blocked_by: []
claimed_by: "pi (2026-11-21)"
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

## Facts established (2026-11-21, source + host config - before any measurement)

- **The kernel needs no feature bit for this.** A write to the mount's own
  `/dev/fuse` whose `out_header.unique` is `0` is dispatched as a notification by
  `out_header.error` (`fs/fuse/dev.c`, `fuse_dev_do_write` -> `fuse_notify`);
  `FUSE_NOTIFY_INVAL_INODE` and `FUSE_NOTIFY_INVAL_ENTRY` are both in the switch
  with **no check of what INIT negotiated**. Sizes are exact:
  `INVAL_INODE` needs `len == 24` (`ino`, `off`, `len`), `INVAL_ENTRY` needs
  `len == 16 + namelen + 1` (`parent`, `namelen`, `flags`), namelen <= 1024.
- **What each one actually does.** `fuse_reverse_inval_inode` (fs/fuse/inode.c)
  bumps `attr_version`, calls `fuse_invalidate_attr` and drops cached ACLs, then
  drops page cache **only when `offset >= 0`** (`off = -1` invalidates attributes
  but keeps pages; `off = 0, len = 0` is the full page drop). So an editor that
  re-reads content needs `off = 0`, not `-1`. `fuse_reverse_inval_entry`
  (fs/fuse/dir.c) needs the parent's nodeid plus the name and does
  `d_invalidate` unless `FUSE_EXPIRE_ONLY`.
- **`fuse-backend-rs` cannot send them on our transport.** Its
  `notify_inval_entry`/`notify_resend` exist only under the `fusedev` feature and
  take a `FuseDevWriter` (a buffer writer), not a socket. Composing the frames by
  hand is trivial - `crates/cang-guest-relay/src/server.rs` already builds frames.
- **The pump is strictly serial**, and that is the real cost: after writing a
  request it reads exactly one frame as *the reply* (`crates/cang-relay-pump/src/pump.rs`),
  so a notification arriving at that moment is misframed and the reply stream
  desynchronizes. Supporting notifications means at least "drain `unique == 0`
  frames between requests", at most a concurrent pump that matches replies by
  `unique`. Which one is needed is what the measurement below decides.
- **Host side config**: `fs.inotify.max_user_watches = 524288`,
  `max_user_instances = 524288`, `max_queued_events = 16384`; host kernel
  `7.3.4-xanmod1`.

```
## Resolution

**Yes, worth it - and the shape that works is not the one the ticket sketched.**
Measured 2026-11-21 through the product relay ([note 66](../notes/66-invalidation-measured.md)):

1. **The kernel honours it, ungated.** `INVAL_INODE` (40-byte frame, `off=0`,
   `len=0` or `-1`) makes a host reader that *holds an open descriptor* see a
   guest write in **0.77-1.35 ms** (n=8), against a **4.95-4.99 s** stale window
   with no notification, and **never** for a same-size rewrite. `off=-1` is
   attributes-only (pages stay stale), and `INVAL_ENTRY`/`DELETE` refresh names
   and attributes but never an open inode's page cache - so `INVAL_INODE` with
   `off=0` is the content lever.
2. **A plain `open` was never stale**: `CachePolicy::Auto` drops cached pages on
   every `open`, so `cat`/`git`/editor-open were always fresh. That validates
   ticket 23's wording ("stale until it reopens it") and narrows the problem to
   long-lived descriptors - the agent harness itself, an editor, an mmap - where
   a same-size rewrite is stale **indefinitely**.
3. **The pump must poll the transport while idle.** The reply-path-only drain
   only delivers while a request is in flight, and the first post-write host
   access sends none: measured, the stale window stayed at **4.98 s** (= OFF).
   With the transport in the poll set, an idle frame reaches `/dev/fuse` in
   **135.6 us** (offline, real relay loop, two tests pin it) and end to end
   **~1 ms**. Cost: **+138 / -40 lines in one file** (`pump.rs`, `main.rs`
   unchanged), **1.57 us** per delivered notification, all 17 existing pump
   tests unchanged, and one required rule - **a refused notification (`ENOENT`
   from a lagging map) is logged and skipped, never a transport failure**.
4. **The watcher is cheap**: 1 374 directories in this repo (3 641 with a
   populated `target/`) against the guest's **122 262** watch limit, and a storm
   produces **2 899-2 910 events** per 666-file extraction (7.5-8.1 k events/s,
   4.4 per file).
5. **What it does not do**: no host `inotify` event is synthesised (FUSE
   invalidates cache, it does not deliver events), and `mmap` readers were not
   measured.

Resolution: implement it, as [ticket 32](32-invalidation-watcher-implementation.md) -
the pump's idle-poll change first (it is the whole difference between "still
stale" and "fresh in 1 ms"), then the guest watcher (path->nodeid map + inotify),
then the live A/B re-run by the host session.
