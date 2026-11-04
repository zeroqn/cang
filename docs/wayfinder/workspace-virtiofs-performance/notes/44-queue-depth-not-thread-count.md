# 44 - Queue depth, not thread count: why the metadata ceiling is not the worker pool

**Measured 2026-11-04** on `main` `d1b8fbf` (fork `v2.0.0-cang.17`), binary
`nix build .#cang`, image `.#container` loaded as `localhost/cang:latest`
(`ae5f7aca…`), guest kernel `7.2.10-hardened1`, `--mem 6`. One boot per arm, no
other VM on the host, repo clean before and after (the one source edit below was
reverted).

This note answers a single question that ticket 04 left standing: with the pool
shipped and the request-removing levers spent, **what actually caps
parallel-metadata throughput on the share?** Answer: per-request latency through
one serialized request queue. Not the pool's size, not the dispatcher's CPU, not
notification overhead.

## 1. Per-path numbers (the question this session started from)

Single-stream probe, 64 MiB sequential file + metadata classes, one boot per
path. `~/.cargo`, `~/.cache/sccache`, `~/.pi`, `/workspace`, `/` and `/nix` are
all grafts inside the one root export (see note 40), so these differ only by
their host filesystem underneath.

| path | write 64 MiB | read O_DIRECT | read via FUSE | create/s | unlink/s | 4 KiB write+close/s | write+rename/s | cold lookup/s | warm stat/s | open+read+close 4K/s | exec/s |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `/workspace` | 476 MB/s | 1601 MB/s | 684 MB/s | 1 918 | 5 468 | 1 255 | 946 | 5 021 | 330 k | 2 895 | 420 |
| `/` (root export) | 757 MB/s | 1 388 MB/s | 616 MB/s | 1 629 | 4 591 | 1 094 | 737 | 2 639 | 203 k | 2 124 | 327 |
| `~/.cargo` | 734 MB/s | 1 777 MB/s | 752 MB/s | 2 112 | 5 427 | 1 399 | 992 | 4 364 | 311 k | 3 868 | 426 |
| `~/.cache/sccache` | 673 MB/s | 1 684 MB/s | 782 MB/s | 1 998 | 5 314 | 1 443 | 1 060 | 4 273 | 382 k | 2 920 | 446 |
| `~/.pi` | 480 MB/s | 1 510 MB/s | 691 MB/s | 1 756 | 5 244 | 1 169 | 923 | 8 958 | 315 k | 4 718 | 401 |
| `/nix` (ro for uid 1000) | - | 1 045 MB/s (12 MB binary) | - | - | - | - | - | 2 933 | 327 k | 2 412 | 409 |
| `/tmp` (tmpfs control) | 3 708 MB/s | 10.7 GB/s | 12.0 GB/s | 188 k | 239 k | 109 k | 69 k | - | - | - | 395 |

Host yardstick (same probe, no FUSE): `/workspace`-backed dir 46.8 k create/s,
249 k cold stat/s, 53.5 k small reads/s, 498 MB/s write, 3.5 GB/s read; the
cargo/sccache dirs 53-54 k create/s and 14 GB/s read; host `/nix` 30 k cold
stat/s, 9.7 k small reads/s, 775 MB/s big read.

