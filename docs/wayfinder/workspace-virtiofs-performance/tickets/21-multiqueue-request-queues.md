---
label: wayfinder:task
title: Advertise N virtio-fs request queues - parallelise the queue handoff, not the pool
status: open
blocked_by: []
claimed_by:
---

## Question

The pool shipped and the request-removing levers are spent, yet parallel metadata
churn on the share still tops out at **7-8 k creates/s (~28 k FUSE requests/s)**
while the tmpfs control reaches 25-29 k, and per-create latency grows from
0.53 ms solo to ~4 ms at 26-way concurrency. Note 44 priced every thread-count
explanation out of that gap:

- the single `fs worker` dispatcher used **40.4 %** of one core during a 26-shard
  storm, and each of the four `fs work` pool threads **17.4-17.6 %** - nothing is
  saturated, and 1.14 cores of FUSE CPU could serve ~4x the requests we get;
- raising `CANG_FS_WORKER_THREADS` from 4 to 16 changed nothing (mean 6.3 k vs
  5.8 k creates/s @26, two interleaved rounds per side), so the pool is not a knob;
- completion signalling is already batched - 651 host->guest interrupts/s against
  ~28 k requests/s, i.e. ~43 replies per notification - so kick batching is spent
  too;
- the connection's background-request credit (kernel default 12) is not reachable
  from inside the guest at all: there is no `/sys/fs/fuse/connections/<id>/` entry
  for a virtiofs mount;
- and the guest kernel would use **N** request queues if the device advertised
  them: `virtio_fs.c` reads `num_request_queues` from the config word and treats
  `0` as `min(ncpus, 64)`, giving each queue its own `fuse_iqueue`.

So the remaining cost is **one serialized kick -> dispatch path per request**, and
the lever is the queue count, not the thread count. Upstream virtiofsd has this
shape; cang's fork does not (`defs::NUM_QUEUES = 2`, `config.num_request_queues =
1`, `device.rs:83`).

Do:

1. **Make the request-queue count dynamic in the fork**: size `QUEUE_CONFIG` as
   `1 + N`, set `config.num_request_queues = N` (no feature bit exists - the
   config word is the whole negotiation), and serve `N` request event fds from
   `work()`'s epoll set instead of the single `REQ_INDEX` fd. Decide whether that
   is one dispatcher thread over N queues or one dispatcher per queue; the pool's
   `FsJob`/`FsCompletion` already carry `queue_index` and replies already publish
   into `queues[queue_index]`, so the server side needs no refactor either way.
2. **Gate it per device and per launch** the way the pool and the other levers
   are: a fork `FsDevice::set_request_queues` (+ `include/libkrun.h`, regenerated
   bindings, `CANG.md`), surfaced from cang as `CANG_FS_REQUEST_QUEUES` /
   `--fs=request-queues:N`, default 1 so the A/B needs no image rebuild and the
   shipped default changes only after it measures.
3. **Prove the guest actually uses them**: count the request-queue interrupts in
   the guest (`/proc/interrupts` entries per request queue) and confirm requests
   spread across queues rather than piling on queue 0.
4. **A/B 1 vs 4 (and 8 if 4 pays)** with the arms in note 44: the 26-shard
   `storm.py` create/s figure, `probe5.py`'s create/read scaling ladder, the
   per-path small-file classes, and one cold 2401-crate `tools/rust-compile-bench`
   arm as the regression guard. Interleaved rounds per side, one boot per arm,
   kernel/binary/image identity recorded, repo clean before and after.

**Done means**: either the storm rate moves materially above 7-8 k creates/s
(toward the 25-29 k tmpfs control) with host CPU per request unchanged or lower
and no build regression - then it ships as a default with numbers - or it measures
flat, the fork keeps `1`, and this note closes the last queue-side explanation of
the metadata ceiling, leaving only per-request cost inside the fork (30 us pool +
11 us dispatcher) as an explanation and no further lever.

## Starting facts (verified 2026-11-04, this checkout)

- `defs::NUM_QUEUES = 2` with `HPQ_INDEX = 0` / `REQ_INDEX = 1` and a static
  `QUEUE_CONFIG: [QueueConfig; 2]` (`deps/libkrun/src/devices/src/virtio/fs/mod.rs:95-101`);
  `config.num_request_queues = 1` (`.../fs/device.rs:83`); the whole device is
  registered once from `launcher.rs` (`fs_device_new(CANG_FS_ROOT_TAG, …)`).
- `worker.rs::work` epolls exactly two queue fds plus the stop and pool-completion
  fds and dispatches to `handle_event(HPQ_INDEX)` / `handle_event(REQ_INDEX)`;
  `process_queue(queue_index)` takes the index but still reads the event fds from
  the `REQ_INDEX`/`HPQ_INDEX` constants.
- `FsJob { queue_index, index, desc_table, queue_size }` and
  `FsCompletion { queue_index, index, len }` - the pool is already queue-agnostic.
- Guest kernel: `virtio_cread_le(...num_request_queues...)`; `0` means
  `min(num_online_cpus(), VIRTIO_FS_DEFAULT_QUEUE_SIZE)`; no module param, no
  feature bit (ticket 15's correction stands, this ticket does not contradict it).
- `/sys/fs/fuse/connections` is empty in the cang guest, so `max_background` /
  `congestion_threshold` cannot be part of the experiment.

## Numbers to beat

See [note 44](../notes/44-queue-depth-not-thread-count.md): storm 7 002-8 392
creates/s; scaling ladder 1 075 / 4 686 / 7 359 for `/workspace` at 1 / 8 / 26
processes; tmpfs control 2 113 / 19 378 / 29 008; single-stream classes 1.9 k
create/s, 1.26 k 4 KiB write+close/s, 0.95 k write+rename/s.
