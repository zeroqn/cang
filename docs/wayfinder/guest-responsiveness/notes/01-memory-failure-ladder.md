---
label: wayfinder:note
ticket: 01-memory-failure-ladder
---

# The guest's memory failure ladder, measured: a build storm never reaches it, an anonymous presser does, and zram's refusal arrives as a *slot* wall, not a memory wall

Seven live cang boots (six arms; the probe arm ran twice), one VM at a time, 2026-11-25
04:45-05:16Z on this host (28 CPUs, 17.9 GiB RAM, no host swap). Every number below was read from a guest this session ran; the raw capture
for each arm is a `samples.txt` written by a 0.5 s in-guest sampler (verbatim `/proc/meminfo`,
`/proc/vmstat`, `/proc/pressure/memory`, the zram device's own counters and a `/proc` census of
every process with its `oom_score_adj`) plus the guest's own logs, all under
`.scratch/ladder01/`.

## 0. What was run, and with what

| | |
|---|---|
| host CLI | `/nix/store/x9dflild0m1ppfwl4l4pdhzhzdsa0zk3-cang-0.23.6/bin/cang` (`result/bin/cang`) |
| image | `localhost/cang:nixone` |
| guest init | `/nix/store/az5idng9kg2caiwxh0rmcd41y0awl5a6-cang-static-x86_64-unknown-linux-musl-0.23.6/bin/cang-guest-init`, built 04:36:32Z from worktree HEAD `a318ba3b` **plus** the uncommitted guest-init edits in `crates/cang-guest-init/src/guest_init/{process.rs,runtime/mod.rs,runtime/session.rs}` (mtimes 04:17-04:22Z). That the OOM guard is in the bytes that booted was checked against the binary itself: `grep -ac` finds `guest swap worker` (3), `guest /nix overlay prep worker` (1), `guest session supervisor` (1), `guest workload OOM score not restored` (1). |
| guest kernel | `7.3.5-hardened1` (`uname -r`, `arm0/out/guest.log`) |
| guest tools | `rustc 1.99.1`, `cargo 1.99.1`, 26 vCPUs (host-derived), `dev` uid 1000 for every workload |
| storm | `tools/rust-compile-bench/gen-workload.sh` tree: 2401 crates / 19204 files, `cargo build --offline` with `CARGO_BUILD_JOBS=$(nproc)` = 26 — the shipped default, since nothing in `crates/` or `nix/` sets a job count |

Kernel behaviour below is cited from
`/nix/store/1pmrvbg56rly33v67d14sghsxcdzp2pw-linux-7.3.5.tar.xz` (extracted files kept beside the
evidence as `.scratch/ladder01/host/{oom_kill.c,zram_drv.c,page_io.c}`), not from memory.

The arms:

| arm | launch shape | `--mem` | workload | what it settles |
|---|---|---|---|---|
| 0 | managed session | 2 | none (10 s sampler, rustc check) | plumbing + environment readback (`arm0/`) |
| 1 | managed session | 4 | 45 s build storm + **presser v1** (identical zero pages) | a zero-page presser measures nothing (rung 0 of the method) |
| 2 | **exec listener** (`cang exec`) | 4 | presser v2 | who dies when the workload is exec'd |
| 3 | managed session | 2 | build storm only, 120 s window | does a build storm reach the wall |
| 4 | managed session | 4 | 45 s build storm + **presser v2** (distinct pages) | the wall, the refusal, the victim, the ceiling |
| 5 | managed session | 1 | build storm only | a real build's own pages in zram: the ratio `c` |

