---
label: wayfinder:research
title: Build the pool behind the single virtio-fs request queue - does it take READDIRPLUS out of the ceiling, and does a build move?
status: done
blocked_by: []
claimed_by: pi worker-pool child (2026-10-26)
---

# 21 - Worker pool behind the one virtio-fs queue: design, A/B, verdict

Note 20 left one action: the single `fs worker` saturates at **one** guest thread
for READDIRPLUS (70% of a core, 95% at 64) while the guest stays at 9-22%, so a
pool behind the request queue should be worth building **for that class only**.
This note is that implementation, its A/B, and the honest answer to whether it
should ship.

## Verdict

1. **The pool works on the class it was built for.** `readdirplus` at 16 guest
   threads went **154 -> 316 ops/s (+105%)** and at 64 threads **149 -> 325
   ops/s (+118%)**; scan p50 halved (131 ms -> 66 ms at 16 threads, 524 ms ->
   262 ms at 64), and scaling over one thread went **1.21x -> 2.35x** (16) and
   **1.17x -> 2.41x** (64). The dispatcher thread drops from 95.9/97.8% of a core
   to 4.6/3.9% while the four pool threads run at **~396%** - i.e. the four
   threads are now the ceiling, not the one.
2. **It costs nothing anywhere else** - measured, not assumed: open/read/stat at
   1, 16 and 64 threads and `statwarm` all land within noise of the before arm
   (op/s ratios 1.00-1.03x), and a single guest thread driving the queue keeps
   the pre-pool latency because a lone request is served on the dispatcher
   thread (see section 3.3 - without that rule the same binary measured **-19%**).
3. **A full rustc build does not measurably move.** Three runs each, in two
   orders: before 322.7 / 320.2 / 300.2 s, after 308.8 / 303.7 / 298.6 s (mean
   -3.4%, but the last before run is as fast as any after run, so this is host
   drift and not a win). What *does* change is who does the work: the dispatcher
   drops from 28-30% to 22-24% of a core on every run. The pool is engaged
   during a build; the thread it unloads was not the critical path of this
   workload, whose directories hold 1-2 crates each rather than 4096 entries.
4. **Ship it.** It removes a measured hard ceiling, is neutral everywhere else,
   is off entirely for a lone request, and its cost is bounded (default 4 host
   threads, `set_worker_threads(1)` restores the pre-pool behaviour exactly).
   The open question it does not answer is note 20's last one: **how much of a
   real build is READDIRPLUS** - count it before expecting a build-level win.

## 1. The design

virtio-fs has no multiqueue. `defs::NUM_QUEUES = 2` and `num_request_queues = 1`
(`fs/device.rs`), there is no `VIRTIO_F_MULTIQUEUE` for it, and the guest driver
maps one request queue however many vCPUs it has - so the only way to use more
than one host core is a pool behind that one queue, which is what upstream
virtiofsd's `process_queue_pool` does. The split:

| who | does |
|---|---|
| dispatcher (the thread that is still called `fs worker`) | owns both `Queue`s: pops the available ring (`next_avail` bookkeeping), calls `add_used`, decides `needs_notification`, signals the interrupt |
| pool threads (`fs work N`) | `Server::handle_message` (the FUSE op), including writing the reply into the chain's writable buffers, then hand back the length for the used entry |

- **A job is `{queue_index, head index, desc_table, queue_size}`, not a
  `DescriptorChain`.** A `DescriptorChain` - and the `Reader`/`Writer` built from
  one - borrows the `GuestMemoryMmap` (`queue.rs:193`, `queue.rs:345`), so it
  cannot cross a thread boundary. A pool thread rebuilds the chain from its own
  clone of the guest memory (an `Arc` bump) with
  `DescriptorChain::checked_new`. Ring bookkeeping stays on the dispatcher, which
  is the property that makes the used ring single-writer.
- **Out-of-order completion is fine here, and that is why this is safe.** The
  FUSE client matches a reply to its request by `fuse_in_header.unique`, and the
  driver returns the descriptor chain whose head index the used-ring entry names,
  so no order is implied between chains. This device only ever has split rings
  (no `VIRTIO_F_IN_ORDER`, no packed ring), where the spec imposes no ordering on
  the used ring either. One used-ring entry per processed chain is preserved
  exactly: every job gets exactly one `add_used`, on the dispatcher.
- **`Server`/`AugmentFs`/`PassthroughFs` are already shareable, not duplicated:**
  every handler takes `&self` and the mutable state is interior
  (`RwLock` inodes/handles, `AtomicU64 next_handle`, `AtomicBool writeback`),
  which is what lets one `Arc<FsServer>` serve 1 or 4 threads with the same
  locking it already did for one. Nothing needed a new lock; the DAX
  `VirtioShmRegion` is three integers and is copied per thread.
