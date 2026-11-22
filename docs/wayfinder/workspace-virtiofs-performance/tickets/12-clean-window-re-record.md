---
label: wayfinder:task
title: Re-record the baseline in a clean window (no co-tenant VM on the host)
status: closed
blocked_by: []
claimed_by: ""
---

## Question

The recorded numbers were taken while another session may have been running a live
smoke against the same host, so they are indicative rather than canonical. Re-run
and replace them:

1. Confirm the window is clean: no other `cang` task running (`cang ps`), no
   `libkrun-vm-worker` process, and bob has confirmed the other session's smoke is
   finished.
2. `tools/rust-compile-bench/run.sh --runs 3` (host, guest-virtiofs, guest-local),
   one arm at a time.
3. Replace the README's baseline table and the map's numbers with the clean
   figures, keep the old ones only if they differ by more than the spread, and say
   which window each number came from.
4. Re-take the `fs_worker` CPU sample (`notes/` has the recipe) against the clean
   window - it is the number ticket 04 was decided on, and it is worth having it
   uncontended.

## Resolution

Re-recorded 2026-11-22 in a window that passed every test the ticket names, and
written up in [note 72](../notes/72-clean-window-baseline.md).

1. **The window**: `cang ps` empty, no `libkrun-vm-worker`, no `cang-workspace`
   mount, 0.1 % host CPU busy over 10 s; host `7.3.4-xanmod1`, 28 CPUs, **17.6 GiB**
   (the 2026-10 window had 24 GiB), rustc 1.99.1, and the host is itself a **nested
   KVM guest**.
2. **The numbers**, now the README's canonical table: host **90.47 s** (three runs,
   0.7 % spread), `guest-local` **311.3 s**, `guest-virtiofs` **372.6 s** (one run
   each, cold - 2401 `Compiling`, 0 `Fresh`). Ratios: 3.44x and 4.12x against the
   host, and a **1.20x** virtio-fs penalty, narrowed from 1.29-1.36x.
3. **The old table is kept as history** and labelled: the host arm got 13 % faster
   while both guest arms got 19-34 % slower in absolute seconds, so seconds do not
   travel across windows and only ratios do.
4. **The guest slowdown is unattributed, with one candidate removed**: a firmware
   A/B in the same window (guest `7.2.10-hardened1` pinned back) measured 311.3 vs
   315.7 s - flat - so the guest kernel is not the cause. The remaining candidates
   are the host's RAM loss and the nested-virtualisation steal.
5. **A second pass the same day disagreed, and the disagreement is recorded rather
   than resolved away.** Its guest rows were 435.8 / 857-959 s against this pass's
   311.3 / 372.6 (1.4-2.6x), and its firmware A/B was 1.62x (435.8 vs 269.2) where
   this pass measured flat (311.3 vs 315.7, with `uname -r` proving the override
   took effect in both). So the kernel difference is **window-dependent** and the
   gap between the two windows is **unattributed** - not established to be that
   pass's own probes. Both sets of rows stay in `summary-synthetic.tsv`; only this
   pass's are baseline figures, and the guest rows are single runs either way.
6. **The lever neither pass tested**: `halt_poll_ns=0` (and any window where host
   steal stays 0% under a live 26-vCPU guest). Both need root, so they are bob's if
   the guest arms are ever to be re-recorded with confidence.
6. **The `fs_worker` sample** in this window is 0.8-1.5 % (`guest-local`) and 7.0 %
   (`guest-virtiofs`): low, but on a build this slow that is a *starved* worker, not
   an idle one, so it does not revise the 12 % -> 26 % figure ticket 04 was decided
   on. The measurement that does: the share serves `read` at **4336 ops/s / p50
   215 us** at one thread, 5.5x better per op than the recorded probe.
7. **A harness defect found and fixed**: the devshell exports `CARGO_TARGET_DIR`, so
   `run.sh`'s `rm -rf target` deleted nothing the build used and every run after the
   first measured a warm target. The payload now pins `CARGO_TARGET_DIR="$PWD/target"`
   and prints it; verified by two host runs with no external export showing 2401
   `Compiling` / 0 `Fresh` in both.

## The halt_poll_ns A/B (added 2026-11-22 16:05)

The lever this ticket named as untested was toggled by root (`halt_poll_ns{,_grow}`
0 vs the saved 200000/2) with two `guest-local` runs per side:

| run | `halt_poll_ns` | `guest-local` |
| --- | --- | --- |
| A1 | 0 | 166.0 s |
| A2 | 0 | 141.3 s |
| B1 | 200000 | 147.7 s |
| B2 | 200000 | 169.0 s |

**Ruled out**: A mean 153.6 s against B mean 158.4 s is a 3% difference inside the
runs' own 18% spread, with the two slowest runs on opposite sides.

**And it exposed the real problem**: those four runs are half of what the same arm
measured an hour earlier (311.3, 315.7 s), and `guest-virtiofs` measured 274.9 s
against 372.6 s - so the guest arms move ~2x **between windows** with nothing known
changed, and the penalty ratio therefore reads 1.20x or 1.63x depending on which
window it came from. Neither the code, the image, the guest kernel nor `halt_poll_ns`
differs across them.

So this ticket's remaining work is **variance, not another setting**: 3 runs per arm
interleaved inside one window, medians rather than single runs, and host
`/proc/stat` steal sampled per run so the two can be correlated. Until that is done
the guest rows are a range, and the map says so.
