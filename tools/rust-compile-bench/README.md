# rust-compile-bench

A repeatable measurement of **how much cang's virtio-fs `/workspace` costs a
rustc build**, in three arms that separate the two penalties:

| arm | where the tree and `target/` live | what it measures |
|---|---|---|
| `host` | a host directory, no VM | the floor: no VM, no FUSE, no share |
| `guest-virtiofs` | `/workspace` inside the guest, i.e. the single virtio-fs root share | VM + virtio-fs |
| `guest-local` | copied to the guest's `/dev/shm` (tmpfs, RAM-backed), built there | VM only, no FUSE |

- `guest-virtiofs / guest-local` is the **virtio-fs penalty** - the number the
  virtio-fs work has to move.
- `host / guest-local` is the **VM penalty** - CPU and scheduling, not IO, and no
  virtio-fs change can touch it.

`guest-local` is RAM-backed on purpose: a plain cang guest attaches no
containers disk (`/dev/vdb` is absent unless podman has run, and
`/home/dev/.local/share/containers` is not writable in a plain run), and a tmpfs
is the strictest form of "the same VM without the FUSE share". It is an upper
bound on guest IO performance, not a disk-equivalent number; a real disk would sit
between the two.

## Workloads

`--workload synthetic` (default) - `gen-workload.sh` generates a workspace of
**2401 crates** (1800 leaves, 450 mid1, 150 mid2, one bin, three dependency
levels), **16801 `.rs` files**, 151 MB, deterministic and with **no external
crates**, so `cargo build --offline` never touches the network. Its shape is
chosen for rustc's file mix rather than for interesting code: thousands of small
sources read once, and per crate an `rlib` + `rmeta` + `.d` + a three-file
fingerprint dir (~7200 files in `target/debug/deps`). This arm weights the
**metadata/stat/lookup traffic** that a single-threaded FUSE server is most
suspected of throttling.

`--workload cang` - a copy of this repository at HEAD (submodule trees included,
`target`/`.git` excluded: 119 MB, 1356 files) built with
`cargo build --offline -p cang`: the real crate graph, our own code, bindgen,
proc macros and the final link. On this host that is 84 s. This arm weights
**heavy per-unit compile work**, and it needs build-time paths the image does not
ship - `cang-workload-env.sh` resolves them from the `/nix` mount the guest
already has (`PKG_CONFIG_PATH` for virglrenderer/libgbm/libdrm, `LIBCLANG_PATH`,
`KRUN_INIT_BINARY_PATH`, `-L` for the linker) by glob, never by a pinned store
hash, and fails loudly naming the missing piece.

Both guest arms and the host arm build the identical tree with the same
toolchain (`RUSTC_WRAPPER=` so sccache is off, `CARGO_INCREMENTAL=0`,
`CARGO_BUILD_JOBS=26` = a cang guest's vcpu count on a 28-CPU host, cold
`target/`). Each run's timer covers only the build: it starts after the tree is
in place and stops when the build reports, so VM boot, rootfs materialization and
the `guest-local` copy are excluded.

## Running it

```bash
nix build .#cang                     # or point CANG at any built cang
tools/rust-compile-bench/run.sh                       # 3 arms, 2 runs, synthetic
tools/rust-compile-bench/run.sh --workload cang --arms guest-virtiofs --runs 3
tools/rust-compile-bench/gen-workload.sh /tmp/w       # workload only (LEAVES/MODULES/LINES)
```

Artifacts land in `$BENCH_ROOT` (default `/home/dev/cang-bench`):
`logs/<arm>-<n>.log` per run, `logs/threads-<arm>-<n>.txt` (per-thread CPU capture
of the VMM during a guest arm) and `summary-<workload>.tsv` (appended, never
rewritten).

```bash
tools/rust-compile-bench/run.sh --report      # the ratio table, from the summary
```

Run guest arms one at a time on an otherwise idle host: two live VMs, or a
concurrent GPU job, distort the numbers more than any tuning under test. A run
taken while another session was busy is a different measurement - say so when
you record it.

## Reading a number, not just taking one

Each guest arm samples the VMM's threads at 1 Hz (`mon-threads.sh`), and the
summary carries the mean CPU of the thread that serves the share:

```bash
tools/rust-compile-bench/summarise-threads.sh "$BENCH_ROOT/logs/threads-guest-virtiofs-1.txt"
#  12.3%  fs_worker              (span 315s)
#  65.1%  fc_vcpu_0              (span 323s)
```

`fs_worker` is the **only** thread serving the FUSE share, so its utilisation
decides whether a faster *server* can help at all: at 12% of a core it cannot -
the cost is per-request round-trip latency, and only fewer round trips (caching,
which cang rules out) or a cheaper op path would move it. That is how the worker
pool in `docs/wayfinder/workspace-virtiofs-performance/tickets/04-*` was rejected
on measurement rather than taste.

