---
label: wayfinder:task
title: The default launch takes both bounds, and the early responder fires before the wall
status: closed
blocked_by: []
claimed_by: "pi arm-13 (child, agent, 2026-11-25)"
---

## Question

Two claims that the earlier arms left open, provable in one boot campaign.

1. **The default launch is bounded.** [09](09-rustc-job-cap.md) found the seam bug: a
   default `cang` launch (payload dropped to the dev identity) reached
   `drop_to_identity_and_exec`, which reset only the OOM score, so the payload and
   every `rustc` kept `Cpus_allowed_list 0-25` and the guest left **no core idle**
   (least busy 54 %) - the shape [04](04-workload-cpu-bound.md) called *not a CPU
   bound*. The fix is `process::become_workload()`, one call carrying both halves,
   used by all three seams. Prove on a live default launch: payload `nproc 24`,
   mask `0-23`, supervisor still `0-25`, `rustc` inherits, two cores near-idle.
2. **The early responder acts before the wall.** [11](11-earlyoom.md) shipped
   `earlyoom` (`-m 10 -s 100 -p`, its own `-1000`) and left the live boot owed. Arm
   01's arm 4 is the control: `mem_used_total` pinned at `mem_limit`, 158 350-175 997
   refused stores, ~2.2M page scans in the second before its kill. The responder's
   claim is that none of that happens: it kills the pressure's owner while
   `MemAvailable` is still healthy, `failed_writes` stays near zero, and `oom_kill`
   stays 0.

Both are code deltas in the current worktree plus an image rebuild (`pkgs.earlyoom`
joined the tooling layer), so the booted identity of guest-init *and* image is part
of the evidence.

Resolution = `notes/13-default-bound-and-earlyoom.md` with the read-backs and the
control comparison.

## Resolution

Closed 2026-11-25 by arm 13 (three live boots, one guest at a time, every gate
`foreign_workers=0`), evidence in
[notes/13-default-bound-and-earlyoom.md](../notes/13-default-bound-and-earlyoom.md).
**Both claims hold on the default launch.**

**1. The default launch takes both halves of the bound.** Boot `arm13a`,
`cang --image localhost/cang:nixone --guest-init <0.23.6>/bin/cang-guest-init --mem 4 -- sh
/workspace/arm13a.sh`, **no `--root`**, then arm 09's 1067-crate storm
(`out/results.txt`):

```text
RESULT workload self pid=732 comm=sh nproc=24 cpus_allowed_list=0-23 py_sched_getaffinity=24 oom_score_adj=0
RESULT workload env CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=<unset> MAKEFLAGS=<unset>
RESULT supervisor pid=712 comm=cang-guest-init cpus_allowed_list=0-25 threads=4 oom_score_adj=-1000
RESULT ancestry_child_first 732:sh:0-23 712:cang-guest-init:0-25 1:init.krun:0-25
RESULT responsive_status state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24
RESULT storm max_concurrent_rustc=24 rustc_masks={"0-23": 4426}
RESULT storm supervisor pid=712 masks_during_storm={"0-25": 586}
```