Each arm booted its own VM, in its own workspace directory, with its own workspace tree
(`.scratch/ladder01/armN`), gated on `pgrep -af libkrun-vm-worker-enter` finding no
foreign VM and on `cang ps` showing no running task for that workspace
(`.scratch/ladder01/host/run-arm.sh`). Every arm's gate line reads
`gate foreign_workers=0 pgrep_x_cang=0` (`grep 'gate ' arm*/host/driver.log`), and the arms ran
strictly one after another (each arm's `END arm=…` line precedes the next arm's `gate …` line; the
first appeared at 05:23:36Z, nine minutes after arm 5 ended, and was not touched). Arm 0 ran twice:
its first boot found two sampler bugs — `dash`'s `printf` rejects `--`, and the workload uid cannot
read the per-field zram sysfs (`mem_limit`, `mem_used_max` are root-only; `mm_stat` is not) — so its
evidence was replaced by the fixed run and the first boot is not quoted anywhere below. The launcher
process was `script -q -e -c "<cang …>" /dev/null` for the managed-session arms, so stdin was a
terminal and the guest ran the shipped managed-session path (supervisor at `oom_score_adj`
-1000, PTY child at 0 — verified from the guest's own `/proc`, see §6).

Device geometry, read in the guest (`arm4/out/status-at-start.txt`, `/run/cang/swap.status`; the
same numbers in `/sys/block/zram0/mm_stat`):

```
state=ready
device=/dev/zram0
size_bytes=4123996160          # 3932 MiB = 100 % of MemTotal  (zram.rs:62)
mem_limit_bytes=1030999040     #  983 MiB =  25 % of MemTotal  (zram.rs:71)
priority=100
```
and `comp_algorithm` = `lzo-rle lzo [zstd]` — zstd is the active compressor
(`arm0/out/status-at-start.txt`).

## 1. Rung 0-1: a full 26-job rustc build never touches zram, and never makes the kernel reclaim anon

Arm 3: `--mem 2` (MemTotal 2019 MB, SwapTotal 2019 MB), the 2401-crate build at 26 jobs, no presser.

* the build **completed**: `Compiling` lines 2401, `Finished dev profile [unoptimized] target(s) in 1m 25s`, `rc=0` (`arm3/out/build.log`)
* peak `AnonPages` 1176 MB, peak `MemAvailable` floor 512 MB, **peak swap used 0 MB**
* `/proc/pressure/memory` `full avg10` = **0.00 for every one of 142 samples** (`arm3/out/samples.txt`)
* `/proc/vmstat` deltas over the whole arm: `pgscan_direct` 0, `pgscan_kswapd` 0, `pgsteal_*` 0, `pswpout` 0, `allocstall_*` 0, `oom_kill` 0. The only deltas were `pgmajfault` +13741 and THP activity (`thp_fault_alloc` +53324, `thp_split_pmd` +26220) — i.e. the build faults pages in and never asks the kernel for memory back.

Arm 5 repeats it at `--mem 1` (MemTotal 1014 MB) — 26 rustcs, peak `AnonPages` 714 MB against a
1 GiB guest — and the build **still completes** (`Finished … in 1m 03s`, rc=0,
`arm5/out/build.log`): peak `AnonPages` 714 MB, `MemAvailable` floor 82 MB, peak `PSI full avg10`
**1.59**, peak swap used 74 MB, `pgscan_direct` +54276, `pgsteal_direct` +34115, `pswpout` +22283,
`failed_writes` 0 (`arm5/out/samples.txt`). This is the only arm where the *build's own* pages
reached zram, and it is where the real compression ratio comes from (§7).

**So the ladder's first rungs are a file-cache story, not a swap story**: the kernel hands cargo
and rustc their pages by reclaiming cache, silently, and PSI stays at zero. A representative storm
— 26 rustcs, every vCPU, the shipped default — does **not** reach the wall at 4 GiB, at 2 GiB, or
even at 1 GiB. Note 36's observation (`MemAvailable` 6 564-7 418 MB on a 12.7 GiB guest) is the same
phenomenon with more headroom; the interesting consequence is the other way round from what the map
assumed: *the guest's memory failure ladder is not reachable by a build storm at all.*

## 2. Rung 2: what it takes to get to the wall, and the first thing that went wrong on the way

Arms 1 and 4 add an anonymous presser (`presser.rs`, compiled in-guest with `rustc -O`), because a
build storm cannot supply the pressure. Arm 1's presser filled every page with zeros: `vec![0u8; n]`
plus one touched byte per page leaves the whole page zero — and it reached 8208 MiB of logical
anonymous memory with **no OOM kill at all**, on a 4 GiB guest, with `mem_used_total` never above
16 MiB (`arm1/out/samples.txt`, `presser`: `PRESS DONE alloc_mb=8208`). The reason is in the pinned
zram driver:

```
zram_drv.c:2302   same_filled = page_same_filled(mem, &element);
zram_drv.c:2304   if (same_filled)
zram_drv.c:2305           return write_same_filled_page(zram, element, index);
zram_drv.c:2232   /* write_same_filled_page: slot flag ZRAM_SAME + the fill value; no pool allocation */
```

A page whose every byte is identical is stored as a flag plus one fill value and costs zram nothing
(`same_pages` 11080 in arm 1). **A zero-fill presser is therefore not a memory test**; it exhausts
the swap *address space* (arm 1: 3908 MB of swap slots in use for 119 MB of stored pages) without
ever spending the compressed-memory budget. Arm 4 replaced it with a presser that gives every page
its own xorshift32 byte stream: distinct, essentially incompressible pages — the worst case for a
zram budget, and the case where `mem_limit` actually binds.

## 3. Rung 3: the wall itself, in 3.5 seconds (arm 4, `--mem 4`)

Build storm for 45 s (it finished at 47.4 s: `Finished dev profile [unoptimized] target(s) in 47.38s`,
`arm4/out/build.log`), then the presser from t≈47.5 s. `t` is seconds after the first sample.


| t (s) | MemAvailable MB | AnonPages MB | swap used MB | zram stored MB | zram compressed MB | mem_used_total MB | oom_kill | PSI full avg10 | pgscan_direct | pgsteal_direct | pswpout |
|---|---|---|---|---|---|---|---|---|---|---|---|
|  60.9 | 1011 | 2600 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  61.7 | 936 | 2676 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  62.8 | 872 | 2744 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  64.4 | 796 | 2814 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  65.3 | 769 | 2923 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  65.9 | 525 | 3160 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  66.6 | 272 | 3396 | 0 | 0 | 0 | 0 | 0 | 0.00 | 0 | 0 | 0 |
|  67.2 | 96 | 3606 | 0 | 0 | 0 | 0 | 0 | 0.00 | 4346 | 1551 | 0 |
|  68.2 | 16 | 3172 | 564 | 611 | 559 | 560 | 0 | 2.53 | 87597 | 83972 | 146552 |
|  69.5 | 8 | 198 | 3738 | 1222 | 982 | 983 | 0 | 2.53 | 970912 | 172986 | 2209659 |
|  70.7 | 3711 | 10 | 25 | 18 | 1 | 2 | 1 | 12.58 | 3170803 | 175800 | 5769355 |
|  71.4 | 3567 | 19 | 25 | 18 | 1 | 2 | 1 | 12.58 | 3170803 | 175800 | 5769355 |
|  72.0 | 3692 | 19 | 25 | 18 | 1 | 2 | 1 | 10.84 | 3170803 | 175800 | 5769355 |

Reading it in order (`arm4/out/samples.txt`; the wall sample is line 42398):

* **t=67.2** — `MemAvailable` 96 MB, `AnonPages` 3606 MB, swap still 0, `pgscan_direct` 4346: the
  guest has spent the page cache and started scanning, with nothing in swap yet. `Cached` is already
  6 884 kB in that sample (its `Cached:` line) — there was no reclaimable file memory left.
* **t=68.2** — first swap. Stored 611 MB uncompressed for 559 MB compressed: `c` = 1.09. `full avg10`
  leaves 0.00 for the first time (2.53).
* **t=69.5** — `mem_used_total` **= 1031000064 = `mem_limit` exactly**, for the first time, and
  `mem_used_max` 1031028736 (one page over, which is what `zram_can_store_page`'s `<=` allows:
  `zram_drv.c:201-209`). Stored 1222 MB, compressed 982 MB. Meanwhile **swap slots are at 3738 MB of
  a 3932 MB device** — three times what zram is actually holding.
* **t=69.5 → 70.7** — one second in which the kernel made 2 199 891 more page-scan attempts
  (`pgscan_direct` 970 912 → 3 170 803), issued 3 559 696 more swap write attempts
  (`pswpout` 2 209 659 → 5 769 355), and stole 2 814 more pages (`pgsteal_direct` 172 986 →
  175 800). A scan:steal ratio of roughly 780:1 in that second. This is the hard wall: reclaim runs
  and gets nothing.
* **t=70.7** — `oom_kill` increments to 1, `AnonPages` falls to 10 MB, swap drains to 25 MB: the OOM
  killer took the presser, and the guest recovers immediately.

The kernel's own console, captured through the VMM's console passthrough
(`arm4/host/taskdir/arm4-79321-1795582986412610062.helper.stderr.log`):

```
[05:04:18.761222Z ERROR init_or_kernel] [   70.454148] Write-error on swap-device (253:0:2187264)
… (ten such lines in the captured copy; the console line is rate-limited, `pr_alert_ratelimited`, `page_io.c:538`)
[05:04:20.389761Z ERROR init_or_kernel] [   72.082553] Out of memory: Killed process 22619 (presser) total-vm:10493096kB, anon-rss:38716kB, file-rss:12kB, shmem-rss:0kB, UID:1000 pgtables:8020kB oom_score_adj:0
```

The victim is striking and is the clearest single finding of the ladder: the presser had **38 MB of
resident anon** when it was killed — its 3.6 GB was in swap — and the kernel still picked it, because
`oom_badness` scores `rss + swapents + pgtables` (`oom_kill.c:228-229`). A process that has been
swapped out is *more* of a target, not less.

## 4. What "the device refuses" actually costs: 175 997 refused stores, and a swap device that fills with nothing

Arm 4's zram counters at the wall, quoted from the device:

```
arm4/out/samples.txt:42398  zram0:mm_stat 1282273280 1029930513 1031000064 1031000064 1031028736    58254        0   251003   251003
                            orig_data_size compr_data_size mem_used_total mem_limit mem_used_max same_pages pages_compacted huge_pages huge_pages_since
arm4/out/samples.txt:42891  zram0:io_stat failed_reads failed_writes notify_free =        0   175997 0  1006614
```

`failed_writes` = **175 997 pages (687 MB) refused**; `notify_free` = 1 006 614 slots freed at that
sample (1 006 629 by the end of the arm). The
refusal path is `zram_write_page()`: it compresses, allocates, and only then checks the budget —

```
zram_drv.c:2324   handle = zs_malloc(zram->mem_pool, comp_len, GFP_NOIO | __GFP_NOWARN | __GFP_HIGHMEM | __GFP_MOVABLE, …)
zram_drv.c:2332   if (!zram_can_store_page(zram)) { zcomp_stream_put(zstrm); zs_free(…); return -ENOMEM; }
zram_drv.c:2778   if (zram_bvec_write(…) < 0) { atomic64_inc(&zram->stats.failed_writes); bio->bi_status = BLK_STS_IOERR; break; }
```

and the kernel's response to a failed swap write keeps the page *and* its swap slot:

```
page_io.c:499-506   if (failed) { set_page_dirty(page); ClearPageReclaim(page); } end_page_writeback(page);
```

`PSWPOUT` is counted at submission, before the outcome (`page_io.c:381-382`), which is why `pswpout`
(5 769 355) is ~2.9× the number of slots ever in use (1 006 629 freed + ~957 000 live) — those are
retries of a write that cannot succeed. There is no feedback loop from "the store is full" to "stop choosing anon pages": the
reclaimer keeps selecting anon, the write keeps failing, and the page stays dirty in the swap cache
holding its slot. The device's *address space* therefore drains at ~3× the rate its *memory budget*
does, and the guest hits "swap full" (`SwapFree` 199 MB of 3932) while zram holds 983 MB — exactly
its configured cap — of the 3932 MB the device claims to offer.

The `MEM_LIMIT_PERCENT` comment in `crates/cang-guest-init/src/guest_init/components/swap/zram.rs:65-71`
says "once the cap is reached the device refuses further pages and the kernel falls back to its
normal reclaim behaviour". This arm is what that behaviour is: ~1 s of unproductive scanning, then
an OOM kill. It is not a graceful degradation, and the slot leak means the guest has less usable
swap than `SwapTotal` advertises at exactly the moment it matters.

## 5. Who dies, and does the microVM survive it

| arm | launch shape | victim (console line) | other killable processes at that instant | microVM |
|---|---|---|---|---|
| 4 | managed session (`cang … -- sh /workspace/storm.sh` under a PTY) | `Killed process 22619 (presser) … oom_score_adj:0` | 4 processes at `oom_score_adj` 0 with any RSS at all: the presser (24 MB), two `sh`, `init.krun`; 8 processes at -1000: five named `cang-guest-init`, `nix-daemon`, and the `timeout`/`nix` pair under it — i.e. the session supervisor, the detached workers, and what those workers spawned | **survived**: the guest kept running for 5 s after the kill, finished its script, `cang` exited on its own, `cang ps` empty (`arm4/host/driver.log`) |
| 2 | exec listener (`cang exec <task> -- sh /workspace/exstorm.sh` inside a live session) | `Killed process 730 (sleep) … oom_score_adj:0` — **the session payload, not the 3.6 GB presser** | 11 processes at -1000, including **`proc 980 presser -1000`** and the two `sh` scripts that started it | **died**: `Kernel panic - not syncing: System is deadlocked on memory` 25 ms later |

Arm 4's census, quoted from `arm4/out/samples.txt` (the sample at guest-relative t=69.5 s):

```
proc 22619 presser 0 263610 18619 …
# and, at -1000: 710 cang-guest-init, 723/724/725 cang-guest-init, 23252 nix-daemon,
#                23482 timeout, 23483 nix, 23485 cang-guest-init
```

## 6. Rung 5: the multi-victim hazard is reachable today — `cang exec` hands its children the supervisor's -1000

Arm 2 is the same presser, in the other launch shape: a managed session whose payload is a
`sleep`, and all the memory pressure coming through `cang exec`. The guest's own census
(`arm2/out/samples.txt`, and the same process table in `arm2/out/exstorm-status-at-start.txt`) is
unequivocal:

```
proc 730 sleep    0 264702 1891     # session payload: score 0, killable
proc 745 sh   -1000 264094 2056     # the exec'd exstorm script
proc 829 sh   -1000 …               # the sampler it started
proc 980 presser -1000 263626 62661 # the 3.6 GB anonymous consumer
```

and the console tells the rest (`arm2/host/taskdir/arm2-89036-1795583249589076714.helper.stderr.log`):

```
[05:07:46.952119Z ERROR init_or_kernel] [   15.339670] Write-error on swap-device (253:0:2224128)
…
[05:07:48.069982Z ERROR init_or_kernel] [   16.457430] Out of memory: Killed process 730 (sleep) total-vm:1058808kB, anon-rss:0kB, file-rss:12kB, shmem-rss:0kB, UID:1000 pgtables:72kB oom_score_adj:0
[05:07:48.094434Z ERROR init_or_kernel] [   16.482150] Kernel panic - not syncing: System is deadlocked on memory
```

and `cang exec` reported `cang: cang exec connection closed before exit status`
(`arm2/host/cang-exec-storm.log`), with no task left in `cang ps` afterwards.

The mechanism is one missing call. The managed session's PTY child resets its score before running
the workload:

```
crates/cang-guest-init/src/guest_init/runtime/session.rs:1522-1526
        // The workload subtree inherits the supervisor's OOM score, so it has to
        // stay killable: the guest needs a victim for the kernel to pick.
        process::allow_workload_kill();
```

The exec listener's child does not — its `pre_exec` resets the process group, the nice level and the
credentials, and nothing else:

```
crates/cang-guest-init/src/guest_init/runtime/exec.rs:143-165
    let mut command = Command::new(&argv[0]);
    …  command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 { … }
            if libc::setpriority(libc::PRIO_PROCESS, 0, 0) != 0 { … }
            if process::is_root() { process::apply_dev_credentials(&identity, permissions)?; }
            Ok(())
        });
```

`allow_workload_kill` has exactly one caller in the crate and `exec.rs` is not it
(`grep -rn allow_workload_kill crates/cang-guest-init/src` → `session.rs:1526`, its definition at
`process.rs:392`). So every command launched through `cang exec` inherits the supervisor's -1000 and
becomes unkillable, which inverts the guard's purpose: the OOM killer skips the real consumer, takes
whatever is left at score 0 (here a 7 MB `sleep`), and then, with that mm marked as an OOM victim and
nothing else eligible, reaches

```
oom_kill.c:1159-1166   if (!oc->chosen) { dump_header(oc); pr_warn("Out of memory and no killable processes...\n");
                       if (!is_sysrq_oom(oc) && !is_memcg_oom(oc)) panic("System is deadlocked on memory\n"); }
```

`is_global_init` protects pid 1 (`oom_kill.c:161-162`), so the only candidates were the score-0
processes — and the exec'd work was not among them. **The "no killable processes" panic is reachable
on the shipped tree, in about 16 guest-seconds, by a `cang exec` command that uses memory.** This is
the answer to question 5, and it is a defect rather than a hypothetical: the fix belongs with ticket
[08](../tickets/08-guard-guest-init-processes.md), and until then the map's "resilience first"
destination is unreachable through `cang exec`.

## 7. The effective ceiling, and `c`

The device's two limits compose as `ceiling = MemTotal + min(0.25·c, 1)·MemTotal`, where `c` is the
ratio `orig_data_size / compr_data_size` zram itself reports, because the capacity (1× MemTotal) and
the memory budget (0.25× MemTotal) are both fractions of the same `MemTotal` and a stored page costs
`1/c` of its size.

| data | `c` (orig/compr, from `mm_stat`) | where |
|---|---|---|
| a real 26-job rustc/cargo build's pages | **8.99-9.8** (peak sample: 72.02 MiB `orig_data_size` for 8.01 MiB `compr_data_size`; earlier, 63.7 MB for 6.5 MB) | `arm5/out/samples.txt:16046` (peak) and `:9822`, `--mem 1` |
| synthetic incompressible pages (worst case) | **1.09-1.25** (611/559; 1222/982) | `arm4/out/samples.txt:42398` and the sample before it |
| identical zero pages | unbounded — no allocation at all | `arm1/out/samples.txt` (`same_pages` 11080, `mem_used_total` 16 MB) |

So:

* **for incompressible data the ceiling is 1.31× MemTotal** (arm 4: 3933 MB of RAM + 1222 MB stored =
  5155 MB, reached at `MemAvailable` 8 MB), which is barely above the crate's `1.25×` shorthand and
  is the true worst case;
* **for a real build's pages the memory budget is nowhere near binding** — even on a 1 GiB guest the
  build used 9 MB of its 253 MB cap — and what binds is the device *capacity* (1× MemTotal, i.e.
  2× MemTotal of compressible anon), which a build storm never approaches either (§1);
* the `1.25×` claim in the map is right only for `c ≈ 1`; the honest statement is
  `MemTotal·(1 + 0.25·c)` with `c` measured, and a cap of `2× MemTotal` from the device size.

## 8. What could not be measured, and what this note does not claim

* **`dmesg` inside the guest is closed to the workload**: `kernel.dmesg_restrict` = 1
  (`arm0/out/guest.log`) and the workload is uid 1000, so OOM text was read from libkrun's console
  passthrough in `helper.stderr.log`, not from the guest. `guest-kernel-console.log` (the file the
  managed session names in `launch.conf`) was **0 bytes in every arm** — the kernel console lines
  arrive in `helper.stderr.log` with an `init_or_kernel` target. Both files were copied while the VM
  lived (1 s copier) and again from the `--preserve-debug` task dir after it exited.
* **`failed_writes` was not sampled in arm 1** (the sampler grew an `io_stat` read afterwards), so
  arm 1's refusal count is inferred from the slot/store gap, not read. Arm 4 reads it directly.
* **The presser's own `VmSwap` readback is not usable for arithmetic**: in both presser arms
  `VmRSS + VmSwap` exceeded the presser's own logical total by ~10 % (`arm4/out/press-progress.txt`,
  last line: `alloc_mb=3648 rss_kb=27108 vmswap_kb=3992536`). The ceiling arithmetic above uses
  `/proc/meminfo` and `mm_stat` instead.
