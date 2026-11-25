---
label: wayfinder:note
ticket: 13-default-bound-and-earlyoom
---

# Both claims hold in one campaign: the default launch takes the whole CPU bound, and the early responder kills the pressure's owner with 368 MB still free

Three boots of this worktree (2026-11-25 08:19-08:49Z, one guest at a time, every gate
`foreign_workers=0`) answer the two questions ticket 13 left open. Both are **positive**, and
both are now proven on the *default* launch - the shape a user gets with no `--root`:

- **Claim A.** The payload is `nproc 24`, `Cpus_allowed_list 0-23`, and every one of 4 426 `rustc`
  mask samples during a 1067-crate storm reads `0-23`, while the **supervisor** stays `0-25` in
  586/586 samples. The 24 cores the mask leaves run at 0.77-0.82 busy; the two it excludes at
  **0.176 / 0.156**. `CARGO_BUILD_JOBS=24` and `/run/cang/responsive.status` agree with the mask.
  The pre-fix control was arm 09's `D-dev` default launch: `0-25`, `nproc 26`, no core under 10 %
  (least busy 0.542).
- **Claim B.** `earlyoom` fires at **9.96 % of MemAvailable (368 MB of 3 697 MiB) and kills the
  presser 0.100 s later**, and the guests' own counters show the wall was never approached:
  `mem_used_total` **20 480 kB of `mem_limit` 1 031 000 064** in all 210 sampler blocks,
  `failed_writes` **0**, `SwapFree` **never moved**, `pgscan_direct`/`pgscan_kswapd`/`pswpout`
  **0** for the whole arm, `oom_kill` **0**, and no kernel line on the console at all. The kernel's
  killer never ran; the session survived and answered a `cang exec` with `rc=0` after the kill.
  Arm 01's arm 4, the control, pinned at `mem_limit`, refused 175 997 stores and had 2.2M page
  scans in its last second.

Identity of the booted bytes (the campaign overlapped other agents' edits, so each is named):

| | |
|---|---|
| host CLI | `/home/dev/cang/cang/result/bin/cang` 0.23.6 (`/nix/store/x9dflild0…-cang-0.23.6/bin/cang`) |
| guest-init | `/nix/store/1mwvy9pxj12yfxgyl2lmrf7i223y98xs-cang-static-x86_64-unknown-linux-musl-0.23.6/bin/cang-guest-init`, sha256 `1e40265426a319e8…463ec41b`; drv `/nix/store/iq77wkid3xcpdr6c2xwnshrz8kv52gw5-…drv`; src `/nix/store/6c9hcs3v37ig4gq3845whkh4nv90jg1b-cang-workspace-source` |
| image | `localhost/cang:nixone` ID `3f2be435d370`, digest `sha256:04b32a0fc16d3bd9664ead63f892d269e26641f658ac2beb8482e43a0eea96f8`, built this session from `nix build .#container` → `/nix/store/d2vw72xac125mk5g1kalk95fjgng9514-cang.tar.gz`; its entrypoint is a 0.23.6 guest-init package (`/nix/store/x93xvnbwrqxig656j83ff3jr37nybcxm-…-0.23.6/bin/cang-guest-init`, the 08:05 build; the boots pass `--guest-init` explicitly, so the entrypoint is not what ran), and its tooling layer 8 lists `/nix/store/c2dah59nl7433dqh43af6ikhcis283rg-earlyoom-1.9.0` (probe inside the loaded image: `command -v earlyoom` → `/nix/store/1dwkcb0vk1j32c4ihnla0r9ff210nafw-cang-tooling-layer/bin/earlyoom`, banner `earlyoom 1.9.0`) |
| guest kernel | `7.3.5-hardened1` in all three boots; 26 vCPUs (`vcpus_online=26`), 28-CPU host |
| build identity | `.scratch/arm13/provenance.txt`: guest-init check phase `running 437 tests` / `ok. 437 passed` and relay `ok. 31 passed`; src-vs-worktree sha256 equal for `components/responsive.rs`, `process.rs`, `runtime/cang.rs`, `runtime/cpu_bound.rs`, `nix/image/layers.nix` |

Two notes on the identity, because they shaped the campaign:

1. The first `nix build .#container` of this session **failed** with `error[E0583]: file not found
   for module 'responsive'` - the flake's source is the git tree, so the two new-but-untracked
   module files were not in it. `git add -N` on
   `crates/cang-guest-init/src/guest_init/components/responsive.rs` and
   `crates/cang/src/runtime/launch/config/components/responsive.rs` fixed it (intent-to-add, no content
   staged); the rebuild then produced the image above.
2. `components/responsive.rs` **changed under the campaign** between my first build (08:05) and the
   first boot (08:19): `EARLYOOM_ARGS` lost `-p` (`["-m","10","-s","100"]`) and the niceness `-20`
   moved into the `pre_exec` hook, because *earlyoom applies `-p` after it starts, which would
   overwrite the `-1000` its own `pre_exec` wrote*. The boots below ran the **later** bytes
   (`cc7e381e…`, the file as it now stands), and claim B's census read-back of `oom_score_adj -1000`
   on a live `earlyoom` is what confirms the fix landed. My earlier build's bytes never booted.

## Claim A: the default launch takes both halves of the bound

Boot `arm13a`: `cang --image localhost/cang:nixone --guest-init <pkg>/bin/cang-guest-init --mem 4 --
sh /workspace/arm13a.sh`, **no `--root`** - i.e. the payload is dropped to the dev identity by
`process::drop_to_identity_and_exec`, the seam arm 09 found unbounded. The payload prints its own
read-backs and then runs arm 09's storm (`storm9.py`, the same 1067-crate tree from
`.scratch/arm04-cpu-bound/ws04/wl`, `CARGO_TARGET_DIR` on the workspace, cold). Verbatim
(`out/results.txt`):

