---
label: wayfinder:research
title: What can this guest kernel and the fork's FUSE server actually negotiate, and where does upstream virtiofsd differ?
status: closed
blocked_by: []
claimed_by: pi research child-1 (2026-10-25)
---

## Question

Before any virtio-fs knob is picked, name the ones that exist. The fork's device
(verified 2026-10-25) offers `VIRTIO_F_VERSION_1 | VIRTIO_RING_F_EVENT_IDX`
only, sets `config.num_request_queues = 1`, returns a static 2-queue
`queue_config`, serves them on one `FsWorker` thread, and advertises a FUSE_INIT
set of `DO_READDIRPLUS | READDIRPLUS_AUTO` (+ optional `SUBMOUNTS`,
`WRITEBACK_CACHE`). Answer, with primary sources:

1. **The guest side.** In the *pinned guest kernel* (`libkrunfw` `cang` branch,
   linux 7.2.7 + hardened; the configs are in `deps/libkrunfw/config-libkrunfw_*`),
   read `fs/fuse/virtio_fs.c` and `fs/fuse/inode.c`: how does the driver size
   its request queues (`num_request_queues` module param vs the device config
   value vs `nr_cpu_ids`), what does it do when `VIRTIO_F_MULTIQUEUE` is not
   negotiated, and which FUSE_INIT flags does the kernel *use* when the server
   offers them (WRITEBACK_CACHE, ATOMIC_O_TRUNC, PARALLEL_DIROPS,
   EXPLICIT_INVAL_DATA, NO_OPEN_SUPPORT, NO_FLAVOR_SUPPORT, HANDLE_KILLPRIV,
   ASYNC_READ, MAX_PAGES/BIG_WRITES, MAP_ALIGNMENT)? Cite file:line from the
   kernel tree that this config actually builds.
2. **What each flag buys for a rustc-shaped load** (many `statx`/`open`/`read`/
   `mmap` of small files, plus `target/` writes): which flags remove guest->
   host round trips, and which merely change semantics. Be explicit about
   `ATOMIC_O_TRUNC` (rustc writes/truncates constantly), `PARALLEL_DIROPS`, and
   entry/attr timeout sizing. Say which of these the fork could advertise
   **without** changing its coherence contract, and which cannot.
3. **The server side, measured against upstream.** Compare the fork's
   `linux/passthrough.rs` (`init` at ~`:897`, `Config` at ~`:320-371`) with
   upstream `virtiofsd` (gitlab.com/virtio-fs/virtiofsd) and with
   `libkrun` upstream: what does virtiofsd advertise by default, what are its
   `--cache=auto|always|never`, `--thread-pool-size`, `--inode-file-handles`,
   `--announce-submounts` semantics, and how does its worker actually dispatch
   FUSE requests across threads (per-queue threads? a pool? how is the inode map
   shared?). If virtiofsd already solved the concurrency problem, name the
   mechanism and the files, since the fork's `Server`/`PassthroughFs` state
   model is what a multi-queue design must confront.
4. **Host requirements.** `FUSE_PASSTHROUGH` on this host (`7.2.11-xanmod1`):
   what it needs from the exporter (VMM-opened fds, `fuse_backing`, the
   `FUSE_DEV_IOC_BACKING_OPEN` ioctl), whether it is usable when the daemon is
   the VMM rather than a standalone `virtiofsd`, and what the guest needs
   (`CONFIG_FUSE_PASSTHROUGH=y`, kernel >= ?). Same for `FUSE_IO_URING`: is
   `CONFIG_FUSE_IO_URING` reachable at all from a VMM-hosted server, or is it a
   kernel-internal transport for `/dev/fuse` only?
5. **Ordering recommendation.** Given the above, rank the candidate levers by
   (expected win on that load) / (risk + fork work): FUSE_INIT flag set,
   `attr/entry` timeouts, writeback cache, parallel request queues + worker
   pool, DAX window, passthrough/io_uring. Ticket 04 is the grilling that picks,
   so give it a defensible ordering rather than a single answer.

