---
label: wayfinder:note
ticket: 04-workload-cpu-bound
---

# What bounds the workload's CPU: affinity leaves two cores, a cgroup quota does not

Authority: this checkout at HEAD `4e39708f8a66028127c9f747449448bd9714c92f` (host CLI
`/nix/store/x9dflild0m1ppfwl4l4pdhzhzdsa0zk3-cang-0.23.6/bin/cang`, guest-init
`/nix/store/az5idng9kg2caiwxh0rmcd41y0awl5a6-cang-static-x86_64-unknown-linux-musl-0.23.6/bin/cang-guest-init`,
both 0.23.6); kernel `7.3.5-hardened1` read from `.scratch/kernel-7.3.5/linux-7.3.5`
(the same extraction notes 02/03 cite) and `deps/libkrun/init/init-binary/src/fs.rs` for the guest's
cgroup mount; the guest is the preloaded image `localhost/cang:nixone`
(`d755ca7fe741...`, 8.36 GiB, **not rebuilt in this arm** - disclosed in §8). Every number below came
from a guest this arm booted; nothing is quoted from another note. Arm kit and raw artifacts:
`.scratch/arm04-cpu-bound/` (`ws04/arm04/arm.sh`, `ws04/arm04/storm.py`, `harness.py`,
`probe*.py`, `out/<arm>/`).

`N` = the guest's `nproc` = its vCPU count = **26** here (`resources.rs:8-15` takes
`available - 2` on a 28-CPU host; the rule therefore gives `jobs = max(1, N-2) = 24`).

> **Correction, 2026-11-25 (later the same day).** The `nix build` gap this note
> records is closed: the guest confines `nix-daemon` at spawn
> (`components/nix/root.rs::start_daemon`), so the daemon and every builder it
> forks for `nix build` run on the workload's cores. Live proof - five ON boots
> and one OFF control - in [note 14](14-nix-build-bound.md); what it costs
> (`nix eval`/`nix develop` are confined too) is in
> [docs/images-and-storage.md](../../../../docs/images-and-storage.md).
>
## 1. Who the bound has to cover, in this checkout's launch shapes

The ticket guessed two shapes and sorted the payload by them. Read against this tree, there are two
and the first one is universal:

1. **Every launch is a managed session.** The host builds a `ManagedSessionConfig` on the launch path
   (`crates/cang/src/runtime/session/mod.rs:515`) and hands it to the guest unconditionally
   (`managed_session: Some(...)`, `:634`), which sets `CANG_SESSION_MANAGED=1`
   (`runtime/launch/config/mod.rs:129`). Guest-init therefore never takes the raw `enter`-execs-itself
   branch for a normal launch: it forks the payload onto a PTY
   (`crates/cang-guest-init/src/guest_init/runtime/session.rs:1516` -> `child_main_result`, `setsid`
   at `:1563`, `exec` at `:1571-1575`) and stays behind as the supervisor. Its PTY pump/input threads
   run at nice `-20` (`.../runtime/session.rs:854,865`, `.../runtime/priority.rs:11`).
   **The bound belongs on the forked payload, in `child_main_result` before the `exec`** - and not on
   the supervisor, whose threads are the interactive path the rule is protecting.
2. **`cang exec` is a second child of that same supervisor.** The listener and per-client threads run
   at nice `-20` (`crates/cang-guest-init/src/guest_init/runtime/exec.rs:45,60`) and the command itself
   is spawned with a `pre_exec` hook that already sets `setpgid` and restores nice 0
   (`.../runtime/exec.rs:143-162`). **The bound belongs in
   that `pre_exec`**, for the same reason: the listener must not inherit it.
   Measured, not assumed: an exec'd `/bin/true` child carried `Cpus_allowed_list 0-25`, i.e. the
   supervisor's own mask (`out/probe/probe-mask.txt`) - so a bound applied to the supervisor would leak
   straight into the interactive path.