```text
RESULT guest kernel=7.3.5-hardened1 vcpus_online=26 cpuinfo_processors=26 nproc=24 nproc_all=26 uid=1000 user=dev memtotal_kb=4027340
RESULT workload self pid=732 comm=sh nproc=24 cpus_allowed_list=0-23 py_sched_getaffinity=24 oom_score_adj=0
RESULT workload env CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=<unset> MAKEFLAGS=<unset>
RESULT supervisor pid=712 comm=cang-guest-init cpus_allowed_list=0-25 threads=4 oom_score_adj=-1000
RESULT ancestry_child_first 732:sh:0-23 712:cang-guest-init:0-25 1:init.krun:0-25
RESULT responsive_status state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24
RESULT earlyoom binary=/nix/store/1dwkcb0vk1j32c4ihnla0r9ff210nafw-cang-tooling-layer/bin/earlyoom pid=725 oom_score_adj=-1000
RESULT storm wall_s=261.083 cargo_exit=0
RESULT storm effective_cores=18.711 max_concurrent_rustc=24 rustc_masks={"0-23": 4426}
RESULT storm first_rustc_chain=[{"pid": 934, "comm": "rustc", "mask": "0-23"}, {"pid": 933, "comm": "bwrap", "mask": "0-23"}, {"pid": 872, "comm": "cargo", "mask": "0-23"}, {"pid": 866, "comm": "python3.13", "mask": "0-23"}, {"pid": 732, "comm": "sh", "mask": "0-23"}, {"pid": 712, "comm": "cang-guest-init", "mask": "0-25"}]
RESULT storm supervisor pid=712 masks_during_storm={"0-25": 586}
RESULT storm cpus_busy_over_90pct=0 cpus_busy_under_10pct=0 least_busy=('cpu25', 0.156)
RESULT workload_after nproc=24 cpus_allowed_list=0-23 py_sched_getaffinity=24
RESULT supervisor_after pid=712 cpus_allowed_list=0-25
RESULT kthreads_after kswapd0=0-25 kcompactd0=0-25
```

- **The workload.** `nproc 24` (`nproc --all` still 26 - the mask is an affinity bound, not a
  vCPU-count lie), `Cpus_allowed_list 0-23`, `len(os.sched_getaffinity(0)) = 24`,
  `CARGO_BUILD_JOBS=24`, `NIX_BUILD_CORES` and `MAKEFLAGS` unset. Every `rustc` the build started
  inherited it: 4 426 mask samples, **one** distinct value (`0-23`), and the first `rustc`'s
  ancestor chain is `rustc -> bwrap -> cargo -> python3.13 -> sh (0-23) -> cang-guest-init (0-25)`.