## Two cang behaviours this harness has to work around

Both are defects, not harness preferences, and both are why earlier attempts
produced "silent" failures:

1. **A guest command's stdout is not forwarded, and its exit status is not
   propagated.** `cang -- echo hi` prints nothing and exits 0; `cang -- false`
   exits 0. So every arm writes `bench-result-<tag>.txt` into the shared
   workspace (the tag is unique per run, so a stale file can never pass for a
   fresh result) and the harness reads `BENCH_MS` from there.
2. **A launch into a workspace that already has a running task does nothing at
   all** - exit 0, no output. An interrupted run therefore leaves a VM behind
   (`cang ps`), and every later run in that workspace silently measures nothing.
   The harness refuses to start a guest arm while a task is running there, always
   kills the VM it started, and, when a VM dies without a result, appends the
   task's `helper.stderr.log` tail to the arm's log - which is where a virtio-fs
   death (`virtio_fs_enqueue_req() failed -5`, then `Vmm is stopping`, exit code
   0) is visible.

## Baseline

Measured 2026-10-25/26 on `7.2.11-xanmod1`, 28 CPUs, 24 GiB RAM, cang 0.14.0,
image `ghcr.io/zeroqn/cang:latest` (rustc/cargo 1.95.0), 26 vCPUs in the guest,
cold `target/`, `CARGO_BUILD_JOBS=26`, sccache off, incremental off.

> **Window caveat.** These runs were taken while another agent session may have
> been running its own live smoke on the same host, so treat them as ±, not as
> canonical. The *ratios* are what this tool reports and they are stable across
> runs; a clean-window re-record is pending (wayfinder ticket 12). One number is
> contention-safe in the conservative direction: the FUSE worker thread's CPU
> share can only be *inflated* by a co-tenant on the host.

### synthetic workload (2401 crates, 16801 sources, 151 MB)

| arm | runs (s) | vs host | vs guest-local |
|---|---|---|---|
| host | 104.2, 104.7 | 1.00x | - |
| guest-local (`/dev/shm`, no FUSE) | 232.0, 232.9, 235.1 | 2.2x | 1.00x |
| guest-virtiofs (`/workspace`) | 298.9, 306.5, 311.4, 314.4 | 2.9x | **1.29-1.34x** |

Run-to-run spread: host ~2%, guest-local ~1%, guest-virtiofs ~4%. A change must
beat ~5% on the guest-virtiofs arm to mean anything.

Low-concurrency check, `JOBS=4`: guest-virtiofs **683.9 s**. The penalty is not a
high-concurrency artefact - it is present, and larger, when the build leans
harder on per-operation latency.

### Where the time goes (the number that decided the fork work)

Per-thread CPU of the VMM during a full `JOBS=26` guest-virtiofs arm (314 s,
1 Hz sampling, `docs/wayfinder/workspace-virtiofs-performance/notes/`):

- the single **`fs_worker`** thread (the only thread serving the share, one
  request queue): **12% of one core** on average;
- the guest's own vCPUs: ~65% on the busiest and ~22% on each of the rest,
  i.e. the guest is doing the work and waiting.

So the share's cost is **per-request round-trip latency, not worker
serialization** - a worker pool over that single queue would find an 88%-idle
thread. What would actually cut it is fewer round trips (guest caching, which
cang now rules out because it would break host-immediate visibility) or a
cheaper op path (FUSE passthrough / io_uring, unavailable to a VMM-hosted
server).

### cang workload (this repo at HEAD, 1356 files, heavy per-unit compile)

| arm | build | notes |
|---|---|---|
| host | 55.9 s | `nix develop` toolchain, `JOBS=26`, offline |
| guest-virtiofs | **285.3 s** | `JOBS=8`, warm registry, `/workspace` |
| guest-local | not run | `/dev/shm` cannot hold the tree plus `target/` comfortably |

Two inputs a cang build needs are **not in the image's `/nix`** (which is the
*image's* store, 676 entries - the host's store is not visible in a guest), so
`run.sh` stages them into `$BENCH_ROOT/toolchain` and mounts that read-only at
`/cang-toolchain`, where `cang-workload-env.sh` resolves them:

- `virglrenderer` - `deps/rutabaga_gfx/build.rs` probes `virglrenderer.pc`;
- a prebuilt static `krun-init` - `deps/libkrun/init/init-blob/build.rs` needs
  one unless the host has a musl target to build its own.

