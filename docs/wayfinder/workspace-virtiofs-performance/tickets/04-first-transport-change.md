---
label: wayfinder:grilling
title: Which non-caching virtio-fs change ships first, and in what API shape?
status: open
blocked_by: [02]
claimed_by: ""
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

## Resolution

(to follow)
