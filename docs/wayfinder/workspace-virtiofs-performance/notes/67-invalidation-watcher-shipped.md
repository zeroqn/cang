# The invalidation watcher, shipped and re-verified

2026-11-21. Ticket 32 implementation, verified by an independent pass (mine) on
the final tree. Numbers for the mechanism itself are in
[note 66](66-invalidation-measured.md); this note records what shipped, what I
re-ran, and the one defect the implementation itself found in its first design.

## What shipped

- `crates/cang-relay-pump`: the transport is in the pump's **poll set**, so a
  notification the guest sends while no request is in flight reaches `/dev/fuse`
  immediately instead of waiting for the next request. Notifications are written
  by a **dedicated thread** (`relay-pump-notify`, its own `dup(2)` of
  `/dev/fuse`, bounded queue that drops rather than stalling the serving loop).
- `crates/cang-guest-relay`: `watch.rs` (recursive inotify over the served root,
  per-path coalescing within one drain batch, counters), `nodeids.rs` (the
  path->nodeid map filled from the relay's own LOOKUP/READDIRPLUS/CREATE replies),
  `inval.rs` (the frame builders). Only `FUSE_NOTIFY_INVAL_INODE` with
  `off = 0, len = 0` is sent.
- Gate: `CANG_WORKSPACE_RELAY_WATCH=1` on the `cang` command (forwarded into the
  guest), **off by default**, so OFF and ON arms run the same command line and
  the same image.
- `docs/usage.md` states what is promised: held descriptors refresh in ~1 ms, a
  plain `open` was always fresh, and **no host `inotify` event is synthesised**.

## The defect the first design had (measured, then designed out)

Two ways of sending a notification from a single-threaded or careless path
**hang the mount**, and both are now impossible by construction:

| what was tried | what happened | `/proc/<pid>/wchan` |
| --- | --- | --- |
| `INVAL_ENTRY` for a created/renamed name | `stat` through the mount never returned; the pump never completed the frame and could not be killed | `fuse_reverse_inval_entry` |
| notification written by the *serving* thread | same hang, from the page-lock side | `folio_wait_bit_common` |

The cause is the same both times: the kernel's invalidation path takes the
parent's inode lock (entry) or the folio lock (inode page drop), and the host
task holding that lock is waiting for a FUSE reply that only our pump can deliver.
If the pump is the thread that is blocked in the notification write, nothing
answers, and the cycle never breaks. Consequences now in the code:

- **`INVAL_ENTRY` is never sent** - the watcher classifies and counts
  `IN_CREATE`/`IN_DELETE`/`IN_MOVED_*` but sends nothing for them, with a test
  that fails if a code-3 frame ever appears. Nothing is lost: note 66 measured
  that `INVAL_ENTRY` does not refresh an open descriptor's pages, and a stale
  name is a 5 s entry-timeout matter.
- Notifications are written by their own thread, so the serving loop keeps
  answering while a write waits on a folio lock, and the lock holder finishes.
- A refused notification (`ENOENT` for a nodeid the mount no longer holds) is
  logged and skipped, never a transport failure.

Those two experiments left **two `relay-pump` processes in `D` state**
(`1116884` wchan `fuse_reverse_inval_entry`, `1126187` wchan
`folio_wait_bit_common`). They hold no mount and no lock the shipped code needs,
but they cannot be killed (`kill -9` is refused) and only a **reboot** clears
them. They are from the rejected designs, not from the shipped one.

## My independent A/B (final tree, image 660f119ef375, binary /nix/store/kj6b2jj...)

Workload: the server VM serves a 4 GiB btrfs image; the guest writes `f.txt`
(10 bytes `AAAAAAAAAA` -> 10 bytes `BBBBBBBBBB`, **same size**), while a host
process holds the descriptor open and re-reads every ~0.4 ms
(`.scratch/inval-probe/tools/held-reader`).

| arm | result |
| --- | --- |
| OFF | **no change in 9 s** (reader's `ready` line only) - the same-size rewrite stays invisible, as note 66 measured |
| ON (`CANG_WORKSPACE_RELAY_WATCH=1`) | reader saw the new bytes **during the write** (its `changed` stamp is 5 ms *before* `cang exec` returned; the frame was in `serve.log`: `notification from the guest (code=2, 40 bytes) -> /dev/fuse`) |

No new `D`-state process appeared from either arm: the shipped configuration
(gate off, `INVAL_INODE` only, own writer thread) is safe in this arm.

## Validation on the final tree

`cargo fmt --check`, `cargo clippy --all-targets --all-features -D warnings`,
`cargo deny check`, `cargo test` (**1315 tests across 22 suites, 0 failures**),
`nix build .#cang .#container` from this tree, image loaded as
`localhost/cang:latest 660f119ef375` and used for the arms above.

## Still open

- `mmap` readers were not measured (read(2) through the page cache is what the
  evidence covers).
- The watcher does not synthesise host `inotify` events and never will - that is
  a property of FUSE, not a TODO.
- A handoff (a task VM taking the image while the server survives) remains
  unimplemented; with the single-writer lease there is nothing for the read-only
  flip of ticket 23 §3 to act on yet.