* **A build storm never reached the wall in any guest size tried** (4 GiB, 2 GiB, 1 GiB: §1). The
  wall in arm 4 was reached by the presser, and in arm 2 by the presser through `cang exec`. Whether
  some larger or differently-shaped rustc workload (many `cargo build` processes, a huge single
  crate, `-j` unbounded *and* a page-cache-starved guest) reaches it is **not** settled here — this
  workload, which is the repo's own bench workload at the shipped default, does not.
* One boot per arm, one VM at a time; no arm was repeated, so there is no within-arm spread for the
  counters. The `full avg10` peaks are 10 s exponential averages read after the event and are
  reported with the timestamps they were read at.
* Host state moved under the campaign: another session committed `workspace-relay: detach a held
  mount at exit …` (HEAD `a318ba3b` → `4e39708f`) mid-run. Nothing it touched is on the guest memory
  path; the guest-init binary was built before it and its identity is recorded in §0.
* Leftovers: the per-arm workspace trees keep only the guest-written evidence
  (`.scratch/ladder01/armN/{out,host}`); the staged crate trees and their `target/` dirs were removed
  after the runs. Every arm ran with `--preserve-debug`, so `/home/dev/disp/cang/arm{0..5}/tasks/`
  still holds each task dir (`guest-kernel-console.log`, `helper.stderr.log`, `launch.conf`, rootfs
  snapshot). They appear to cost no extra space — `/dev/sdb` reported 121G used / 23G free before the
  first boot and the same after the last — but they are cang state-root content, so they are named
  here and left in place rather than deleted.

