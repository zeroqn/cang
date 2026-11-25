---
label: wayfinder:research
title: What bounds the workload's CPU - job counts, a cgroup quota, or affinity?
status: closed
blocked_by: []
claimed_by: "pi arm-04 (child, agent, 2026-11-25)"
---

## Question

Bob's rule: build parallelism must leave at least 2 cores free of rustc (`nproc`
2 -> 1 free, `nproc` 1 -> none), and he prefers a bound that covers *any*
workload (rustc, nix, make) over a cargo-only environment variable - **if** such
a bound really works in this guest. Decide that, with a live probe.

Compare at least these, on the same storm:

1. **Environment job counts**: `CARGO_BUILD_JOBS` (default comes from `nproc`;
   nothing sets it today), `NIX_BUILD_CORES`, `MAKEFLAGS`/`MAKE_JOBSERVER`.
   Cheapest, but only covers tools that read them; also the *only* option that
   bounds concurrent-compiler **memory** as well as CPU (each `rustc` holds a few
   hundred MB; a CPU-only bound lets cargo spawn `nproc` of them and blow the
   memory budget while throttled).
2. **cgroup v2 CPU quota**: `cpu.max` on a `cang-workload` subtree, with
   `CONFIG_FAIR_GROUP_SCHED=y`/`CONFIG_CFS_BANDWIDTH=y` (`:208-209`) so it
   exists in the kernel. Obstacles to settle: nothing mounts cgroup2 in this
   guest (`/sys/fs/cgroup` is unmounted; `components/podman/config.rs:56` has
   `cgroups = "disabled"`), so who mounts it, does mounting change podman's
   detection, does the quota apply per cgroup across vCPUs, and does a throttled
   cgroup still let the *supervisor's* pump threads (root cgroup, nice `-20`)
   run. Also state the accounting: `cpu.max = (nproc-2)*100000 100000` caps the
   subtree at `nproc-2` cores, which is not the same as "leaving 2 cores idle".
3. **CPU affinity**: `sched_setaffinity` keeping the workload off 2 CPUs
   (`taskset`-style, inherited by children, no cgroup needed, no mount to
   argue about). Does it actually preserve latency for guest-init threads, and
   what does it cost the build (oversubscribing `nproc` jobs onto `nproc-2`
   CPUs)?
4. **Shares, not caps**: `cpu.weight` with the workload at a low weight, and
   whether that is enough without any hard bound.

Where the workload PID is placed matters and differs by launch shape: the exec
path (`runtime/cang.rs`) *is* the process that execs, so it can move itself
before `exec` and let children inherit; the managed path forked the shell
(`session.rs:1521`) so the parent moves the child's pid and the supervisor stays
outside the bound.

Deliverable: `notes/04-cpu-bound.md` with one live arm per candidate, a
latency comparison for the interactive paths under each, and a recommendation
for ticket [09](09-rustc-job-cap.md): which bound ships, and whether the job
count still ships beside it for the memory half.

## Resolution

**Ship the affinity bound; refuse `cpu.max` and `cpu.weight`. Detail, per-arm tables and raw paths:
[notes/04-cpu-bound.md](../notes/04-cpu-bound.md).**

One real cargo storm (1067 crates, `RUSTC_WRAPPER=` empty, `CARGO_INCREMENTAL=0`, cold
`/dev/shm` target, `--mem 8`, `nproc=26` -> `jobs=max(1,24)`), one boot per arm, PTY round trip and
`cang exec` start cost driven from the host, per-CPU busy counted in `/proc/stat` jiffies and the
workload's CPU in `wait4` rusage ticks.