- **The supervisor.** `0-25` in all 586 samples during the storm, before and after it - the
  reserve is still *not* on the interactive path, which is the half of ticket 04's rule that must
  not move. Its `oom_score_adj` is `-1000` (ticket 08's guard), and so is `earlyoom`'s.
- **The record.** `/run/cang/responsive.status` reads
  `state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24` - the policy is readable instead of
  inferred from the mask (ticket 11's observability half).
- **What the cores did.** Per-CPU busy fraction across the build window (`storm.json`):

```text
cpu0-23: 0.769 - 0.823   (each)
cpu24:   0.176
cpu25:   0.156
```

  Against arm 09's `D-dev` default launch - `0-25`, `nproc 26`, `cpus_busy_under_10pct=0`,
  `least_busy 0.542` with every 26 CPUs in the build - the two reserved cores are now 3.1-3.5x
  less busy than the least-busy core of the unbounded shape, and 4.4-5.3x less busy than the cores
  the build actually runs on.

## The reserved cores are not *idle*, and boot two says why

Arm 09 measured 2.0 %/4.1 % on the same two cores. This arm measured 17.6 %/15.6 %, so I ran a
second boot (`arm13a2`) that changes **one** thing - the build's target goes to `/dev/shm` and
`--mem 8`, arm 09's `D-dev` configuration - and adds a per-process CPU sampler
(`cpuwatch.py`, 1 s ticks of every `/proc/<pid>/stat`):

| boot | shape | workload cores | cpu24 | cpu25 | wall | effective cores |
|---|---|---|---|---|---|---|
| arm13a | default, `--mem 4`, target on the workspace | 0.769-0.823 | 0.176 | 0.156 | 261.083 s | 18.711 |
| arm13a2 | default, `--mem 8`, target on `/dev/shm` | 0.861-0.929 | 0.176 | 0.210 | 645.495 s | 18.627 |

The target directory makes no difference to the reserve (0.176/0.156 vs 0.176/0.210), and arm13a2's
read-backs are identical in every respect that matters (`nproc 24`, `0-23`, supervisor `0-25`,
`rustc_masks={"0-23": 10489}`, `supervisor masks {"0-25": 1332}`, same status file). So the
occupancy is not the build's I/O. `cpuwatch` answers *who* spends it - over a 646 s window and
11 835.5 CPU-seconds total (`out/cpuwatch.json.txt`):

```text
rustc                              1125399 ticks  11254.0 s
rcu_exp_par_gp_kthread_worker/1       10849 ticks    108.5 s
rcu_exp_gp_kthread_worker              9878 ticks     98.8 s
khugepaged                             7030 ticks     70.3 s
cargo                                  6406 ticks     64.1 s
bwrap                                  5699 ticks     57.0 s
python3.13                             4823 ticks     48.2 s
cang-guest-init                        2701 ticks     27.0 s
wild                                   2165 ticks     21.6 s
kworker/0:2-events                     1837 ticks     18.4 s
nix                                    1200 ticks     12.0 s
```

`rustc` (11 254 s) is inside the mask; the expedited-RCU grace-period kthreads (207 CPU-s),
`khugepaged` (70 CPU-s) and the guest's own unconfined daemons (`cang-guest-init` 27 s, k-workers
~25 s, `nix` 12 s) are not - and 249 CPU-seconds is exactly what the two reserved cores measured
(0.386 cores over 645 s). **The reserve is doing what it is for: with 24 cores saturated, the
scheduler puts every unbound guest thread on the two that are free.** That is not the workload
escaping its mask, and the mask evidence above settles that separately.

Two instrument facts behind those fractions, because they differ from arm 09's window:

- **Host steal is in the numbers, not in the busy fraction.** On arm13a the per-CPU tick deltas sum
  to 455 090 while `sum(fraction) x wall x 100` implies 503 469 - a 48 379-tick (9.7 %-of-a-busy
  vCPU) shortfall; on arm13a2 it is 1 079 790 against 1 419 000 (≈20 %). That shortfall is steal:
  `/proc/stat`'s busy fields exclude it, so a busy fraction is "of non-stolen time", while
  `guest_total_cpu_ticks` counts only what the guest ran. Arm 09's own `D-dev` numbers were
  self-consistent (135 811 vs 135 775) because its host window was quiet; mine ran while a
  co-tenant was compiling (`cc1`, **load average 25-36** - quoted for honesty, not as a claim), and
  arm13a2's 10 m 44 s wall against arm 09's 62 s is that, not the guest.
- The fractions still agree with the workload's own rusage: 24 × 0.79 + 0.33 ≈ 19.3 busy cores
  against `effective_cores 18.711` (arm13a) and 24 × 0.90 + 0.39 ≈ 21.9 against 18.627 (arm13a2).
  The 4-14 % gap is the same steal. The claim rests on the fractions and the mask read-backs, not
  on the summed ticks.

## Claim B: the early responder fires with 368 MB still free, and the wall is never approached

Boot `arm13b`, arm 01's arm-4 shape through `tools/responsive-arm/run-arm.sh`
(`ARM=arm13b MODE=session PAYLOAD=arm13b.sh MEM=4`): a managed session (default launch), the
0.5 s sampler, then `rustc -O presser.rs` with `CHUNK_MB=32 MAX_MB=8192` (distinct,
incompressible pages). The presser was **pid 1090**, mask `0-23` (claim A's bound applies here
too), and it exited `rc=143` - SIGTERM - at 08:28:54Z. The responder's own log
(`/run/cang/earlyoom.log`, quoted through `out/results.txt` because the guest's `/run` dies with
the task dir) says, in order:

```text
earlyoom 1.9.0
mem total: 3932 MiB, user mem total: 3780 MiB, swap total:    0 MiB
sending SIGTERM when mem avail <= 10.00% and swap free <= 100.00%,
        SIGKILL when mem avail <=  5.00% and swap free <= 50.00%
…
mem avail:   412 of  3695 MiB (11.17%), swap free: 3932 of 3932 MiB (100.00%)
mem avail:   368 of  3697 MiB ( 9.96%), swap free: 3932 of 3932 MiB (100.00%)
low memory! at or below SIGTERM limits: mem 10.00%, swap 100.00%
sending SIGTERM to process 1090 uid 1000 "presser": oom_score 945, oom_score_adj 0, VmRSS 3286 MiB, cmdline "/workspace/out/presser"
kill_release: pid=1090: process_mrelease pidfd=4 success
process 1090 exited after 0.100 seconds
mem avail:  3657 of  3705 MiB (98.72%), swap free: 3932 of 3932 MiB (100.00%)
```

The victim it names is the presser the payload started (`presser_exit rc=143 pid=1090`) and the
trigger is 9.96 % of earlyoom's own `user mem total`. Two swap details, because ticket 11's decision
turned on them: the banner's `swap total: 0 MiB` was read **before** the guest's zram swap existed
(the background swap worker activates it just after the enter path), while every line from the first
trigger check onward reports `swap free: 3932 of 3932 MiB (100.00%)` - the device was up and
**empty**, and the sampler's `SwapFree` never moved from `SwapTotal` 4 027 336 kB. With `-s 100` the
swap half of earlyoom's condition is satisfied whatever the device holds, so only `MemAvailable`
decided the kill.

Scored against the five things the ticket asked for:

1. **A named victim, matching the presser.** `earlyoom.log` pid 1090 `"presser"`; the payload's own
   `PRESSER_EXIT rc=143 pid=1090`; the sampler's census shows pid 1090 `presser oom_score_adj 0`
   for 58 blocks and then gone. **The console has no `Out of memory: Killed process` line at all** -
   `host/vmm/<task>.helper.stderr.log` holds 10 lines, every one `krun_devices::virtio::vsock`
   receiver noise, and `guest-kernel-console.log` is 0 bytes (the known trap). That is the claim,
   not a gap: the kernel's killer never ran, so it never printed a victim.
2. **`mem_used_total` never reached `mem_limit`, and `failed_writes` stayed at zero.** From
   `out/samples.txt`, 210 blocks: `mem_used_total` **20 480 kB in every block** against
   `mem_limit 1 031 000 064`; `mem_used_max 20 480`; `io_stat` **`failed_reads 0 failed_writes 0
   notify_free 0`**; `SwapFree` **4 027 336 kB = `SwapTotal`, unchanged** in every block. The
   timeline the sampler recorded, anchored by the payload's markers (`PRESSER-START` 08:27:35Z,
   `PRESSER_EXIT` 08:28:54Z; blocks carry no timestamps - see the instrument note below):

   | block | MemAvailable | AnonPages | `mem_used_total` | presser RSS | presser adj |
   |---|---|---|---|---|---|
   | 52 | 1016 MB | 2681 MB | 20 480 | 2653 MB | 0 |
   | 55 | 837 MB | 2863 MB | 20 480 | 2837 MB | 0 |
   | 60 | 541 MB | 3159 MB | 20 480 | 3123 MB | 0 |
   | 61 | 479 MB | 3215 MB | 20 480 | 3185 MB | 0 |
   | 62 | **442 MB** | 3264 MB | 20 480 | 3255 MB | 0 |
   | 63 | **3654 MB** | **51 MB** | 20 480 | - | - |
   | 67 | 3672 MB | 51 MB | 20 480 | - | - |

   Between blocks 62 and 63 the presser died and its 3.2 GB came back. The sampler's last look
   before the kill saw 442 MB (12.2 %); earlyoom's own 10/s sampling caught 368 MB (9.96 %) and
   killed 100 ms later - i.e. **with ~9 % of guest RAM still free**, 23x above the 16 MB at
   which arm 4's wall began.
