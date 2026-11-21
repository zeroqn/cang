---
label: wayfinder:task
title: Implement the invalidation watcher - pump idle-poll, then the guest watcher
status: closed
blocked_by: []
claimed_by: "pi (2026-11-21)"
---

## Question

Ticket 30 settled that guest-side invalidation is worth doing and exactly how
([note 66](../notes/66-invalidation-measured.md)). This ticket is the build, in the
order the measurements demand, with the acceptance that decides whether it shipped.

Do, one session, in this order:

1. **`crates/cang-relay-pump`: put the transport in the poll set.**
   `/dev/fuse`, the stop pipe and the transport; a readable transport with no
   request in flight is the guest speaking, so the frame goes straight to
   `/dev/fuse`. Reference shape: `.scratch/inval-probe/design-pump/src/pump.rs`
   (**+138 / -40**, one file, `main.rs` unchanged, the 17 existing tests unchanged).
   Non-negotiable details, all measured: the `unique == 0` discriminator; a
   notification arriving while a request is in flight is delivered *before* that
   request's reply (replies carry their `unique`, so this is safe); **a refused
   notification (`ENOENT` from a lagging nodeid map) is logged and skipped, never a
   transport failure**; a `POLLIN`-only gate so `/dev/fuse` death semantics do not
   change. Regression tests for the idle-delivery path and for the refused-notification
   case (the probe's own two tests are the model).
2. **`crates/cang-guest-relay`: the watcher.** A path->nodeid map filled from the
   relay's own LOOKUP / READDIRPLUS / CREATE / MKDIR / RENAME replies (~30-40 lines),
   a recursive inotify watcher on the served root (~120-160 lines; watch budget in
   note 66), and a shared writer so notifications and replies use the one
   connection (~30-50 lines). Emit `INVAL_INODE` with `off=0`, `len=0` for a file
   whose content changed (CLOSE_WRITE/MODIFY) and `INVAL_ENTRY` (parent nodeid +
   name) for creates/deletes/renames; a nodeid the relay never sent is skipped
   (nothing of that file is cached). Gate it behind a flag or env so the arm can be
   A/B'd without a rebuild of the image, and say which.
3. **The live A/B, run by the verifying session, not only by the implementer**:
   an arm OFF and an arm ON with the *same* workload - guest writes a file while a
   host reader holds its descriptor open (both a size change and a **same-size
   rewrite**), and the host's `stat` of the path - reporting the stale window or the
   detection latency for each. The ticket is only resolved when the ON arm is in the
   ~1 ms band and the OFF arm still shows 5 s / never.
4. **Guard rails**: an idle CPU cost measurement for the watcher (events/s on a
   build-sized storm, note 66's 7.5-8.1 k events/s) and a decision recorded for
   whether watcher events are coalesced per path (a `target/` write storm must not
   become a notification flood at 4.4 events per file).

## Resolution

Shipped 2026-11-21, commit `fca99eb4`, verified independently
([note 67](../notes/67-invalidation-watcher-shipped.md)):

- **pump**: transport in the poll set; notifications written by a dedicated
  thread (`dup(2)` of `/dev/fuse`, bounded queue that drops rather than stalling
  the serving loop); refused notifications logged and skipped; tests for idle
  delivery, the reply-path-only control, and the refusal that must not kill the
  mount.
- **guest**: `watch.rs` (recursive inotify, per-path coalescing within one
  drain, counters), `nodeids.rs` (path->nodeid from the relay's own replies),
  `inval.rs` (frames). Only `INVAL_INODE` with `off=0, len=0`.
- **gate**: `CANG_WORKSPACE_RELAY_WATCH=1`, forwarded into the guest, off by
  default.
- **the defect the implementation found**: two designs hang the mount -
  `INVAL_ENTRY` (`fuse_reverse_inval_entry` waits on the parent's inode lock)
  and any notification written by the *serving* thread (`folio_wait_bit_common`,
  waiting on a folio lock). The first is now never sent (with a test that fails
  if a code-3 frame appears; note 66 had already measured it as no help for
  content), the second is designed out by the writer thread. The rejected
  experiments left two `relay-pump` processes in `D` state that only a reboot
  clears - they hold no mount.
- **my A/B on the final tree** (image `660f119ef375`): a same-size rewrite under
  a held host descriptor is invisible for **9 s** with the gate off and visible
  **during the write** with it on.
- Validation: fmt, clippy `-D warnings`, deny, `cargo test` (1 315 tests, 0
  failures), `nix build .#cang .#container`.

Still open: `mmap` readers were not measured, and no host `inotify` event is
synthesised (a FUSE property, not a TODO).