3. The raw `enter`-execs-itself branch (`crates/cang-guest-init/src/guest_init/runtime/cang.rs:420-432`) is still reachable for a
   guest-init invocation without `CANG_SESSION_MANAGED` (the workspace-server path); a shipped bound
   needs the same two lines before that `exec` too.

Both placements work the same way mechanistically, and that is the argument for the family: **a child
inherits both**. `fork` copies the affinity mask (`.scratch/kernel-7.3.5/.../kernel/fork.c:969-970`,
`dup_user_cpus_ptr`) and `exec` does not reset it (no `cpus` write in `fs/exec.c`); a child joins the
cgroup its parent is in at creation (`Documentation/admin-guide/cgroup-v2.rst:129-131`,
`cgroup_post_fork` at `kernel/cgroup/cgroup.c:7084-7112`). Measured in every bounded arm: the storm's
24 `rustc` children reported the same mask as their parent (`first_rustc_mask`), and in the cgroup
arms `first_rustc_cgroup` was `/cang-workload` (`ws04/arm04/out/A3b/storm.json`, `out/A2b/`, `out/A6/`).

## 2. The matching half: `kswapd0`/`kcompactd0` are re-affinable

The ticket asks whether a kthread that bound itself can be moved. Neither of the two reclaim threads
does: `kswapd` is created with `kthread_create_on_node` and never `kthread_bind`s itself
(`mm/vmscan.c:7702`), `kcompactd` likewise (`mm/compaction.c:3273`), and `PF_NO_SETAFFINITY` is only
set by `__kthread_bind_mask` (`kernel/kthread.c:566-580`). Zero `kthread_bind` calls exist anywhere in
`mm/`. Live read-back: both were on `0-25` at boot and both accepted `taskset -pc 24-25`:

```text
RESULT bound=kthread kswapd0     mask_before=0-25 mask_after=24-25 settable=yes
RESULT bound=kthread kcompactd0  mask_before=0-25 mask_after=24-25 settable=yes
RESULT kthread_after kswapd0 cpus_allowed_list=24-25      (arms A5/A5b)
```

What it *buys* is not measured here, because this storm never reclaimed: `SwapFree` never moved off
`SwapTotal` (`8149956 kB` before and after) and `/sys/block/zram0/mm_stat` was byte-identical
(`orig_data_size 4096`), so no zram compression ran at all. The placement is **prepared, not earned** -
tickets 06/11 own the reclaim arms ([the `zram_submit_bio` -> `zcomp_compress` path is synchronous in
the submitting context, `drivers/block/zram/zram_drv.c:2797` -> `:2309`, so a *background* kswapd is
exactly what carries that cost on the reserved cores]).

## 3. What the guest already offers (live read-back, `out/probe/`)

- **`/sys/fs/cgroup` is already mounted, by the pinned libkrun's init.** The ticket's "nothing mounts
  cgroup2" is stale for this pin: `deps/libkrun/init/init-binary/src/fs.rs:52-62` mounts
  `devtmpfs`, `proc`, `sysfs` and `cgroup2` at `/sys/fs/cgroup` (`MS_NODEV|MS_NOEXEC|MS_NOSUID|
  MS_RELATIME`) as part of `/init.krun`, the guest's PID 1 (`Name: init.krun`,
  `out/probe/probe-mask.txt`). Live mount line, identical in every arm:

  ```text
  cgroup2 /sys/fs/cgroup cgroup2 rw,nosuid,nodev,noexec,relatime 0 0
  ```
- Controllers at the root: `cpuset cpu io memory hugetlb pids`; `CONFIG_FAIR_GROUP_SCHED=y`,
  `CONFIG_CFS_BANDWIDTH=y`, `CONFIG_CPUSETS=y`, `CONFIG_CGROUP_CPUACCT=y`
  (`deps/libkrunfw/config-libkrunfw_x86_64-kvm:205-222`).