## Deliverable

`notes/01-fuse-negotiation-and-queue-facts.md`: the answer per item with
file:line citations for the kernel, the fork and virtiofsd, plus one table of
"flag -> guest round trips removed -> coherence cost -> fork work required".

## Resolution (2026-10-25, pi research child-1)

**Most of this lever is already spent: the fork negotiates essentially every
free FUSE_INIT flag today, the "flag set" lever this ticket was scoped against
does not exist, and only two changes are real - writeback cache and a worker
pool over the existing single request queue.** Deliverable:
`../notes/01-fuse-negotiation-and-queue-facts.md` (39 KB, all items answered with
kernel/fork/virtiofsd citations). Two of the map's starting facts were **wrong**
and are corrected in `MAP.md`:

- **The FUSE_INIT set is not minimal.** `server.rs:897-908` advertises
  ASYNC_READ, PARALLEL_DIROPS, BIG_WRITES, AUTO_INVAL_DATA, ASYNC_DIO,
  HAS_IOCTL_DIR, ATOMIC_O_TRUNC, MAX_PAGES, SUBMOUNTS, HANDLE_KILLPRIV_V2,
  INIT_EXT (+`ALLOW_IDMAP`), plus DO_READDIRPLUS from the fs layer; the guest
  offers all of it and the fork is at virtiofsd parity. `MAX_PAGES` is already at
  the kernel's 1 MiB ceiling. The anonymous-fd `ALLOW_IDMAP` only survives
  because a virtio-fs mount forces `default_permissions`+`allow_other`
  (`virtio_fs.c:1552-1562`), so the guest sends **no** FUSE_ACCESS at all.
- **No inode-map refactor is needed for parallelism.** `Server::handle_message`,
  `PassthroughFs` and `AugmentFs` methods are already `&self` over
  `RwLock`/`Atomic` state (`passthrough.rs:392-408`, `augment_fs.rs:51-86`), so
  `Arc<FsServer>` can be shared as-is; the work is confined to `worker.rs`
  (used-ring + notification behind a mutex, `disable/enable_notification` kept
  on the eventfd-owning thread, a bounded pool) with virtiofsd's
  `process_queue_pool` as the template. A worker pool **without**
  `VIRTIO_F_MULTIQUEUE` is the cheap structural win; multiqueue buys nothing for
  one share.
- **Reachability, answered:** every per-connection knob (INIT flags, timeouts,
  cache policy) reaches `/workspace` because there is one share; DAX is
  per-device **plus a guest mount option** and is reachable without a second
  device, but only whole-share. `FUSE_PASSTHROUGH` and `FUSE_IO_URING` are
  **structurally unreachable** while the FUSE server is the VMM (no `/dev/fuse`,
  no VMM-opened backing fds) - drop them as candidates.
- **Queue sizing, answered:** the driver takes the device config's
  `num_request_queues` then `nr_cpu_ids`; there is no module param, and with no
  `VIRTIO_F_MULTIQUEUE` the missing bit simply means one queue - nothing breaks.
- **Ordering handed to ticket 04:** (1) WRITEBACK_CACHE (near-zero fork work, the
  only lever that helps the write half; risk is purely coherence, i.e. ticket
  03), (2) the worker pool (highest ceiling, lifts read/metadata and multiplies
  every other lever), (3) attr/entry timeout sizing, (4) NO_OPEN_SUPPORT /
  CACHE_SYMLINKS (real round-trip savings but a handle-model change), (5) DAX
  (reachable but large, whole-share, 2 MiB granularity, ill-suited to small-file
  reads), (6) multiqueue, (7) passthrough/io_uring (not levers).
- **Not settled:** no numbers anywhere in the note - magnitudes are mechanism
  arguments, which is exactly what ticket 02 must replace with measurements.