## 9. The numbers tickets 06 and 11 most need

* **Ticket 06 (zram geometry)**: at `--mem 4`, the memory budget pins at `mem_used_total` =
  `mem_limit` = **983 MiB** after storing **1222 MB** of incompressible pages (`c` = 1.25), and in
  doing so **refuses 175 997 page stores** while the swap device's own address space drains to
  **3738 MB of a 3932 MB device**. The two limits are 4× apart in what they can hold at `c ≈ 1`, so
  the capacity should be sized from the budget and `c`, not from `MemTotal`; that is the single
  geometry fact the ticket needs. Secondary: `mem_used_max` overshoots the cap by one page
  (`zram_drv.c:201-209`), and a zero-page workload never spends the budget at all (`same_filled`,
  `zram_drv.c:2302-2305`).
* **Ticket 11 (earlyoom)**: the warning window at the wall is **~2 s wide and PSI-shaped**: `full
  avg10` 0.00 at `MemAvailable` 96 MB, **2.53** one second later at the first refused store, and
  **12.58** (read just after the kill) 2 s after that, with `MemAvailable` at 8 MB and `Cached` already
  down to 6.9 MB. An early responder has to act on a *guest-local* signal in that window — nothing
  host-side (`components/memory.rs` reacts to host pressure only) will see it. Two design
  constraints fall out of the arms: it must count a process's **swapents**, not its RSS, since the
  kernel's own victim had 38 MB resident and 3.6 GB swapped (`oom_badness`, `oom_kill.c:228-229`);
  and it cannot rescue the `cang exec` case at all while exec'd work sits at -1000 (§6) — earlyoom
  skips `oom_score_adj -1000` by design.