3. **`oom_kill` stayed 0.** `/proc/vmstat` in the PRE, AFTER-KILL and AFTER-HOLD snapshots
   (`out/snapshots.txt`), in 177 parsed vmstat blocks of `samples.txt`, and again from inside the
   `cang exec` after the kill: **0**. The reclaim counters say the same thing -
   `pgscan_direct`, `pgscan_kswapd`, `pgsteal_direct` and `pswpout` are **0 across the whole arm**
   (only `pgmajfault` moves: 1 028 -> 4 426), and PSI memory `full total` ends at **237 µs** for the
   whole boot, against arm 4's `full avg10 12.58` and ~2.2M scans in one second.
4. **The session survived, and answered.** After `PRESSER_EXIT` the payload held; the host ran
   `cang exec <task> -- sh -lc '…'` from the arm directory: **`rc=0`**, printing `EXEC_AFTER_KILL
   rc-ok`, the guest's `oom_kill=0` and `MemAvailable 3 744 276 kB`; the payload then wrote
   `done.txt` at 08:30:37Z, exited on it, and the driver's teardown found no leftover task.
5. **earlyoom is out of reach.** The sampler's census reads
   `proc 736 earlyoom -1000 263485 1131` at PRE, AFTER-KILL and AFTER-HOLD. This is the read-back
   that also confirms the mid-campaign fix: with `-p` still in the argument vector the same process
   would have re-set itself to `-100`.