Two readings worth keeping: **metadata and small files cost 20-50x the host**
(`/nix` lookups only ~10x, small reads ~4x), sequential IO 2-4x, and `/tmp` is
not a write ceiling the share can be judged against (`/workspace` at 476 MB/s is
at its own host ext4's ceiling). Exec of a small binary is identical to tmpfs
(2.4-3.1 ms), i.e. process setup, not the share.

## 2. Concurrency scaling - where the ceiling shows up

Process-parallel arms (`probe5.py`, one boot per path, 200 files per shard,
Python shard processes), aggregate throughput:

| path | create/s 1 proc | 8 procs | 26 procs | small-read/s 1 | 8 | 26 |
|---|---|---|---|---|---|---|
| `/workspace` | 1 075 | 4 686 | **7 359** | 1 860 | 11 483 | 6 405 |
| `~/.cargo` | 1 054 | 4 505 | **7 332** | 1 855 | 11 555 | 6 128 |
| `/tmp` (control) | 2 113 | 19 378 | **29 008** | 1 784 | 11 410 | 6 169 |

Repeat rounds: 6 432 / 6 205 for `/workspace` @26, 6 650 / 5 623 for `~/.cargo`.
A 20 s storm with 26 shards sustains 7 002-8 392 creates/s (three runs). So the
share tops out around **7-8 k creates/s (~28 k FUSE requests/s)** while tmpfs
reaches 25-29 k. Per-create wall latency is 0.53 ms solo and ~4 ms at 26-way
concurrency.

## 3. Who burns the CPU during that storm (`runner2.out`)

Host `/proc/<pid>/task/*/stat` snapshots bracketing a 26-shard, 20.0 s storm
(156 680 creates, 7 819/s; 49 guest-visible threads):

| thread | CPU over the window | share of one core |
|---|---|---|
| `fs worker` (the queue's dispatcher) | 8.11 s | **40.4 %** |
| `fs work 0` / `1` / `2` / `3` (the pool) | 3.50 / 3.53 / 3.51 / 3.49 s | 17.4-17.6 % each |
| 26 x `fc_vcpu` | 0.82-1.84 s each | ~2.0 cores total |
| all threads | 80.86 s / 20.08 s | 4.03 cores |

**No thread is anywhere near saturated**, and the pool is idle half the time. Per
request that is ~30 us of pool CPU and ~11 us of dispatcher CPU; 1.14 cores could
serve ~4x the requests we actually get. The wall time is latency and queueing,
not CPU.

## 4. The worker pool is not the lever (A/B, cang-side one-liner)

`CANG_FS_WORKER_THREADS` in `crates/cang-libkrun/src/lib.rs:39` (4) raised to 16,
`nix build .#cang`, arms re-run through the same driver, two rounds per side,
interleaved 4 -> 16 -> 4 -> 16:

| variant | `/workspace` @26 | `/workspace` @8 | `~/.cargo` @26 | `~/.cargo` @8 |
|---|---|---|---|---|
| 4 workers | 6 432 / 6 205 | 4 386 / 3 498 | 6 650 / 5 623 | 4 170 / 4 158 |
| 16 workers | 5 083 / 6 428 | 2 326 / 4 288 | 6 042 / 5 867 | 3 937 / 4 055 |
| `/tmp` control @26 | 27 513 / 25 731 | 20 860 / 16 078 | - | - |

Mean @26: 6.3 k (4 workers) vs 5.8 k (16) - no gain, if anything worse; the
8-process row regresses. Small-file reads unchanged (5.3 k vs 5.3 k @26). The
const was reverted, the tree left clean. **Pool size is not a knob worth having.**

## 5. Notification overhead is already batched

Guest `/proc/interrupts` over a 10.46 s storm at 7 002 creates/s (~28 k
requests/s): the virtio request queue took **6 809 interrupts in 10.46 s = 651/s**,
i.e. ~43 replies per host->guest notification. Completion signalling is already
coalesced by the used-ring notification rules, so "batch the kicks" is not a
lever either. The guest side does pay for it: ~2.0 cores of vCPU time in the 26
vCPUs (~55 us of guest CPU per request).

## 6. Facts that constrain the remaining option

- **The fork advertises one request queue.**
  `deps/libkrun/src/devices/src/virtio/fs/defs.rs`: `NUM_QUEUES = 2`
  (`HPQ_INDEX = 0`, `REQ_INDEX = 1`), `QUEUE_CONFIG` a static 2-element array;
  `device.rs:83`: `config.num_request_queues = 1`. There is **no feature bit** to
  negotiate - the count is purely the config word (this is what ticket 15's
  correction already recorded).
- **The guest honours that word, and treats `0` as "as many as CPUs".** In
  `fs/fuse/virtio_fs.c` (read at v6.18, the shape the pinned 7.2.10 kernel shares):
  `virtio_cread_le(vdev, struct virtio_fs_config, num_request_queues, &fs->num_request_queues)`
  then `if (fs->num_request_queues == 0) fs->num_request_queues = min_t(unsigned int,
  num_online_cpus(), VIRTIO_FS_DEFAULT_QUEUE_SIZE)`; each request queue gets its
  own `fuse_iqueue` via `fuse_conn_init` -> `virtio_fs_fiq_ops`. So N-queue
  virtio-fs is a **host-side-only** change, exactly upstream virtiofsd's shape.
- **The connection's background-request credit cannot be raised from the guest.**
  `fuse_conn_init` sets `max_background = FUSE_DEFAULT_MAX_BACKGROUND` (12,
  congestion 9); `virtio_fs.c` never overrides it; and unlike `/dev/fuse` there is
  **no `/sys/fs/fuse/connections/<id>/` entry for a virtiofs mount** (measured: the
  directory is empty/absent in the cang guest), so the sysfs knobs that exist for
  a FUSE daemon do not exist here. Raising it needs a libkrunfw kernel patch (and
  it only governs *async* requests - a `create` is not one).
- **The pool plumbing is already queue-aware.** `worker.rs`'s `FsJob` and
  `FsCompletion` both carry `queue_index`, and replies are published into
  `queues[queue_index]`, so the server side does not assume one queue. What is
  single-queue today is `defs`, the config word, and `work()`'s epoll set (one
  `REQ_INDEX` event fd) with `process_queue(queue_index)` still reading the
  `REQ_INDEX`/`HPQ_INDEX` constants for the event fd.

## 7. What this leaves

The ceiling is one serialized kick -> dispatch path per request. Two candidate
shapes, in cost order:

1. **Advertise N request queues** (fork, host-side only): the guest spreads its
   requests across N `fuse_iqueue`s, each with its own virtqueue and event fd, so
   the handoff parallelises without touching the FUSE server. Ticket 21.
2. Per-request cost inside the fork (30 us pool + 11 us dispatcher) - only worth
   touching if (1) measures flat.

Payoff guard, unchanged and now sharper: a cold 2401-crate rustc build already
sits at 116.3 s vs 115 s host (~1 %), so queue work buys **metadata-heavy
workloads only** (`git checkout`/`status`, package installs, target-dir churn),
where the gap is 7 k vs 27 k ops/s.

## Reproducing

`/home/dev/cang/probe-vfs/.probe/`: `probe3.py` (single-stream per path),
`probe5.py` (process-parallel scaling), `storm.py` (26-shard churn),
`sample_threads.sh` + `runner1.sh` (host thread CPU attribution bracketing a storm
window via in-guest begin/end markers), `irq.py` (guest interrupt accounting),
`driver3.sh`/`driver4.sh` (one boot per arm, `ROUND=` label prefix). Raw JSON in
`results/` and the host yardsticks in `host/`; kernel sources read from
`raw.githubusercontent.com/torvalds/linux/v6.18/{fs/fuse/virtio_fs.c,fs/fuse/inode.c}`.