- **A subtree needs one write nobody does today**: `cgroup.subtree_control` is empty at boot, so a
  fresh child cgroup sees *no* controllers and a `cpu.max`/`cpu.weight` written there lands in an
  ordinary empty file instead of the controller (`out/void-A2-no-cpu-controller/`,
  `out/void-A4-no-cpu-controller/` - two of this arm's four void boots). After `echo '+cpu' > /sys/fs/cgroup/cgroup.subtree_control`
  the child reports `cgroup.controllers=cpu` and both files bind (`out/A2b/`, `out/A6/`).
- Tools: `nproc`, `taskset` (util-linux 2.42.4), `python3`, `cargo`, `rustc`, `gcc`, `nix` all present;
  **`make`, `ninja` and `cmake` are MISSING** from the image (`out/probe/probe-tools.txt`) - so
  `MAKEFLAGS` is a knob with no consumer in this image unless a user installs one.
- **A nix daemon runs outside any workload subtree**: `nix-daemon --daemon` as its own root process
  (`908 nix-daemon --daemon`, `out/probe/probe-nix.txt`; not present yet at arm start in boot A6 -
  `/run/cang/nix-prep.status` shows it appears ~110 s into the boot). `/etc/nix/nix.conf` is only
  `experimental-features = nix-command flakes`; `nix show-config` as root reports `max-jobs = 1`,
  `cores = 0`, `builders = @/etc/nix/machines`.
- Podman still works with cgroup2 present *and* absent: `podman load` + `podman run` of a 26 MB
  registry image succeeded in every arm (A0b/A2b/A4b), and after a boot-time `umount /sys/fs/cgroup`
  `podman info`/`ps`/`load` all still ran (it warns `Using cgroups-v1`, and does **not** remount -
  `out/probe/console-probe2.log`). Podman is not the mounter and is not broken by the cgroup tree;
  `containers.conf`'s `cgroups = "disabled"` (`components/podman/config.rs:56`) is what keeps it out.

## 4. The storm and the arms

Same storm in every arm: 1067 crates (800 leaves + 200 mid1 + 66 mid2 + 1 bin, `LEAVES=800
MODULES=6 LINES=3000` from `tools/rust-compile-bench/gen-workload.sh`, 711 MB of source in
`ws04/wl`), built cold into a fresh `/dev/shm/t-<arm>` target dir with `RUSTC_WRAPPER=` empty,
`CARGO_INCREMENTAL=0`, `cargo build --offline`. The build ran under `/workspace/arm04/storm.py`, which
reads `/proc/stat` per CPU in jiffies (`CLK_TCK=100`) across the storm window, `wait4`s cargo for
rusage (`RUSAGE_BOTH`, so cargo's reaped `rustc` children are included), and samples concurrent `rustc`
population, `AnonPages` and the cgroup's `cpu.stat`. The interactive paths were driven from the host
while the storm ran: a console round trip (host writes `PING n` on cang's stdin, the payload shell
reads it off `/dev/pts/0` and echoes `PONG n`, `ECHO` off) and `cang exec <task> /bin/true`, both timed
in `harness.py`.

| arm | bound | storm wall (s) | cargo+rustc CPU (ticks) | effective cores | max concurrent `rustc` | least-busy CPU (ticks busy/total) | cores under 10% | throttled | PTY p50/p90/max (ms) | exec p50/p90/max (ms) | peak `AnonPages` (kB) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **A0b** | none (control) | 59.64 | 136 055 | 22.81 | 26 | cpu10 5246/5977 = 0.878 | 0 | - | 1.95 / 5.20 / 311.2 | 39.4 / 63.5 / 83.7 | 3 375 420 |
| **A1b** | `CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=24 MAKEFLAGS=-j24` | 59.17 | 127 579 | 21.56 | 24 | cpu0 4741/5955 = 0.796 | 0 | - | 2.08 / 4.81 / 188.8 | 35.6 / 52.2 / 78.9 | 3 093 056 |
| **A2b** | cgroup `cpu.max = 2400000 100000` on the storm subtree | 59.67 | 127 746 | 21.41 | 24 | cpu0 4403/6010 = 0.733 | 0 | 98 periods, 675 639 us | 2.17 / 3.94 / 154.1 | 39.9 / 53.3 / 90.9 | 3 076 264 |
| **A3b** | affinity `taskset -c 0-23` (24 of 26 CPUs) | 57.40 | 121 912 | 21.24 | 24 | cpu25 **115/5780 = 0.020**, cpu24 **243/5950 = 0.041** | **2** | - | 1.79 / 3.31 / 204.1 | 37.6 / 52.1 / 77.0 | 2 927 976 |
| **A4b** | cgroup `cpu.weight = 1` on the storm subtree | 60.02 | 138 364 | 23.05 | 26 | cpu5 5329/5990 = 0.890 | 0 | 0 | 2.13 / 3.61 / 183.7 | 45.6 / 82.2 / 97.6 | 3 347 176 |
| **A5b** | affinity `0-23` **+ `kswapd0`/`kcompactd0` -> `24-25`** | 62.55 | 124 497 | 19.90 | 24 | cpu24 **180/6435 = 0.028**, cpu25 **225/6325 = 0.036** | **2** | - | 2.00 / 4.17 / 324.3 | 36.9 / 61.0 / 88.3 | 3 058 980 |
| **A6** | one `cpu.max = 2400000 100000` over the **whole guest** | **128.47** | **264 042** | 20.55 | 24 | cpu0 8258/11789 = 0.700 | 0 | **576 periods, 152 499 053 us** | **12.30 / 304.3 / 1268.6** | **125.5 / 793.2 / 1934.5** | 2 751 600 |

Every arm ran its own boot, in the first series' order: A0b, A1b, A2b, A3b, A4b, A5b (13:31-13:38
local) and A6 alone at 13:57. Raw per-arm evidence: `out/<arm>/results.txt` (guest `RESULT` lines),
`out/<arm>/storm.json`, `out/<arm>/host-<arm>.json`, `out/<arm>/build.log` (cargo's own output:
1067 `Compiling` lines and `Finished ... in <wall>`), `out/<arm>/console-<arm>.log`.

Two more boots were run but are **not** in the table: A0 at 13:21 (harness defect: `PONG` marker
mismatch, void) and A3c/A2c at 14:03-14:06, which are load-independent-only (see §8) and exist to
settle what a tool *inside* the bound sees (§5.4).

## 5. What each lever actually bounds

### 5.1 Affinity leaves the cores; a quota caps the subtree but keeps every CPU busy

This is the ticket's own question - "`cpu.max = (nproc-2)` cores is not the same as leaving 2 cores
idle" - now answered in hardware-counted ticks:

- Control A0b: busiest-to-least-busy spread is 5246..5403 ticks of 5977 over 59.64 s - **no CPU below
  88 % busy**. The rustc storm wants every core it is given.
- cgroup `cpu.max` A2b: the quota is *spent* (usage 1 292 807 227 us over 59.67 s = 21.41 effective
  cores of the 24-core quota, 98 throttle periods, 675 639 us throttled) yet **all 26 CPUs stayed
  busy, least 4403/6010 = 73 %**. Throttling spreads the cap over every CPU; it does not vacate two.
- Affinity A3b: two CPUs are **near-idle** - cpu25 115/5780 ticks (2.0 %), cpu24 243/5950 (4.1 %) -
  and the remaining 24 CPUs are *more* busy than the control's (0.878-0.912), because cargo puts the
  work there instead of spreading it. That is what the rule asks for, measured.
- `cpu.weight = 1` A4b: **zero throttles, zero idle cores** (least 5329/5990 = 89 %); the interactive
  threads are already nice `-20`, so a share-based weight has nothing to win.

### 5.2 The build does not pay for affinity (and did pay, massively, for the whole-guest quota)

Same 1067 compilations, cargo's own `Finished ... in` line in parentheses:

| arm | wall (s) | vs control | cargo CPU ticks | vs control |
|---|---|---|---|---|
| A0b none | 59.64 (`59.52s`) | - | 136 055 | - |
| A1b jobs=24 | 59.17 (`59.05s`) | -0.8 % | 127 579 | -6.2 % |
| A2b `cpu.max` 24/26 | 59.67 (`59.56s`) | +0.1 % | 127 746 | -6.1 % |
| A3b affinity 0-23 | 57.40 (`57.27s`) | **-3.8 %** | 121 912 | **-10.4 %** |
| A4b `cpu.weight=1` | 60.02 (`59.91s`) | +0.6 % | 138 364 | +1.7 % |
| A5b affinity + kthreads | 62.55 (`1m 02s`) | +4.9 % | 124 497 | -8.5 % |
| A6 whole-guest `cpu.max` | **128.47 (`2m 08s`)** | **+115 %** | **264 042** | **+94 %** |

A6 is the important negative: putting the quota around *everything* (so that it would cover any
workload, including the nix daemon) cost 2.15x the wall time **and** doubled the CPU time, and it did
so in the guest's *system* time: `cpu.stat` `system_usec` +1 079 318 284 us (103 228 of the storm's
264 042 jiffies) against the control's 9 923 - the CFS bandwidth throttle/unthrottle path on a
24-of-26-core group that is chronically at its limit. It also produced the campaign's **worst interactive numbers**
(PTY p50 12.3 ms vs ~2 ms; `cang exec` p50 125 ms vs 36-46 ms, p90 793 ms), because the supervisor
and its pumps are inside the same quota.

