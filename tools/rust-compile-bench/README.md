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

### cang workload (this repo, 1356 files, heavy per-unit compile)

| arm | build |
|---|---|
| host | 55.9 s |

The guest arms are blocked: the image's `/nix` closure ships clang, libclang,
gcc, rustc, cargo, pkg-config, mesa-libgbm and libdrm but **no `virglrenderer`**,
which `deps/rutabaga_gfx/build.rs` requires, and a `cang -v` of a host store path
does not help because the guest's store is a *different* store (same package
versions, different hashes). Unblocking it is an image decision
(`docs/wayfinder/workspace-virtiofs-performance/tickets/11-image-ships-cang-build-inputs.md`).