Dependencies come from a **warmed registry, not a vendor dir and not `--offline`
in the timed path**: a cang guest mounts an *empty* per-workspace dir at
`/home/dev/.cargo`, so `run.sh` seeds that state dir from the host's `~/.cargo`
once (412 MB, reused). Without it a guest build cannot resolve even `clap`, which
is what first looked like an offline-mode problem and was really a cold cargo home.

`JOBS=8` for the guest is a memory decision, not a benchmark one: cang sizes a
guest at 80% of host RAM (~19 GiB here) while the host has ~3 GiB genuinely free,
and `JOBS=26` died mid-build (VM gone, no result). Record the guest's `JOBS`
alongside the number.


## Does the share scale with concurrency? (`concurrency-scaling.sh`)

`run.sh` prices a whole build; this one prices the *server*. It answers whether
the single virtio-fs request queue and its single `fs worker` thread serialise
the guest's FUSE traffic, or whether the cost is per-request latency the guest
pays no matter how many server threads exist.

One guest boot, two filesystems inside it, alternating rounds so host drift hits
both arms:

| arm | where | what it is |
|---|---|---|
| `virtiofs` | `/workspace/tree` | the single root share: 1 request queue, 1 `fs worker` thread |
| `shm` | `/dev/shm/tree` | the same tree on guest tmpfs: the no-FUSE control |

Thread counts 1/4/16/32/64 run a small C workload compiled *in the guest*
(`gcc -O2 -pthread`, executed from `/tmp` because `/dev/shm` is `noexec`), and
while it runs the host samples the VMM's `fs worker` at 4 Hz: state, wchan,
utime+stime, `schedstat` runtime and runqueue wait, voluntary context switches.

```bash
tools/rust-compile-bench/concurrency-scaling.sh                 # 4096 files, 3 rounds
tools/rust-compile-bench/concurrency-scaling.sh --modes read --threads "1 16" --rounds 2
tools/rust-compile-bench/concurrency-scaling.sh --help
```

Modes, and why there are five of them:

| mode | op per iteration | FUSE traffic |
|---|---|---|
| `open` | `openat(O_RDONLY)` + `close` | `FUSE_OPEN` + `FUSE_RELEASE` per op |
| `read` | open + `fadvise(DONTNEED)` + `read` + close | adds a real `FUSE_READ` |
| `stat` | `statx(AT_STATX_FORCE_SYNC)` | forces a `FUSE_GETATTR` |
| `statwarm` | `statx(flags=0)` | guest attribute cache only - the **control** |
| `readdirplus` | `opendir`/full `readdir`/`closedir` | `FUSE_READDIRPLUS` batches |

The share is created with `attr_timeout = entry_timeout = 5 s`, so a repeated
`statx`/read of a small file set is served by the guest caches and never reaches
the server: `statwarm` shows that arm (millions of ops/s) and is why `stat` and
`read` defeat the caches explicitly. A concurrency table built without that
distinction measures the guest VFS and nothing else.

Output: the table (ops/s, `ops/s(T)/ops/s(1)`, p50/p99, guest CPU burned, and
for the server thread its runtime%, `R/S/D` sample split and the share of
samples parked in `epoll_wait`), plus the raw per-cell and sampler files:

```
fs       mode    threads  cells   ops/s  ratio  p50us  p99us benchcpu% fs_worker%  R/S/D %  epoll% vcpucpu%
```

Read it the way note 20 of `docs/wayfinder/workspace-virtiofs-performance/` does:
a flat curve with `fs_worker` at ~100% is server serialisation (a pool helps);
a flat curve with the worker below ~30% is the guest / latency (nothing
server-side helps); a rising curve with the guest's vCPUs saturating is the
guest's own cost.

Two instrumentation traps this script encodes, both of which have produced wrong
numbers in this repo before: `/proc/<tid>/stat` must be parsed from after the
LAST `)` (the comm is literally `fs worker`, so a naive field index sums the
wrong pair and silently drops `stime`), and `/proc/<tid>/wchan` has no trailing
newline, so `read -r x < .../wchan || x=0` discards every value and reports 0
for a sleeping thread. `/proc/<tid>/syscall` is not readable for the VMM at all
(it is non-dumpable), which is why the sampler uses `wchan`.

What it found (2026-10-26, the probe behind note 20 of the map): the
`open`/`read`/`stat` mix plateaus at **3.2-4.7x** between 16 and 64 guest threads
while the `fs worker` stays at **12-27% of one core** - it is waiting, not
computing, and its serialised service is 96-162 us against a 1.2-23 ms round
trip, so a pool or io_uring buys essentially nothing there. `readdirplus` is the
exception: the same thread is at **70% of a core with one guest thread** and 95%
at 64, throughput is flat (1.3-1.5x) and p50 latency grows **48x** - that is the
one op class a pool would fix, and whether it is worth fixing depends on the
class's share of a build, which this probe does not measure.