### 5.3 What the interactive paths actually cost under each bound

With 26 vCPUs, **no arm's interactive paths starve**, and none of the bounds buys a latency number
either:

| arm | PTY p50 | PTY p90 | PTY max | exec p50 | exec p90 | exec max |
|---|---|---|---|---|---|---|
| A0b none | 1.95 | 5.20 | 311.2 | 39.4 | 63.5 | 83.7 |
| A1b jobs | 2.08 | 4.81 | 188.8 | 35.6 | 52.2 | 78.9 |
| A2b `cpu.max` | 2.17 | 3.94 | 154.1 | 39.9 | 53.3 | 90.9 |
| A3b affinity | 1.79 | 3.31 | 204.1 | 37.6 | 52.1 | 77.0 |
| A4b `cpu.weight` | 2.13 | 3.61 | 183.7 | 45.6 | 82.2 | 97.6 |
| A5b affinity+kthreads | 2.00 | 4.17 | 324.3 | 36.9 | 61.0 | 88.3 |
| A6 whole-guest | **12.30** | **304.3** | **1268.6** | **125.5** | **793.2** | **1934.5** |

`n=28-31` samples in the six series-2 arms and 55 in A6; p99 == max at that n, so it is not quoted
separately. The p90 spreads
(3.3-5.2 ms PTY, 52-82 ms exec) are single-boot variation, not an arm ordering; the outlier tails
(154-324 ms) are unordered by bound and appear on the control too. The only real *order* is A6, and it
is 5-13x worse. This is the honest null for the ticket's latency hope: **at `N=26` the two reserved
cores are not what protects the keystroke - nice `-20` already does (note 36's 24.1 -> 7.5 ms p99),
and a 26-vCPU guest has enough cores that nothing starves. The bound's value is the reserve itself,
which the memory-pressure arms (01/05/11) are the ones that can price.**

### 5.4 The job count follows the bound, so affinity carries the memory half too

Both a quota and an affinity mask are visible to the job-count discovery tools, and that is what
bounds concurrent-compiler memory without `CARGO_BUILD_JOBS`:

| arm | `max_concurrent_rustc` (0.4 s sampling) | peak `AnonPages` | vs control |
|---|---|---|---|
| A0b none | 26 | 3 375 420 kB | - |
| A1b `CARGO_BUILD_JOBS=24` | 24 | 3 093 056 kB | -8.4 % |
| A2b `cpu.max` 24/26 | 24 | 3 076 264 kB | -8.9 % |
| A3b affinity 0-23 | 24 | 2 927 976 kB | **-13.2 %** |
| A4b `cpu.weight=1` | 26 | 3 347 176 kB | -0.8 % |

And the discriminating read-back (boots A3c/A2c, load-independent):

```text
A3c jobcount affinity       nproc=24  py_affinity=24  cgroup=/          cpu_max=(none)
A2c jobcount cgroup         nproc=24  py_affinity=26  cgroup=/cang-workload cpu_max=2400000 100000
A2c jobcount plain (outside) nproc=26 py_affinity=26  cgroup=/
```

So `nproc` (coreutils 9.11) reports 24 in **both** shapes, `sched_getaffinity` reports 24 only under
the mask, and cargo's 24-compiler population in both arms says cargo's `available_parallelism` saw
the bound either way. `make -j$(nproc)` would follow the mask; a cgroup quota is invisible to
`sched_getaffinity`, so a launcher that reads the mask alone would not see a quota at all.

### 5.5 What the bound does *not* cover: nix

Neither a subtree affinity nor a subtree cgroup reaches a nix build, because the guest's
`nix-daemon --daemon` is a separate root process that is not a descendant of the shell
(`/run/cang/nix-prep.status` `state=ready`, `nix show-config` `max-jobs = 1` client-side,
`/etc/nix/nix.conf` setting neither). The daemon forks the builders, and they inherit *its* mask and
cgroup. Also: **`NIX_BUILD_CORES` is not the job count** - `cores` is how many cores *each* builder
job may use (`cores = 0` here = all), while the number of concurrent builders is the daemon's
`max-jobs`. So the job-count family cannot be claimed to cover nix either, and a "covers any
workload" CPU bound would have to be applied to the daemon itself (a guest-init component decision,
not a subtree one) or be a whole-guest bound - which §5.2 prices at 2.15x. A real `nix build` storm
was **not** run here (§8).

