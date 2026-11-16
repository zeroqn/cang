---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# A real compile does not care about the block I/O mode; `cang exec` under a build stalls for
# seconds, and that tail is the process-spawn storm

Asked to settle pool / O_DIRECT / "two readers" against the actual workload - a Rust build of
`sandsea` in the guest - and to measure vsock meanwhile. The answer is that the block-I/O knobs
**do not move a compile**, and that the exec path degrades far more than the isolated CPU test
suggested.

## 1. The workload, and what it costs

`sandsea-src-20261105-1537.tar.gz` is 1.7 MB but expands to a **24-crate workspace with 1 053
locked dependencies** and no vendored directory. The guest toolchain is
`/nix/store/x1caqzs9…-cang-rust-toolchain-layer` (cargo/rustc 1.99.1) - the bare
`rustc-stable-…` store path in the image has **no `std`** (`lib/rustlib/x86_64-…/lib` is empty),
so a build pinned to it dies with `can't find crate for 'core'`; four attempts were lost to that
before the toolchain layer was found.

The registry was fetched once on the host (`cargo fetch --locked`, 1.5 GB) and seeded onto the
build disk, so every arm built `--offline` from a clean target directory.

Full workspace: `cargo build --release --workspace --jobs 26` on the default mode ran **past
22 minutes** without finishing (polars), and was killed - recorded because it shows the build's
real scale.

Bounded workload for the mode A/B: `cargo build --release -p sandsea-collector --jobs 26`.

## 2. Mode A/B: position in the round dominates, the knob does not

| round | arm | mode | build |
|---|---|---|---|
| 1 | 1st | pool (default) | **553 s** |
| 1 | 2nd | serial | 246 s |
| 1 | 3rd | pool + O_DIRECT | 236 s |
| 1 | 4th | serial + O_DIRECT | 266 s |
| 2 | 1st | serial + O_DIRECT | **681 s** |
| 2 | 2nd | pool + O_DIRECT | > 18 min, aborted (host pressure) |

The **first arm of each round is 2-3x slower than the rest**, whichever mode it is: 553 s and
681 s for two different modes as first-of-round, against 236-266 s for everything after. The
three warm arms of round 1 span **236-266 s (13 %)**, which is the noise floor of this host - so
pool vs serial vs O_DIRECT is **not distinguishable** in a compile. Round 2 was abandoned when
its second arm hit host memory pressure (the worker held 5.9 GB, `MemAvailable` fell to 9.2 GB,
and the same build that took 4 minutes took 18+), which is the note-13 trap reappearing from an
accumulation of per-mode target directories.

Mechanism, consistent with the read benchmarks: a compile of ~100 crates reads a few hundred MB
of source *sequentially* and spends its time in rustc's CPU and in writing target files. The
per-request copy that the IO pool costs, and the cache bypass O_DIRECT buys, are proportions of
an I/O phase that is under a tenth of the build.

## 3. vsock under load: CPU saturation costs 80 ms, a build costs seconds

Host-side `cang exec <task> /bin/true` sampled once a second during each arm:

| condition | p50 | p95 | max |
|---|---|---|---|
| idle (all boots) | 20-27 ms | 22-47 ms | 51 ms |
| 26 spin loops on 26 vCPUs | 52.6 ms | 79.1 ms | 99 ms |
| **full-workspace build** | 23-69 ms | **14.7 s** | **125.9 s** |
| `-p sandsea-collector` build | 31-57 ms | 2.6-14.5 s | 11-59 s |

Saturating every vCPU with spin loops makes the round trip 2.2x at p50 and 2.8x at p95 - but a
real build makes the **p95 minutes-adjacent (seconds to two minutes)** while its p50 stays near
35-57 ms. The difference between the two loads is the build's process-spawn storm: thousands of
rustc/cc invocations, each a fork+exec the guest has to schedule, versus a fixed 26 busy threads.
So `cang exec`/`attach` during a build should be expected to hitch for seconds, not tens of ms.

## 4. What this means for the configuration question

- **Do not disable the IO pool for a build.** The read benchmarks (note 29) show it costs 15-30 %
  at 1-4 concurrent readers, but a compile does not present that shape, and here it measured
  inside the noise.
- **Do not enable host O_DIRECT.** It does not help a compile and it loses at high read depth.
- **"Two readers" is not a knob for a build**: cargo parallelises itself; the guest block queue
  sees whatever rustc's file I/O happens to generate.
- The read-path lever from note 29 stands for *read-heavy* work (`nix build`'s store reads, exec
  of toolchain paths), not for compilation.

## 5. Caveats

- One host, one guest size (8 GiB / 26 vCPUs); the mode arms are 1-2 samples each after the
  order effect is removed.
- The registry lived on the build disk, not the `/nix` lower, so the measured I/O is
  build-disk-dominant; a build whose dependencies came from `/nix` would exercise the lower more.
- Round 2 is incomplete by intent: the second arm degraded 4x under host pressure and was killed
  rather than reported as a mode difference.

## Evidence

`blkread/run-build-s{0,1}d{0,1}-*` (per-arm `build.log`, `phases.txt` with `seed_ms`/build ms,
`logs/exec-rtt.txt` with window-labelled round trips), `blkread/buildws/evidence/` (the shared
workspace's last arm), `blkread/buildws-modes{,2,3}.log` (the round logs, including the aborted
arm). Probe `probe-build.sh`, runner `run-build.sh`. Two harness defects fixed on the way:
`cang exec` needs the *worker* killed to free RAM (`libkrun-vm-worker-enter`, invisible to a
`bin/cang --mem` grep), and a stale `active-task` record in a reused workspace makes every later
launch refuse with "already has a running cang task".
