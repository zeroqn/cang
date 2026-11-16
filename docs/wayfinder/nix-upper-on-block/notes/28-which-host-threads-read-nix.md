---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Which host threads read `/nix`: a block IO pool of 8, a virtiofs pool of 4

Neither path reads on one thread, and neither pool is configurable from cang today.

## Block lower (`--nix=lower:blk`)

`virtio-blk`'s worker builds an `IoPool` when `parallel_reads` or `parallel_writes` is set, and
cang sets reads explicitly:

- `crates/cang-libkrun/src/linked.rs:417` - `device.set_parallel_reads(true)`, with a comment
  that the fork defaults it but cang pins it "so the behaviour survives a fork re-base that
  drops that default: a guest's parallel block I/O otherwise stalls on one device thread".
- `deps/libkrun/src/devices/src/virtio/block/worker.rs:365-377` - the pool exists only when
  those flags are set.
- size: `resolve_pool_size(requested = None, available_parallelism)` (`worker.rs:767`) =
  `available_parallelism().clamp(2, 8)` -> **8 host threads** on the 26-vCPU guest, each
  `thread::Builder::name("block pool worker {i}")` (`worker.rs:155`) holding a clone of the
  same `disk.file` and each `DiskProperties::new(...)`.
- cang never calls `set_pool_size`, so the 8 is libkrun's clamp, not a cang choice.

So the host side of the lower *is* pooled and parallel. That is why the depth-8 collapse
measured earlier is not thread starvation: 8 readers over 8 pool threads still fell to 45-58
MB/s aggregate with iowait 41-68 s and CPU at 17-22 % - the cost is the per-request round trip
plus the staging copy, not a shortage of reading threads. The one-thread case I built as a
prototype (`CANG_BLOCK_SERIAL_READS`, `parallel_reads=false`) was 1.5x faster at one reader and
indistinguishable at eight, which is consistent: the pool costs a bounce-buffer copy per request
and buys overlap the guest cannot use at that concurrency.

## virtiofs (`lower:virtiofs`, and any host-composed `/nix`)

- `deps/libkrun/src/devices/src/virtio/fs/worker.rs:40` - `DEFAULT_WORKER_THREADS = 4`, i.e. a
  4-thread `FsWorker` pool plus a dispatcher thread per device
  (`fs/device.rs:104`; `set_worker_threads` at `:226` is never called by cang).
- The comment on the constant explains the sizing: "The measured case for a pool is
  READDIRPLUS, where a single guest thread already saturates the one server thread (note 20 in
  cang); the open/read/stat classes leave that thread at ~27% of a core, so they gain little
  and lose nothing. Every pool thread is a host thread with its own 1 MiB stack, so the default
  stays small."

That 27 %-of-a-core figure is the honest reason this path is slower for metadata: the FUSE
server is not CPU-bound, so more threads would not help either; what costs is the FUSE round
trip, which is why the guest-composed overlay over the block lower measured 432-479 ms against
virtiofs's 913-1 199 ms (note 27).

## What this means for the open questions

- A bigger block pool is untested and unlikely to help: the arms above show iowait and device
  service, not thread saturation, and the pool is already at its ceiling of 8.
- A bigger FUSE pool is explicitly argued against by the fork's own measurement.
- The remaining lever is therefore the request count and the copy, not the thread count - the
  same conclusion as notes 22-23 from a different direction.

## Evidence

Source citations above; measured arms in notes 09 (direct-to-guest vs pooled), 22 (erofs request
count), 23 (bulk 444 MB/s, device-bound) and 27 (the `/nix` A/B).