## 6. Verdicts

1. **Ship the affinity bound.** `taskset`-style `sched_setaffinity` to CPUs `0..N-3`, leaving the last
   two to everything else, applied to the managed-session payload child and to each `cang exec` child
   (§1). It is the only candidate that **actually leaves two cores free** (cpu25 115/5780 ticks = 2.0 %,
   cpu24 243/5950 = 4.1 %), it needs **no mount and no system-wide write**, it inherits across fork+exec
   for real `rustc` children (mask read back on them), it **costs the build nothing** (-3.8 % wall,
   -10.4 % CPU ticks), and it is the arm with the lowest peak anonymous memory (-13.2 %), because
   cargo's own job count follows the mask.
2. **Refuse `cpu.max` as the default.** It bounds throughput and is measurable as bound (98 throttled
   periods, 675 639 us; usage 21.41 effective cores), but it leaves **no idle core** (least busy 73 %),
   it needs `+cpu` written into `cgroup.subtree_control` at boot (a guest-wide policy write that nothing
   does today), and quota visibility is not the rule's wording. Its whole-guest form is 2.15x slower
   with the worst interactive numbers of the campaign.
3. **Refuse `cpu.weight` on its own.** 0 throttles, no free core, and the worst clean exec p50
   (45.6 ms) with no measured win; the nice `-20` pump already outranks a 1/101 weight.