## The control, side by side

| | arm 01 arm 4 (control, note 01 §3-§4) | arm13b (this arm) |
|---|---|---|
| shape | `--mem 4`, managed session, same presser | identical (`MODE=session`, `MEM=4`, `CHUNK_MB=32 MAX_MB=8192`) |
| who killed the presser | kernel OOM killer | **earlyoom, at 9.96 % MemAvailable** |
| `mem_used_total` | **pinned at `mem_limit` 1 031 000 064** at t=69.5 s | **20 480 kB, never moved** |
| `failed_writes` | **175 997** pages refused (687 MB) | **0** |
| `pgscan_direct` in the last second | 970 912 -> **3 170 803** (2.2M) | **0 for the arm** |
| `pswpout` | **5 769 355** | **0** |
| `oom_kill` | **1** | **0** |
| console | `Out of memory: Killed process 22619 (presser) … oom_score_adj:0` | no OOM line, no `Write-error on swap-device` |
| session | survived | survived, and a `cang exec` answers `rc=0` afterwards |
| cost to the guest | ~1 s of unproductive scanning, then a kill at the wall | a SIGTERM 0.100 s after the threshold, with 368 MB free |

## What this does not establish

- **One boot per claim, and the two claims were not interleaved.** Claim A has a same-shape second
  boot (`arm13a2`) for the reserved-core question only; claim B has no second boot, so its numbers
  are one guest's, compared against note 01's one guest.
- **The reserved cores are measured, not explained away.** 17.6 %/15.6 % (and 17.6 %/21.0 % in
  `/dev/shm`) is *not* arm 09's 2-5 %, and this arm does not establish why the guest's expedited-RCU
  and `khugepaged` churn was 277 CPU-s here and absent there. What is established is that the
  workload cannot be on those cores (4 426 + 10 489 mask samples, one value) and that the 249
  CPU-seconds on them is the same order as the unconfined consumer list. The host was loaded
  (25-36) during both boots; whether that also *causes* the churn is untested.
- **`--mem` differs between the two claim-A boots** (4 vs 8 GiB), deliberately: arm13a2 reproduces
  arm 09's `D-dev` configuration. Both guests were 26 vCPUs and both read the same status file.
