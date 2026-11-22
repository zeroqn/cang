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
5. **Two claims from a first pass are retracted** (its own probes ran around its
   arms, inflating the guest rows 1.4-2.6x): the 435.8 / 857-959 s figures and the
   "1.62x from `7.3.5-hardened1`" conclusion. The retracted rows stay in
   `summary-synthetic.tsv`, labelled by run order, and are not baseline figures.
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