| candidate | leaves 2 cores free? | covers rustc / make / nix? | build cost | verdict |
|---|---|---|---|---|
| affinity `taskset -c 0-23` | **yes**: cpu25 115/5780 ticks = 2.0 %, cpu24 243/5950 = 4.1 % | rustc yes (mask on the children), make yes via `nproc`, nix **no** (daemon outside the subtree) | wall 59.64 -> 57.40 s, CPU ticks 136 055 -> 121 912 | **ships** |
| cgroup `cpu.max = 2400000 100000` | **no**: 98 throttle periods, 675 639 us throttled, yet every CPU stayed 73 %+ busy (least 4403/6010) | same subtree limits, nix no | wall +0.1 % | refused |
| env `CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=24 MAKEFLAGS=-j24` | **no**: least busy CPU 4741/5955 = 79.6 % | cargo yes, make yes where make exists (it does not in this image), nix no (`cores` is per job, not `max-jobs`) | wall -0.8 % | not a CPU bound; ships only as the default-only job count beside the mask |
| cgroup `cpu.weight = 1` | **no**: 0 throttles, least 5329/5990 = 89 % | - | exec p50 45.6 ms (worst clean arm) | refused |
| one `cpu.max` over the whole guest (the "covers any workload" shape) | no | would | **wall 59.64 -> 128.47 s, CPU ticks 136 055 -> 264 042**, PTY p50 1.95 -> 12.3 ms, exec p50 39.4 -> 125.5 ms | refused |

Settled sub-questions:

1. **Where the bound goes.** Every launch in this tree is a managed session (`ManagedSessionConfig` is
   built on the launch path, `runtime/session/mod.rs:515,634`), so the bound belongs on the forked
   payload (`.../runtime/session.rs:1516` -> `child_main_result` before the `exec`) and on every
   `cang exec` child (`.../runtime/exec.rs:143-162`'s `pre_exec`) - never on the supervisor or the
   listener, whose threads are the nice `-20` interactive path (measured: an exec'd child carried the
   supervisor's own `0-25` mask).
2. **kswapd0/kcompactd0 are re-affinable** (no `kthread_bind` in `mm/`, no `PF_NO_SETAFFINITY`):
   `taskset -pc 24-25` on both held, mask read back `24-25`. What it buys is not measurable here -
   this storm never swapped (`SwapFree` never left `SwapTotal`, zram `orig_data_size` stayed 4096), so
   the reclaim half stays with tickets 06/11.
3. **The cgroup obstacle in the ticket was stale**: the guest's PID 1 (`init.krun`, the pinned
   libkrun init) already mounts cgroup2 at `/sys/fs/cgroup`
   (`deps/libkrun/init/init-binary/src/fs.rs:52-62`), and podman is neither the mounter nor broken by
   it (`podman load` + `podman run` succeeded in every arm, and still ran after a boot-time `umount`).
   What is *not* done: `echo '+cpu'` into `/sys/fs/cgroup/cgroup.subtree_control` - without it a child
   cgroup has no `cpu` controller and `cpu.max` lands in an ordinary empty file (two void boots).
4. **The job count follows the bound for free**: inside the mask `nproc` = 24 and
   `len(os.sched_getaffinity(0))` = 24; inside `cpu.max` `nproc` = 24 but `sched_getaffinity` = 26;
   cargo's compiler population peaked at 24 in both shapes (26 in the control), so peak `AnonPages`
   fell 13.2 % under affinity (3 375 420 -> 2 927 976 kB) versus 8.4 % under the env job count. That
   is the memory half, without exporting `CARGO_BUILD_JOBS` - but keep the export as the
   default-only fallback for a launcher that cannot see the bound.
5. **nix is the one workload a subtree bound cannot cover**: `nix-daemon --daemon` is its own root
   process (outside the shell's subtree), so its builders inherit *its* mask and cgroup; and
   `NIX_BUILD_CORES` is cores-per-job, not the job count. A `nix build` storm was not run - 09 must
   either accept the gap or apply the bound to the daemon as a guest-init component.
6. **The latency the bound buys at `N=26` is a null**: PTY p50 1.8-2.2 ms and exec p50 36-46 ms in
   every bounded arm, with unordered p90/tail spreads (3.3-5.2 ms / 52-82 ms) - the only ordered
   result is the whole-guest quota being 5-13x worse. The reserve is a resilience lever, and the
   memory-pressure arms (01/05/11) are what can price it.

For [09](09-rustc-job-cap.md) the values to ship are: mask `0..N-3` (here `0-23`), `kswapd0`/
`kcompactd0` on `N-2..N-1` (here `24-25`), `CARGO_BUILD_JOBS=max(1, N-2)` (here 24) *only when unset*,
`N=2` -> mask `0-0`/1 job, `N=1` -> no bound; no `cpu.max`, no `cpu.weight`, no whole-guest cgroup, no
`/sys/fs/cgroup` mount.
