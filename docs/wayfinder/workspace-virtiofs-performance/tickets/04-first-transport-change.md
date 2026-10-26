---
label: wayfinder:grilling
title: Which non-caching virtio-fs change ships first, and in what API shape?
status: closed
blocked_by: [02]
claimed_by: pi (2026-10-26)
---

## Question

Ticket 01 did the survey; ticket 03 then **ruled out write caching entirely**
(cang promises exact host visibility, so `WRITEBACK_CACHE` and `CACHE_SYMLINKS`
are out, and metadata timeouts ship only if a later measurement makes the case).
That leaves ticket 01's ordering, minus everything that caches:

1. **A worker pool over the existing single request queue** - highest ceiling and
   cheaper than this map first feared: no `VIRTIO_F_MULTIQUEUE`, no queue-count
   change, no inode-map refactor (server state is already `&self` +
   `RwLock`/`Atomic`); the work is `worker.rs` (used-ring/notification behind a
   mutex, notification ownership on the eventfd thread, a bounded pool) with
   virtiofsd's `process_queue_pool` as the template. Lifts the read/metadata half
   and multiplies every other lever.
2. NO_OPEN_SUPPORT (one fewer OPEN per open - the most frequent remaining
   request - but a handle-model change, and only if the pool leaves the number
   short); 3. DAX (ticket 05); 4. multiqueue (nothing left to buy after the
   pool); 5. attr/entry timeouts and passthrough/io_uring (measured later /
   dead).

Decide:

- **Ship order and grouping, against the numbers.** Ticket 02 measures how much
  of the penalty is really FUSE (guest-virtiofs vs guest-local): if the gap is
  small, say plainly that the pool is not worth the fork work and stop; if it is
  large, does the pool ship alone in its own `v<base>-cang.<n>` tag? Note cang
  pins no libkrun release - the `deps/libkrun` submodule pointer is the pin - so
  say what a fork release is even for here (upstream-facing artifact vs cang's
  own reproducibility).
- **The API shape.** How the knob reaches a user: a per-device option on the
  fork's `FsDevice` (`set_request_queues`, `set_worker_threads`)
  mirrored through `krun_fs_device_*`, consumed by `crates/cang-libkrun`'s
  `fs_device_new` (`linked.rs:317`) and set from `launcher.rs`; or a cang CLI
  flag; or always-on with no flag.
- **The judging numbers.** Exactly which arms and deltas ticket 02 must show
  before the change counts (e.g. cold full-build guest-virtiofs time closing by
  X% against the guest-local-disk arm), and the run-to-run spread that makes a
  delta believable on this host.