- **Backpressure**: bounded job channel of 4 per thread; the dispatcher blocking
  on it is the throttle. Completions are unbounded and consumed by the
  dispatcher, so a burst cannot deadlock the pair.
- **A panicking or failing handler cannot wedge the share.** Note 19's crash fix
  (wayfinder/fs-worker-robustness) is kept and extended: a malformed chain, a
  handler error or a handler *panic* fails exactly that request with a
  zero-length used entry (`catch_unwind` around `handle_message`), because in a
  pool a dead thread is a slice of the guest's root filesystem gone.

Public surface: `Fs::set_worker_threads(n)` (1 = serve inline, clamped to the
host's available parallelism, default 4), exported as
`krun_fs_device_set_worker_threads` and as `FsDevice::set_worker_threads` in the
Rust API. cang sets `CANG_FS_WORKER_THREADS = 4` explicitly in
`crates/cang-libkrun`, the same way it sets the block device's parallel reads, so
behaviour does not depend on a fork default surviving a re-base. **No CLI flag**:
a user has nothing to decide here.

## 2. The diff

Fork, branch `wayfinder/fs-worker-pool` (on top of `wayfinder/fs-worker-robustness`
= `057a2b9a`, which carries the crash fix), commit `179c6e00`:

| file | what |
|---|---|
| `src/devices/src/virtio/fs/worker.rs` | +384/-50: `FsPool`, `FsJob`/`FsCompletion`, `serve_jobs`, `serve_chain` (extracted, with the panic guard), dispatcher completion drain on a new eventfd, dispatch policy |
| `src/devices/src/virtio/fs/device.rs` | +25/-1: `worker_threads` field, `set_worker_threads`, plumbed into `FsWorker::new` |
| `src/libkrun/src/api/device_builders.rs` | +20: `FsDevice::set_worker_threads` (the FFI name is generated from it) |
| `bindings/.../ffier-krun.json` | +33: regenerated (`cargo build -p libkrun --features ffi,blk,net,gpu,input,timesync`; the only diff is the new symbol) |
| `include/libkrun.h` | +20: the new declaration + fn typedef, written in the generator's format (the `ffier-gen-c-header` binary is not in the devshell and cannot be built offline) |
| `CANG.md` | +13: the fork-only-work entry |

cang, commit `7cddff0`: `CANG_FS_WORKER_THREADS` + the launcher call +
`LibkrunApi::fs_device_set_worker_threads` (+ the recording fake's
`Call::FsDeviceSetWorkerThreads` and the launcher's exact call-order test). Tool:
`concurrency-scaling.sh` gains pool-thread sampling (`pool%`), see section 5.

## 3. The A/B

One VM at a time, one boot per arm, the same command both sides:
`concurrency-scaling.sh --threads "1 16 64" --rounds 3`, 4096 files x 4 KiB, guest
26 vCPUs (kernel `7.2.10-hardened1`), image `localhost/cang:latest`. Two binaries from the same
source tree, differing only in the fork checkout:
`cang-before` = `hbqy3wg2...` (pristine fork), `cang-after` = `j8bc1hq9...` (the
pool). The `shm` control arm moved by <3% between the runs below, which is what
licenses comparing absolute numbers (note 20 had a 1.35x spread between its two
runs).

Runs: before `/home/dev/cang-bench-pool/runs/concurrency-20261026-121241`, after
`.../concurrency-20261026-125555`.

### 3.1 The table

| arm | mode | thr | ops/s before | ops/s after | delta | p50 before | p50 after | fs_worker% before -> after | pool% after |
|---|---|---|---|---|---|---|---|---|---|
| virtiofs | open | 1 | 1971 | 1987 | +1% | 498 us | 493 us | 12.6 -> 12.9 | 0.1 |
| virtiofs | open | 16 | 6771 | 6844 | +1% | 4.1 ms | 4.1 ms | 23.8 -> 24.8 | 7.4 |
| virtiofs | open | 64 | 6980 | 7043 | +1% | 16.4 ms | 16.4 ms | 25.8 -> 26.7 | 15.7 |
| virtiofs | read | 1 | 1737 | 1791 | +3% | 576 us | 552 us | 12.5 -> 12.4 | 0.0 |
| virtiofs | read | 16 | 4922 | 4969 | +1% | 4.1 ms | 4.1 ms | 23.7 -> 24.5 | 6.8 |
| virtiofs | read | 64 | 5121 | 5158 | +1% | 16.4 ms | 16.4 ms | 25.6 -> 26.4 | 10.0 |
| virtiofs | stat | 1 | 6909 | 7076 | +2% | 123 us | 121 us | 11.2 -> 11.2 | 0.0 |
| virtiofs | stat | 16 | 19940 | 19754 | -1% | 756 us | 758 us | 20.9 -> 22.3 | 6.5 |
| virtiofs | stat | 64 | 20968 | 21103 | +1% | 4.1 ms | 4.1 ms | 22.4 -> 23.4 | 13.6 |
| virtiofs | statwarm | 1 | 437860 | 438657 | 0% | 2 us | 2 us | 2.1 -> 1.9 | 0.0 |
| virtiofs | statwarm | 64 | 7543514 | 7527441 | 0% | 3 us | 3 us | 1.1 -> 0.8 | 1.0 |
| virtiofs | **readdirplus** | **1** | 127 | 135 | **+6%** | 10.9 ms | 8.2 ms | 76.1 -> 75.2 | 0.0 |
| virtiofs | **readdirplus** | **16** | 154 | **316** | **+105%** | 131 ms | 66 ms | 95.9 -> 4.6 | **395.9** |
| virtiofs | **readdirplus** | **64** | 149 | **325** | **+118%** | 524 ms | 262 ms | 97.8 -> 3.9 | **395.8** |

Scaling over one thread (the number note 20's verdict was written on):

| mode | 16/1 before -> after | 64/1 before -> after |
|---|---|---|
| open | 3.44x -> 3.44x | 3.54x -> 3.54x |
| read | 2.83x -> 2.77x | 2.95x -> 2.88x |
| stat | 2.89x -> 2.79x | 3.03x -> 2.98x |
| **readdirplus** | **1.21x -> 2.35x** | **1.17x -> 2.41x** |

`pool%` is the pool threads' `schedstat` runtime summed, also as a percentage of
one core. At 16 and 64 threads it is ~396%, i.e. all four threads are busy for
the whole cell: with four threads the pool is now the ceiling of that class, and
`set_worker_threads(8)` would go further (not measured here; 4 is the shipped
default).

### 3.2 Why "flat 2.4x and not 4x"

The guest can feed more (busy vCPU is 31% of one core at 16 threads, `gcpu%` 38),
so the limit is the four pool threads plus the dispatcher, not the guest. Where
the 2.41x sits against the "~4-8x" guess in note 20 §2: the pool is 4 threads
wide, so 2.4x aggregate with a 4-thread server is the arithmetic, not a shortfall
of the design.

### 3.3 The dispatch policy is load-bearing (three measured variants)

The first implementation dispatched everything, and that cost the small-file
classes **-19% at one guest thread** (open 1971 -> 1594, read -20%, stat -24%)
because a lone request pays two scheduler wakeups (out to a worker, back with the
used-ring entry) that nothing overlaps. Three policies were measured on the same
harness (`readdirplus@16` / `open@1`):

| policy | readdirplus@16 | open@1 | note |
|---|---|---|---|
| pool always | 277 | **1594** | the -19% regression |
| inline when the ring is empty *after* the pop | 209 | 1986 | no regression, but it bounces the head of every burst onto the dispatcher |
| inline when `len() <= 1` before the pop (burst-aware) | 300 | **1597** | `Queue::len` counts the chain you are about to pop, so "lone" never fired |
| **inline when lone *and* the pool has nothing in flight** | **316** | **1987** | shipped: `run/concurrency-20261026-125555` |

So the pool is used whenever there is anything to overlap it with (another chain
already available, or a request still in the pool), and only a request on an
otherwise idle device stays on the dispatcher's stack. That is also why the
`open@1` numbers come back to the pre-pool value rather than merely close.

## 4. The full synthetic arm

`BENCH_ROOT=/home/dev/cang-bench-pool CANG=<binary> IMAGE=localhost/cang:latest
run.sh --arms guest-virtiofs --runs N`, 2401 crates, 26 jobs. Recorded baseline
in note 15: 298.9 / 306.5 / 311.4 s (and 326 s in note 20's run).

| order | before (s) | after (s) | delta |
|---|---|---|---|
| before x2, then after x2 (13:04-13:30) | 322.7, 320.2 | 308.8, 303.7 | -4.6% |
| after x1, then before x1 (13:36-13:50) | 300.2 | 298.6 | -0.5% (the after run ran first) |
| mean of all | 314.4 | 303.7 | -3.4%, **within host drift** |

The last before run (300.2 s, taken after the after runs) is as fast as any after
run, so the honest reading is **no build-level win**, not a 4% one. What is
consistent across every run is the *dispatcher's* share of a core: 29.9 / 28.2 /
28.0% before vs 23.6 / 22.5 / 23.4% after - about 6 percentage points of server
CPU moved into the pool on a real build. This is what note 20 §5 left open: the
pool removes the serialisation, but a build's FUSE traffic is dominated by
open/read/stat, where the queue was never the ceiling, and this workload's
directories (1-2 crates each) do not produce 4096-entry scans.

## 5. Tool change

`tools/rust-compile-bench/concurrency-scaling.sh` now samples the pool threads
(`fs work N`, summed ticks, `schedstat` runtime and count) into
`samples.txt` fields 10-12 and a `pool%` column, documented in the tool's README.
This is what separates "the work moved into the pool" from "the run was faster":
with the pool the dispatcher's `fs_worker%` falls by roughly what `pool%` gains.
A build without a pool reports 0 in that column, so one script prices both arms.

## 6. Gates

- fork `cargo build` (devshell): clean. `cargo build -p libkrun --features
  ffi,blk,net,gpu,input,timesync` (the FFI surface the release build uses): clean.
- fork `cargo fmt --check`: clean. `cargo clippy -p krun-devices --all-targets
  --features blk,net,gpu,input,timesync -- -D warnings`: clean for the touched
  files; the run still reports 3 **pre-existing** violations elsewhere
  (`console/process_rx.rs:55` collapsible-if, `gpu/virtio_gpu.rs:1256` items
  after a test module, `utils/udmabuf.rs:178` manual `is_multiple_of`) plus the
  known `--all-features` breakage (`virgl_resource_map2` wants a rutabaga with
  `resource_map`, which the pin does not have).
- cang workspace: `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test` all exit 0 (631 + 315 + 54 + 39 +
  5 + 4 tests), including the launcher's exact call-order test with the new
  `Call::FsDeviceSetWorkerThreads`.
- `nix build .#cang --override-input libkrun-src "git+file://$PWD/deps/libkrun"`
  builds the before (`hbqy3wg2`) and after (`j8bc1hq9`) binaries; the after binary
  contains `fs work ` and `krun_fs_device_set_worker_threads`, the before binary
  neither, and the before build's `cang-workspace-source` derivation points at
  the pristine fork (`6792qlb9...`), which is how the two were attributed.
- Live boot/exit status, one VM each: `cang -- sh -c 'cd /workspace && gcc -O2 -o
  hello hello.c && ./hello > hello.out && exit 7'` -> CLI exit **7** on both
  binaries, `hello.out` = `guest-build-ok` visible in the host cwd, 3 s and 4 s.

## 7. What this does not settle

- **How much of a real build is READDIRPLUS.** Still note 20 §5's measurement, and
  now the only thing between this change and a build-level win. Counting
  `FUSE_READDIRPLUS` requests in a `guest-virtiofs` arm is cheap.
- **The competing lever for the same class**: the 2.5 us per directory entry is a
  full `openat(O_PATH)+statx` per entry (`linux/passthrough.rs`); make that
  cheaper and the pool's win shrinks with it.
- **4 vs 8 threads.** `pool%` ~396% at 64 guest threads says the pool, not the
  guest, is the next ceiling. A `set_worker_threads(8)` arm is a one-line build.
- The single-queue round trip itself (note 20 §5: 23 ms of latency at 64 threads
  with neither side's CPU explaining it) is untouched; the pool does not shorten
  a round trip, it removes a serial server in front of it.

## Sources

| what | where |
|---|---|
| the pool | fork `wayfinder/fs-worker-pool` @ `179c6e00` (`worker.rs`, `device.rs`, `device_builders.rs`, ffier json, header, `CANG.md`) |
| cang wiring | cang `wayfinder/workspace-virtiofs-perf` @ `7cddff0` (`CANG_FS_WORKER_THREADS = 4`, `fs_device_set_worker_threads`, launcher, tests) |
| concurrency A/B | `/home/dev/cang-bench-pool/runs/concurrency-20261026-121241` (before), `.../125555` (after), plus the three policy variants `.../122055`, `.../123008`, `.../123846`, `.../124710` |
| full arm | `/home/dev/cang-bench-pool/logs/arm-full-{before,after,before-bis,after-bis}.log`, `summary-synthetic.tsv` |
| boot/exit status | `/home/dev/cang-bench-pool/logs/status-check.log` |
| gates | `/home/dev/cang-bench-pool/logs/{gates,clippy-devices2,clippy-devices5,build-before,build-after7}.log` |
| the decision this implements | note 20 sections 2 and 4 |
| the crash fix this keeps | `deps/libkrun` commit `057a2b9a` |