4 426 `rustc` mask samples, one value (`0-23`); the supervisor `0-25` in 586/586 and unchanged
before and after. Per-CPU busy across the build: 24 mask cores at 0.769-0.823, the two reserved
cores at **0.176 / 0.156** (arm 09's pre-fix `D-dev` control: `0-25`, `nproc 26`, least-busy
0.542, no core under 10 %). The reserved cores are not *idle* to arm 09's 2-5 %, and a second
boot (`arm13a2`, `/dev/shm` target, `--mem 8`, arm 09's `D-dev` configuration) reproduces
0.176/0.210 and attributes it with a per-process CPU sampler: the workload is entirely inside
`0-23`, and the ~250 CPU-s on the freed cores are the guest's **unconfined** threads -
expedited-RCU grace-period kthreads (207 s), `khugepaged` (70 s), `cang-guest-init`/k-workers/nix
(~64 s) - which the scheduler places on the free cores exactly as the reserve intends. The
fractions exclude host steal (the host was at load 25-36 during both boots), which is why the
per-CPU tick sums are 10-20 % short of `wall x 100`; the fractions agree with the workload's own
rusage (19.3 vs 18.711 effective cores).

**2. The early responder acts before the wall.** Boot `arm13b`,
`ARM=arm13b MODE=session PAYLOAD=arm13b.sh MEM=4 tools/responsive-arm/run-arm.sh` (arm 01's
arm-4 shape: sampler + `presser` at `CHUNK_MB=32 MAX_MB=8192`). `earlyoom` killed **the presser,
pid 1090, `rc=143`**, 0.100 s after its threshold:

```text
mem avail:   368 of  3697 MiB ( 9.96%), swap free: 3932 of 3932 MiB (100.00%)
low memory! at or below SIGTERM limits: mem 10.00%, swap 100.00%
sending SIGTERM to process 1090 uid 1000 "presser": oom_score 945, oom_score_adj 0, VmRSS 3286 MiB
process 1090 exited after 0.100 seconds
mem avail:  3657 of  3705 MiB (98.72%)
```

and nothing the ticket proposed to score shows the wall:

| read-back | arm 01 arm 4 (control) | arm13b |
|---|---|---|
| `mem_used_total` vs `mem_limit` 1 031 000 064 | pinned at the limit | **20 480 kB in all 210 blocks** |
| `failed_writes` | 175 997 | **0** |
| `pgscan_direct` in the last second | 2.2 M | **0 for the arm** (`pswpout`, `pgsteal_direct`, `pgscan_kswapd` also 0; PSI full total 237 µs) |
| `oom_kill` | 1 | **0** (all snapshots, all 177 vmstat blocks, and from inside `cang exec`) |
| console | `Out of memory: Killed process 22619 (presser)` | **no OOM line, no `Write-error on swap-device`** |
| session | survived | survived; `cang exec <task>` after the kill answers **rc=0**, `MemAvailable` back to 3.66 GB |
| earlyoom itself | n/a | census `earlyoom -1000` at PRE/AFTER-KILL/AFTER-HOLD |

The sampler's own timeline (58 blocks with the presser, RSS 39 MB -> 3255 MB, `MemAvailable`
1016 -> 442 MB) ends with the presser gone and `AnonPages` 3264 -> 51 MB in one tick, with
`SwapFree` never leaving `SwapTotal`: the kill happened with ~9 % of guest RAM still free.

**Identity.** The boots ran guest-init
`/nix/store/1mwvy9pxj12yfxgyl2lmrf7i223y98xs-cang-static-x86_64-unknown-linux-musl-0.23.6` (check
phase `437 passed` + relay `31 passed`; src `/nix/store/6c9hcs3v…-cang-workspace-source`
byte-identical to the worktree for `responsive.rs`, `process.rs`, `runtime/cang.rs`,
`cpu_bound.rs`) and image `localhost/cang:nixone` ID `3f2be435d370`
(`sha256:04b32a0f…`), rebuilt this session because the tooling layer now carries
`earlyoom-1.9.0` (probed inside the loaded image). Two facts a later arm should know: the first
`nix build .#container` failed `E0583` because the new `components/responsive` modules were
untracked (`git add -N` fixed it), and `EARLYOOM_ARGS` lost `-p` mid-campaign - the `-1000`
census read-back above is the proof the replacement (nice in `pre_exec`) works.

**Not established, stated rather than hidden:** one boot per claim; the reserved-core occupancy
is measured and attributed but its *cause* (why expedited-RCU/`khugepaged` churn was 277 CPU-s
here and absent in arm 09's window) is open; no `CANG_GUEST_RESPONSIVE=0` live control (the
control is note 01's arm 4, taken before the responder existed); no SIGKILL-path arm; the guest's
`/run/cang/earlyoom.log` is not retained whole - the two quoted slices in `out/results.txt` are.
