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
`logs/<arm>-<n>.log` per run and `summary-<workload>.tsv` (appended, never
rewritten). Run guest arms one at a time on an otherwise idle host: two live VMs,
or a concurrent GPU job, distort the numbers more than any tuning under test.

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

<!-- FILLED IN BY THE FIRST RECORDED RUN, per workload, once guest arms pass -->