- **What must not regress**, and how each is re-run (ticket 03 named these):
  host edit -> guest rebuild -> host commit sees every file; a host-side `git
  status` agrees with what the guest wrote after a build; a hard-killed guest
  leaves the host tree usable; plus the Chromium/venus GPU smoke, the VA-API
  video arm (rules #223/#229), guest podman and `cang attach`.

## Resolution (2026-10-26, shipped)

**Both changes this ticket weighed shipped in one fork tag (`v2.0.0-cang.11`,
fork `d00b6d70`), and both were judged on measurement rather than prediction.**

- The **pool** (option 1) is in, 4 threads by default, `set_worker_threads(1)`
  restores the old behaviour: readdirplus at 16 guest threads 154 -> 316 ops/s,
  at 64 threads 149 -> 325, dispatcher CPU 96-98% -> 4%. It does **not** move a
  full build on its own (298-323 s either way).
- **Zero-message open** (ticket 19, which this ticket did not originally consider)
  is what moves the build: 427.1 -> 176.0 s, 2.42x - because the share is also
  `/`, so every `rustc`/`ld` launch was paying OPEN+RELEASE too.
- Judged against the corrected floor (131.5 s, not the lever-off 232 s), the
  share's cost went from **296 s to 45 s**; what remains is LOOKUP/FLUSH/WRITE/
  GETXATTR, which no data-path design in notes 05/18/24 touches.
- The regression gates ticket 03 named all hold, and the live smoke gate
  (Chromium-on-venus, VA-API decode/encode) passed on the tagged commit.

## Resolution (2026-10-26, pi) - **no worker pool: the single worker is 88% idle**

Ticket 02 produced the number this ticket was waiting for. Per-thread CPU of the
VMM during a full `JOBS=26` guest-virtiofs arm (314 s build, 1 Hz sampling):

- **`fs_worker` - the only thread serving the share, one request queue - used 12%
  of one core on average.**
- The guest's own vCPUs used ~65% (busiest) and ~22% each of the rest.

A worker pool lifts throughput only when the serving thread is the constraint. At
12% it is idle 88% of the time, so the pool would add complexity (used-ring and
notification ownership, a shared server, join semantics on reset) to a thread
that has nothing to do. The measured penalty is **per-request round-trip
latency**, paid in the guest (`fuse` client + virtio ring) and in a host syscall
per op, not a serialized server.

Rejected, with reasons, so nobody re-derives them:

- **worker pool** - above.
- **multiqueue** - strictly more work than the pool for the same non-existent
  saturation; also needs a guest-side driver request.
- **DAX** - ticket 05 (drop).
- **writeback cache / symlink caching / handle reuse** - ticket 03: cang promises
  host-immediate visibility, so no guest caching of any kind.
- **NO_OPEN_SUPPORT** - the one remaining round-trip saving that is *not*
  caching of data (it reuses an open handle for a `LOOKUP+OPEN` pair). Worth
  measuring only if the map is redrawn to allow a handle-table change; today there
  is no evidence it is worth it either, and 5s of guest round trips per build is
  what 12% worker utilisation already says is small.
- **FUSE passthrough / io_uring** - note 01 §4: not reachable when the exporter is
  the VMM.

**What the numbers say instead:** most of the guest's slowness is the VM itself
(2.2x the host), and the virtio-fs share adds ~30% on top of that floor. So the
"rustc is slow in a cang guest" complaint is answered by *fewer round trips per
build* (a compiler-side question: `-Z` options, unit counts, sccache) or by VM
sizing, not by the transport.

The one virtio-fs change this effort did ship is a correctness fix (ticket 08:
`f3167a42`, the worker no longer dies on a malformed guest descriptor chain), not
a performance one. Whether the map's destination ("at least one virtio-fs change
shipped **on that evidence**") is met by that, or whether the destination changes
to "measured and explained, no lever worth shipping", is bob's call - recorded in
the map.

### Status 2026-10-26: reopened with a split verdict (note 20)

Measured with the concurrency-scaling probe (`tools/rust-compile-bench/
concurrency-scaling.sh`, note 20), one guest boot, 4096-file tree, `/dev/shm` as
the no-FUSE control:

| op class | ops/s scaling 16/1 | `fs worker` CPU% (1 -> 64 thr) | verdict |
|---|---|---|---|
| `open` (open+close) | 3.85x | 13.7 -> 26.1 | not serialised |
| `read` (+`FUSE_READ`) | 3.16x | 13.6 -> 26.8 | not serialised |
| `statx` (forced) | 4.63x | 12.1 -> 22.9 | not serialised |
| `readdirplus` | 1.29x | **70.1 -> 95.2** | **serialised** |

Per-op latency budget at one thread: `open` 934 us wall / 411 us guest CPU /
128 us worker CPU; `read` 1189 / 511 / 162; `statx` 357 / 161 / 43; and
`readdirplus` **14.9 ms per 4096-entry scan**, of which **10.5 ms is that one
thread**. At 64 threads a scan's p50 goes 21.8 ms -> 1048 ms.

So the fork's one-worker design costs little on the file-at-a-time path - the
round trip is paid in the guest's FUSE client, not in the server - but it *is*
the limit for directory scans, which rustc/cargo do for every crate and every
`target/` walk. **The pool is worth building for that class**, and the open work
is: (a) size the win with the real workload (how much of the 70 s penalty is
`readdirplus`? note 20's tooling can count it), (b) implement the pool per note
15's inventory (~6 sites, `Vec<QueueConfig>` + per-queue threads), (c) fork tag,
submodule pin + `flake.lock`, and the live smoke set (rules #223/#229).