4. **`CARGO_BUILD_JOBS` alone is not a CPU bound** - it leaves every CPU at 80-87 % busy - so it does
   not satisfy the rule; **but it is not redundant with affinity either**: it is the one export that
   survives a launcher that cannot see the bound, and it is what the memory half is stated in. Ship it
   *only when unset* (`sccache.rs`'s precedent), value `max(1, N-2)`, and do not present it as covering
   nix (§5.5).
5. **The kthread half is settable and unmeasured**: `kswapd0`/`kcompactd0` accept `taskset -pc`
   (mask 0-25 -> 24-25, held), and no reclaim ran in this storm, so its value belongs to 06/11.

## 7. The numbers ticket 09 should adopt

- Guest `nproc` = vCPU count = **26** here; the rule gives `jobs = max(1, 26-2) = 24`.
- Workload CPU bound: **affinity mask `0-23`** (`0..N-3`), the last two CPUs reserved. Measured
  reserve: cpu25 115/5780 jiffies busy = 2.0 %, cpu24 243/5950 = 4.1 %, versus the control's least
  busy CPU 5246/5977 = 87.8 %.
- Kernel threads: `kswapd0` and `kcompactd0` -> **`24-25`** (accepted, held; `kswapd0` has no
  `PF_NO_SETAFFINITY`).
- Job count, default-only: **`CARGO_BUILD_JOBS=24`**, plus `NIX_BUILD_CORES=24` only as the
  per-builder core hint (not a job count, §5.5). `MAKEFLAGS=-j24` has no consumer in this image (`make`
  is MISSING) but does no harm where a user installs make.
- Placement: managed-session payload child before its `exec` (`.../runtime/session.rs:1516` ->
  `child_main_result` at `:1559`, after `setsid` at `:1563`), and every `cang exec` child in the
  `pre_exec` at `.../runtime/exec.rs:143-162`. **Never** the supervisor or the exec listener threads
  (they are the nice `-20` interactive path).
- Small-`nproc` edges: `N=2` -> `jobs=1`, mask `0-0` (one CPU reserved); `N=1` -> no bound and no
  export (there is no second CPU to reserve).
- Do **not** ship: `cpu.max`, `cpu.weight`, a whole-guest cgroup, or a mount of `/sys/fs/cgroup` (the
  libkrun init already mounts it).
- Expected price of the bound, on the same 1067-crate storm: wall 59.64 -> 57.40 s, CPU time
  136 055 -> 121 912 jiffies, peak anon 3 375 420 -> 2 927 976 kB, interactive paths unchanged
  (PTY p50 1.95 -> 1.79 ms, exec p50 39.4 -> 37.6 ms).

## 8. What this does not establish

- **No exclusive host window.** The gate (`pgrep -af libkrun-vm-worker-enter`, excluding this
  workspace's own state) recorded `foreign=0 launches=0` at the start of every scored arm
  (`logs/gate.log`), but the 5 s contamination sampler could not be read as co-tenancy: this arm's own
  VM worker's argv carries the workspace *slug* (`ws04`) rather than the directory name the filter
  tested for, so it counted itself as foreign (`logs/contamination.log`, `FOREIGN_VMS=2` at every boot).
  A sibling session was demonstrably booting guests on this host at 13:59-14:06 (its `ws12` workers).
  **A3c and A2c were run in that window and their wall/CPU/latency numbers are discarded** (79.5 s and
  89.9 s wall, 4-5x the guest system time of their clean twins at equal effective cores); they contribute
  only the load-independent read-backs (masks, cgroup membership, `jobcount`). Everything in §4-§7 is
  from the 13:31-13:38 series, whose six boots sat in one window on purpose - and is therefore a
  *comparison within one window*, not an absolute performance record.
- **One boot per arm.** §4's wall/CPU columns are single boots; the only repeat of an arm (A3c) is
  contaminated. The direction check that exists is A0 vs A0b (two control boots: 68.41 s and 59.64 s
  wall, both saturating all 26 CPUs), which is why the wall numbers are read as a *comparison*, not a
  measurement of typical build time.
- **The image was not rebuilt.** Every arm booted the preloaded `localhost/cang:nixone`
  (`d755ca7fe741...`, an 8.36 GiB dev image built outside this arm; rebuilding `.#container` and loading
  it into a hermetic store would have cost ~8 GiB of the 23 GiB free and minutes of host CPU in the
  middle of a sibling arm's window). Tool presence is therefore a claim about *that* image, read back
  live (§3); the CPU-bound mechanism itself is image-independent. Likewise the guest-init binary is
  `az5idng9...` 0.23.6, built at 12:50 from a worktree that other arms have since continued to edit
  (`crates/` carried in-flight changes throughout; this arm edited nothing under `crates/`, `nix/`,
  `scripts/` or `tools/`).
- **No memory pressure.** `SwapFree` never left `SwapTotal` and zram's `orig_data_size` stayed at
  `4096` in all arms, so nothing here prices kswapd placement, direct reclaim, or a latency win under
  memory pressure. That is tickets 01/05/06/11's arm.
- **No nix storm, no container inside the bound.** §5.5's nix gap is structural (a live daemon pid and
  its own cgroup/mask, plus nix's `cores` vs `max-jobs` semantics), not a measured build; and no
  `podman run` was executed *inside* the bounded subtree (podman load/run were run as the workload,
  unbounded, to check the cgroup2 hazard).
- **Single-boot `AnonPages` peaks.** The memory column is the peak of a 0.4 s sampler inside the guest,
  not the sum of RSS, and one boot per arm cannot establish a distribution.
- **jiffy granularity.** Per-CPU busy numbers are `USER_HZ=100` ticks (10 ms) over a ~60 s window
  (`CLK_TCK` read in the guest), so the two free CPUs' 115 and 243 ticks are 2 and 5 tick-quanta -
  directionally decisive, but not sub-tick precise.
- **A harness defect worth recording**: `cgroup.cpu.max` and `cpu.weight` are pseudo-files that
  `stat` as size 0, so the arm script's `[ -s "$CG/cpu.max" ]` "bound_enforced" check printed `no` even
  where the write bound (read-back `2400000 100000`, 98 throttle periods). Trust the read-back and
  `cpu.stat`, not the size test (`out/A2b/results.txt`).

## 9. Raw artifacts

| path | holds |
|---|---|
| `.scratch/arm04-cpu-bound/logs/gate.log` | per-arm exclusivity gate line (`foreign=`, `launches=`, `pgrep_x_cang=`) and post-arm census |
| `.scratch/arm04-cpu-bound/logs/series2.log`, `series3.log` | arm order and timings |
| `.scratch/arm04-cpu-bound/logs/contamination.log` | 5 s foreign-worker samples (self-matching; see §8) |
| `.scratch/arm04-cpu-bound/out/<arm>/results.txt` | guest `RESULT` lines (bounds, per-CPU jiffies, `cpu.stat`, kthread masks, podman, jobcount) |
| `.scratch/arm04-cpu-bound/out/<arm>/storm.json` | raw per-CPU arrays, rusage ticks, cgroup deltas, masks/cgroups |
| `.scratch/arm04-cpu-bound/out/<arm>/host-<arm>.json`, `console-<arm>.log` | host-timed PTY/exec samples and the guest console |
| `.scratch/arm04-cpu-bound/out/<arm>/build.log` | cargo's own output (`Finished ... in <wall>`) |
| `.scratch/arm04-cpu-bound/out/probe/` | environment probe: mounts, cgroup, tools, nix, masks, meminfo, `console-probe2.log` (the `umount`/podman test) |
| `.scratch/arm04-cpu-bound/out/void-A0-run1`, `void-A1-argv-json`, `void-A2-no-cpu-controller`, `void-A4-no-cpu-controller` | the four void boots, each named for the defect that voided it |
