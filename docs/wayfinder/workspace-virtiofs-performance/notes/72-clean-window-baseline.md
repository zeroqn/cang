# Ticket 12: clean-window baseline - the host arm and both guest arms re-measured, and two inflated claims retracted

2026-11-22. Ticket 12. Artifacts: `cang` at `/home/dev/cang/cang/result` (a GC-rooted
`-o result` build at HEAD `ee7744bc`, `cang 0.22.0`), image `localhost/cang:latest`,
`BENCH_ROOT=/home/dev/disp/cang-bench`, `WORKLOAD=synthetic` (2401 crates, 16801
sources, 151 MB), `JOBS=26`, cold `target/` in every run that is counted (each inner
run log shows **2401 `Compiling`, 0 `Fresh`**).

## The window

| fact | value |
| --- | --- |
| `cang ps` / `pgrep libkrun-vm-worker` / fuse mounts | none |
| `nproc` / `MemTotal` / `uname -r` | 28 / **17.6 GiB** / `7.3.4-xanmod1` |
| the host itself | a **nested KVM guest** (`hypervisor`, `svm`), Ryzen 9 7945HX |
| rustc / cargo | 1.99.1 (the 2026-10-25/26 record was 1.95.0) |
| guest | `vcpus=26`, `ram_mib=13312` (13 GiB), guest kernel from the libkrunfw pin |

Two `D`-state `relay-pump` processes (1116884, 1126187) from a rejected experiment
are still present (each 0.0 % CPU, unkillable without a reboot); they are not part of
any measurement below.

## The numbers

| arm | runs (s) | mean | vs host | vs guest-local |
| --- | --- | --- | --- | --- |
| `host` | 90.761, 90.109, 90.549 | **90.47** (spread 0.7 %) | 1.00x | - |
| `guest-local` (`/dev/shm`) | 311.264 | **311.3** | 3.44x | 1.00x |
| `guest-virtiofs` (`/workspace`) | 372.645 | **372.6** | 4.12x | **1.20x** |
| `guest-local`, firmware `cang.4` (guest 7.2.10) | 315.719 | 315.7 | 3.49x | - |

The guest rows are single runs: the guest arms are minutes long and the windows that
produced them were re-checked clean before each. `fs_worker` came back **0.8 %** and
**1.5 %** on the two `guest-local` runs and **7.0 %** on `guest-virtiofs`.

Cross-window comparison, with the recorded 2026-10-25/26 figures:

| arm | 2026-10-25/26 | 2026-11-22 | change |
| --- | --- | --- | --- |
| `host` | 104.2, 104.7 | 90.5 | **13 % faster** (toolchain 1.95 -> 1.99, same box) |
| `guest-local` | 232.0, 232.9, 235.1 | 311.3 | **34 % slower** |
| `guest-virtiofs` | 298.9-314.4 | 372.6 | **19-25 % slower** |
| virtio-fs penalty (`virtiofs/local`) | 1.29-1.34x | **1.20x** | **narrowed** |

So the *penalty the map is about* narrowed, while both guest arms got slower in
absolute seconds. The guest slowdown is **unattributed**, with two facts that bound
it: the host arm got faster on the same box, and a firmware A/B (same image, same
CLI, same window, `CANG_LIBKRUNFW_LIBRARY` at `libkrunfw-v5.6.2-cang.4`, guest
`uname -r` 7.2.10-hardened1, confirmed in an in-guest probe) measured **311.3 s
against 315.7 s** - **flat**, 1.4 %, inside noise. The most likely remaining
candidates are the host's RAM (24 GiB at the recorded window, **17.6 GiB** now, so
the guest gets 13 GiB) and the nested-virtualisation steal that only appears while a
26-vCPU guest runs.

## Two claims from the first pass of this window retracted

A first pass at this ticket reported `guest-local` 435.8 s, `guest-virtiofs` 959.1 /
856.6 s, and concluded that **1.62x of the gap was today's `7.3.5-hardened1` guest
kernel**. Neither survives re-measurement:

1. **The guest numbers were contended - by the same session's own probes.** The pass
   ran `concurrency-scaling.sh`, a `mon-threads.sh` capture and a host `/proc/stat`
   build probe around and between its arms; its own note 72 first draft cites those
   probes as concurrent. Re-measured with nothing else running: 311.3 s and 372.6 s,
   i.e. 1.4x and 2.3-2.6x faster than that pass reported. The retracted rows are
   kept in `summary-synthetic.tsv` (they are real runs, labelled by run order) but
   they are **not** baseline figures.
2. **The firmware A/B is flat** (311.3 vs 315.7 s), so "1.62x from the new guest
   kernel" is withdrawn. The pin is not the cause of today's guest slowdown on this
   arm.

## What the first pass did establish, and is kept

- the host is a **nested KVM guest**, and physical `steal` runs 23-46 % in 10 s
  windows *only while a 26-vCPU guest is alive* (0.16 % cumulative since boot), which
  is a confound no cang-level "is the window clean" test can see;
- the share is **faster than recorded** per request: `concurrency-scaling.sh`
  `read` at one thread **4336 ops/s, p50 215.5 us** against the recorded probe's
  1528 ops/s / 1189 us (5.5x per op, 2.8x ops/s), and 12424 ops/s at 16 threads;
- with `CANG_VIRTIOFS_STATS=1` the server's own opcode histogram shows the
  zero-message-open lever active (`OPEN 3`, `RELEASE 1` at the 5000-request
  milestone) with `LOOKUP` 50.3 % and `READ` 36.7 % of the stream;
- the `fs_worker` at 3-7 % while a build crawls is a **starved** worker, not an idle
  one, so it does not revise the 12 % -> 26 % figure ticket 04 was decided on;
- **a harness defect, now fixed**: the devshell exports `CARGO_TARGET_DIR` (one per
  checkout, outside the tree), so `run.sh`'s `rm -rf target` deleted nothing the
  build used and every run after the first measured a **warm** target
  (`Finished dev profile in 1.07s`, zero `Compiling` lines). `run.sh`'s payload now
  exports `CARGO_TARGET_DIR="$PWD/target"` itself and prints it; verified by running
  the `host` arm twice with no external export: **2401 `Compiling` / 0 `Fresh`** in
  both runs (`target=/home/dev/disp/cang-bench/host/target`), 104.8 s then 91.6 s
  (the first cold page cache).

## What is not claimed

- one run per guest arm is not a spread; a change must still beat ~5 % on the
  guest-virtiofs arm, which is the rule the README states and the reason the guest
  rows are marked single-run;
- the guest slowdown against the recorded window is unexplained, not diagnosed;
- nothing here re-measures the 8.2 s compile-test iteration or the 45 s cold-build
  penalty the map's Destination cites - those come from other notes and were not
  re-taken.