- **The build's wall time is not a claim anywhere here.** 261 s on the workspace and 645 s on
  `/dev/shm` under load 25-36 are not comparable to note 09's 62 s, and this arm makes no
  performance claim from them.
- **Only the SIGTERM arm of earlyoom ran.** No arm drove the guest past the 5 % line to see the
  SIGKILL path, and none ran with `CANG_GUEST_RESPONSIVE=0` as a live control - the control here is
  note 01's arm 4, taken before the responder existed.
- **earlyoom's full log is not retained.** `/run/cang/earlyoom.log` lives in the guest and dies with
  the task dir; what survives is the two quoted slices in `out/results.txt` (the boot banner and the
  900 bytes around the kill). The counters around those quotes are the sampler's, and they are
  complete.
- **The image built this session is one of several on this host.** `podman images` shows several
  `localhost/cang` tags from earlier arms; `nixone` now points at `3f2be435d370`, and the older
  `d755ca7fe741` (note 09's image) is still present untagged. Nothing was pruned.

## Instrument defects found in the arm kit (reported, not patched)

1. **`sample-guest.sh:25` writes no `--- SAMPLE` marker under this image's `sh`.**
   `printf '--- SAMPLE %s\n'` is rejected - twice per tick, verbatim
   `/workspace/sample-guest.sh: line 25: printf: --: invalid option` and
   `printf: usage: printf [-v var] format [arguments]` - because the format starts with `--`
   (this image's `/bin/sh` is `bash-interactive-5.3p9`, which rejects it in POSIX mode). Result:
   `out/samples.txt` has **no timestamps at all** (210 blocks, `--- PROCS` and the counters,
   nothing else), so blocks can only be anchored by the payload's own markers, and
   `host/cang-session.log` is 430 lines, 428 of them that error. The kit's README says this trap
   was already fixed; it is not in this copy. One-line fix would be
   `printf '%s\n' "--- SAMPLE $(date -u +%s.%N)"`.
2. `sample-guest.sh`'s per-process `read` also prints `…/proc/<pid>/comm: No such file or directory`
   for processes that vanish between glob and read - cosmetic, but it is most of the session log.
3. The driver's done-sentinel loop logs `cang launcher exited after N s with no done sentinel`
   immediately before `done sentinel: done` (both arms): the launcher exits on the payload, and the
   loop's `kill -0` check fires before its 2 s sleep sees the file. Harmless, but it reads like a
   failure in the log.

## Raw artifacts

`.scratch/responsive-arms/`:

| path | holds |
|---|---|
| `arm13a/host/driver.log`, `out/results.txt`, `out/storm.json`, `out/build.log` | claim A: gate, launch line, every read-back, per-CPU fractions, cargo's own 1067 `Compiling` lines and `Finished … in 4m 19s` |
| `arm13a/host/vmm/arm13a-*.helper.stderr.log` | the VMM console copy (vsock noise only), `guest-kernel-console.log` 0 bytes |
| `arm13a2/out/results.txt`, `out/cpuwatch.json`, `out/cpuwatch.json.txt` | the second boot's read-backs and the per-comm CPU totals |
| `arm13b/out/results.txt` | claim B: the earlyoom slices, the presser's `rc=143`, the exec probe, the final `oom_kill` |
| `arm13b/out/samples.txt` | the 210-block sampler capture (meminfo, vmstat, PSI, zram `mm_stat`/`io_stat`, census) |
| `arm13b/out/snapshots.txt` | PRE / AFTER-KILL / AFTER-HOLD: `mem_used_total`, `failed_writes`, `oom_kill`, the census with `earlyoom -1000` |
| `arm13b/out/presser-exit.txt`, `out/press-progress.txt`, `out/presser.out` | the payload's own anchors and the presser's RSS/VmSwap per chunk |
| `arm13b/host/vmm/arm13b-*.helper.stderr.log` | the console copy quoted above |
| `arm13a*/arm13a*.sh`, `arm13a2/cpuwatch.py`, `arm13a2/storm9.py`, `arm13b/arm13b.sh` | the guest-side arm scripts (the storm telemetry script is arm 09's, copied verbatim) |
| `../arm13/provenance.txt`, `../arm13/build-cang-musl.log`, `../arm13/build-container.log` | the two builds, the image, and the src-vs-worktree sha256 table |
