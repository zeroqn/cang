---
label: wayfinder:note
ticket: 02-kernel-knob-inventory
---

# Which memory and CPU knobs linux-7.3.5 offers this guest, ranked

Authority: the extracted `/nix/store/1pmrvbg56rly33v67d14sghsxcdzp2pw-linux-7.3.5.tar.xz`
(`.scratch/kernel-7.3.5/linux-7.3.5/`) plus the effective config
`deps/libkrunfw/config-libkrunfw_x86_64-kvm`; the submodule's
`tarballs/linux-7.3.1.tar.xz` was not used. `linux-hardened-v7.3.5-hardened1`
(`nix/pkgs/libkrunfw.nix:94-95`) was **not** in the store, so defaults come from
vanilla-7.3.5 C or its `Documentation/`; the config (already carrying the
hardened Kconfig fragment) decides which arms exist.

Failure mode: a rustc storm, only swap is zram at 100 % capacity / 25 %
`mem_limit` (`swap/zram.rs:62,71`), no disk swap, `jobs = nproc - 2`. Arithmetic
assumes 8 GiB and 12.7 GiB `MemTotal` (80 % of host RAM, `resources.rs:134-138`).

## 1. Defaults read off the source

| knob | path | default here | citation | source used |
|---|---|---|---|---|
| `min_free_kbytes` | `vm/min_free_kbytes` | not a constant at boot: `int_sqrt(lowmem_kbytes*16)` clamped 128..262144 kB → **11.3 MiB @8 GiB**, 14.4 @12.7 GiB; the `1024` is only the pre-init placeholder | `mm/page_alloc.c:6768`(fn),`:6774-6777`(formula+clamp),`:276` | initialising C |
| `watermark_scale_factor` | `vm/watermark_scale_factor` | 10 → non-MIN gap = `max(wmark_min/4, 0.1 % of zone)` = 8.2 MiB each @8 GiB (kswapd starts ≈27.7 MiB free) | `mm/page_alloc.c:279`; gap `:6703-6705` | initialising C |
| `watermark_boost_factor` | `vm/watermark_boost_factor` | 15000 = boost capped at **1.5 × WMARK_HIGH** (≈42 MiB @8 GiB), fragmentation path only | `mm/page_alloc.c:278`; cap `:2203-2204`; trigger `:2185-2221` | initialising C |
| `vfs_cache_pressure`+`_denom` | `vm/vfs_cache_pressure`, `..._denom` | 100 / 100 (ratio 1, the documented default) | `fs/dcache.c:76-77`; `Documentation/admin-guide/sysctl/vm.rst:1132` | initialising C |
| `swappiness` | `vm/swappiness` | 60 | `mm/vmscan.c:201`; `vm.rst:1062` | initialising C |
| `page-cluster` | `vm/page-cluster` | **3** (≥16 MB RAM, else 2) = 8-page readahead window per swap-in | `mm/swap_state.c:1036-1044`, decl `:32` | initialising C (`swap_readahead_setup`) |
| `dirty_ratio` / `dirty_background_ratio` | `vm/dirty_ratio`, `vm/dirty_background_ratio` | 20 / 10 → 1.6 GiB dirty allowed, flusher at 0.8 GiB (@8 GiB) | `mm/page-writeback.c:91`, `:74`, arithmetic `:447` | initialising C |
| MGLRU `min_ttl_ms` | `/sys/kernel/mm/lru_gen/min_ttl_ms` (**sysfs**) | **0 = disabled** (zero-init static) | `mm/vmscan.c:4209`, attr `:5447`; `Documentation/admin-guide/mm/multigen_lru.rst:82` | C + doc agree |
| KSM `run` | `/sys/kernel/mm/ksm/run` | 0 (`KSM_RUN_STOP`): built but **off at boot**; `echo 1 > run` enables | `mm/ksm.c:490`; `Documentation/admin-guide/mm/ksm.rst:111-113` | initialising C |
| KSM scan cost | `.../pages_to_scan`, `sleep_millisecs`, `advisor_mode` | 100 pages / 20 ms ≈ 5 000 pages/s ≈ 20 MB/s of *mergeable* anon; advisor `none` | `mm/ksm.c:259,289,292,345` | initialising C |
| THP | `/sys/kernel/mm/transparent_hugepage/{enabled,defrag}` | `enabled=madvise`, `defrag=madvise` (+khugepaged, use_zero_page); shmem/tmpfs THP **never** | `mm/huge_memory.c:61-69`; config `:997-1007` | initialising C from config flags |
| `sched_latency_ns`, `sched_min_granularity_ns`, `sched_wakeup_granularity_ns` | **no such path** | replaced by one `sysctl_sched_base_slice = 700000` ns, scaled `1+ilog2(min(ncpus,8))` → **2.1 ms @4 vCPU** | `kernel/sched/fair.c:85-86`, scaling `:215-225`; the three old names: 0 hits in the tree | initialising C + absence sweep |
| that replacement's interface | debugfs only: `debugfs_create_u32("base_slice_ns",…)`, and **`# CONFIG_DEBUG_FS is not set`** | `kernel/sched/debug.c:724`; config `:3477` | source + config |
| zram `mem_limit` | `/sys/block/zram0/mem_limit` | 0 = unlimited until cang writes 25 % | `drivers/block/zram/zram_drv.c:375-390`, reset `:2868` | initialising C |
| zram `comp_algorithm` | `/sys/block/zram0/comp_algorithm` | `"zstd"` at the zstd default level (built: zstd, lzo only) | `zram_drv.c:45`; config `:1494,1497,1500`; level `backend_zstd.c:64` | C + config |
| zram `algorithm_params` | `/sys/block/zram0/algorithm_params` | unset; accepts `level=`, `priority=`, dict path | `zram_drv.c:1706-1730`, attr `:3019` | initialising C |
| zram `idle` | `/sys/block/zram0/idle` | only literal `all` accepted (numeric age needs `CONFIG_ZRAM_TRACK_ENTRY_ACTIME`, off) | `zram_drv.c:452-486`; config `:1503` | source + config |
| zram `recompress`/`recomp_algorithm`/`writeback*` | exist in the driver, **compiled out** here | `zram_drv.c:1841,2551,3015-3016`; config `:1502,1504` | source + config |
| `overcommit_memory` | `vm/overcommit_memory` | 0 = `OVERCOMMIT_GUESS` | `mm/util.c:794` | initialising C |
| `compaction_proactiveness` | `vm/compaction_proactiveness` | 20 → kcompactd compacts down to 80 % of the low→high gap | `mm/compaction.c:1936`,`:2264`,`:3324` | initialising C |
| PSI | `/proc/pressure/{cpu,memory,io}` | **on**, pollable triggers, window 500 ms..10 s | config `:152-153`; `kernel/sched/psi.c:1357-1379`; `Documentation/accounting/psi.rst:83,102` | config + source |


## 2. Ranked shortlist

Nothing below is shipped or measured; each "buys" is a reading of the source path.

1. **`vm.watermark_scale_factor` 10 → 50..100.** Moves all three non-MIN
   watermarks from 0.1 % to 1 % of the zone (`page_alloc.c:6703-6705`); at 8 GiB
   kswapd starts ≈180 MiB early instead of ≈28. **Buys**: keeps allocations off
   *direct* reclaim — a 4 KiB allocation for fork/exec/a page table reclaiming
   synchronously is exactly the `cang exec` start cost under load (note 32: 397 ms
   p50 / 13.2 s p95). **Costs**: no RAM reserved (thresholds only), but earlier
   and longer kswapd = reclaim CPU and earlier swap-out of reusable pages.
   **Arm**: one boot per value, 13 `rustc` loops, `pgscan_direct`/`pgsteal_direct`
   (`vmstat -n`) plus `/proc/pressure/memory` and the PTY + exec-start
   distributions; proven only if direct-reclaim events *and* p99 fall together.
2. **`vm.min_free_kbytes`** 11.3 → 64/128 MiB @8 GiB: the **only** way to raise
   `WMARK_MIN` (`page_alloc.c:6774-6777`), the reserve direct reclaim and
   `ALLOC_MIN_RESERVE` allocations work against. **Buys**: headroom for bursts
   arriving faster than kswapd reacts (exec, pty fork). **Costs**: 50–120 MiB
   permanently unavailable where RAM is the binding constraint; too high and it
   OOMs early. **Arm**: same storm, 11/64/128 MiB with #1 fixed; `pgscan_direct`,
   `oom_kill`, PTY p99, exec-start p99. Verdict for both: **worth a ticket,
   tested as a pair**.
3. **MGLRU `min_ttl_ms` 0 → 1000..3000.** kswapd refuses to evict the last N ms
   of working set and **OOM-kills instead** (`vmscan.c:4209`, `:4186-4240`,
   `multigen_lru.rst:71-82`). **Buys**: livelock becomes a decisive kill, and with
   `oom_score_adj = -1000` on the supervisor (`process.rs:349,363`) the victim is
   rustc, not the session — the map's survival clause and "resilience first"
   decision in one write. **Costs**: premature OOM kills (lost builds) if set
   beyond what the interactive set needs. **Arm**: storm to the `mem_limit` wall,
   0/1000/3000 ms; time-to-first-OOM, `pgscan`/`pgsteal` rate (thrash signature),
   PTY p99 before the fuse blows, `dmesg` victim. **Verdict: worth a ticket**
   (it also settles ticket 01's ladder).
4. **zram `mem_limit` (25 %) and zstd `level` (`algorithm_params`).** The cap is
   both ceiling and cost (`zram_drv.c:375-390`, refusal `:205-208`): at the cap
   the device stops accepting pages and reclaim falls back to the root path.
   `algorithm_params` (`:1706-1730,1738`) trades ratio for compressor CPU stolen
   from rustc. **Costs**: with 1:1 incompressible pages a larger cap is pure loss
   (the reasoning `swap/zram.rs`'s `mem_limit` comment encodes). **Arm**: read
   `mm_stat` `mem_used_max` vs `mem_limit` under the storm — if the cap is never
   reached both knobs are inert; if it is, sweep 10/25/50 % × level 3 vs 1 on
   time-to-fail and compressor CPU share. **Verdict: worth a ticket — it is
   ticket 06's core.**
5. **`vm.page-cluster` 3 → 0..3.** A zram swap-in decompresses `1<<page_cluster`
   pages (`swap_state.c:804,886`); the default reads 8 to satisfy one fault.
   **Buys**: if the storm's cold pages are scattered, 0–1 trades 8× decompress
   CPU for more faults; if code pages are clustered the default already wins.
   **Costs**: more faults, each smaller; no RAM. **Arm**: one boot per value,
   exec-start + PTY p99 under 13 `rustc` loops *after* ≥1 GiB has been swapped
   out, with `swap_ra`/`swap_ra_hit` (`vmstat -n`) — never on a warm guest.
   **Verdict: worth a small ticket** (cheap, sign unknown).
6. **`vm.dirty_background_ratio` 10 → 3** (and `dirty_ratio` 20 → 10). Dirty
   pages are unreclaimable and 20 % of 8 GiB lets 1.6 GiB be dirty while memory
   is short (`page-writeback.c:447`). **Buys**: the flusher starts at 0.24 GiB, so
   more page cache is clean and freeable exactly when kswapd wants it.
   **Costs**: earlier writeback = I/O CPU during the storm (the workspace is a
   btrfs image, so real work) and worse coalescing. **Arm**: 13 `rustc` loops
   building *into* the workspace; `nr_dirty`/`nr_writeback`, `pgscan_direct`,
   time-to-OOM **and the workload's own wall time** (it can buy memory by costing
   build time). **Verdict: worth a small ticket.**
7. **`vm.swappiness` (60, sweep).** With zram only, both directions lose:
   raising it spends zstd CPU and fills a 25 % budget, lowering it keeps anon
   resident and reaches OOM sooner. MGLRU folds swappiness into anon/file
   eviction (`vmscan.c:2444-2472`), and the new `SWAPPINESS_ANON_ONLY` is
   *proactive-reclaim-only* (`vmscan.c:2562-2567`) so `/proc/sys/vm/swappiness`
   cannot request it. **Buys**: unknown sign; honest prediction is a plateau.
   **Costs**: reclaim CPU, faster budget burn. **Arm**: 0/60/100, same storm,
   time-to-OOM + PTY p99, judged only if the spread exceeds run noise.
   **Verdict: fold into ticket 06's sweep, not its own ticket.**
8. **PSI as input, not a knob.** `/proc/pressure/*` exist with pollable triggers
   (`:152-153`; `psi.c:1357-1379`; `psi.rst:83,102`). **Buys**: the guest finally
   sees its *own* stalls — today only the host reacts to host pressure
   (`components/memory.rs` → `ReclaimMemory` → `vm.drop_caches`). **Costs**: a
   watcher thread and a policy to write and prove. **Arm**: not a knob arm — the
   watcher logs `some`/`full avg10` at 1 s under the storm; the note records which
   threshold would have fired first. **Verdict: worth a ticket** (feeds 01/06).
9. **`vm.compaction_proactiveness` 20 → 0.** kcompactd runs in the background
   whenever the high watermark is crossed (`compaction.c:2264,2269-2273`), i.e.
   during the storm, and with `enabled=madvise` THP there is almost nothing to
   serve. **Buys**: a little CPU, no latency. **Costs**: fewer huge pages for a
   workload that cannot ask for them. **Arm**: `kcompactd0` CPU share on/off +
   `compact_stall`/`compact_daemon_*`. **Verdict: not its own ticket**, one line
   in 06's sweep at most.

## 3. Not worth a ticket, and why

- **`vm.watermark_boost_factor`.** The ticket's premise ("one allocation into a
  reclaim storm") does not hold at this scale: the boost is capped at
  1.5 × WMARK_HIGH (`page_alloc.c:2203-2204`), ≈42 MiB @8 GiB, entered only on
  the fragmentation path (`:2185-2221`), and the guest's single zone is far
  above that path's `4 pageblocks` minimum. Setting 0 removes a tens-of-MB
  wobble, not a storm.
- **`vm.vfs_cache_pressure` / `vm.max_map_count`.** Dentries and inodes are the
  wrong pool to lean on here (anon cannot be reclaimed at the zram cap, the file
  cache is the only freeable one), and rustc's mapping count (65530,
  `include/linux/mm.h:209`) was never observed as a limit. Neither buys latency.
- **KSM.** Built but off (`ksm.c:490`); enabling is `run=1` (`ksm.rst:111-113`).
  It merges only VMAs flagged `MADV_MERGEABLE` (`ksm.c:775-826`,
  `ksm.rst:37,50-58`), and **nothing here marks anything mergeable** — glibc and
  rustc do not, guest-init cannot retro-flag other processes, and Nix-store text
  is already shared read-only file-backed pages, which KSM does not scan. The
  100 pages/20 ms scan (`ksm.c:259,289,292`) would spend CPU and IPIs to find
  nothing. *Arm that would overturn this*: `run=1`, 13 `rustc`, read
  `pages_sharing`/`general_profit` — 0 proves the dismissal instead of assuming
  it.
- **THP `enabled` madvise → always.** rustc's heaps use brk/anon mmap (never
  `MADV_HUGEPAGE`), tail waste and `deferred_split` land exactly when memory is
  short, khugepaged competes for CPU, shmem/tmpfs THP is compiled *never*
  (config `:1000`), and `CONFIG_THP_SWAP=y` (`:1008`) does not help a device that
  compresses per `PAGE_SIZE` (`backend_zstd.c:71`) — whether the folio is split
  before the zram write is **unsettled** and needs `thp_swpout*` from a live
  guest. Do not propose `always` on THP folklore.
- **All three CFS tunables in the ticket.** `sched_latency_ns`,
  `sched_min_granularity_ns`, `sched_wakeup_granularity_ns` **do not exist in
  7.3.5**: zero hits in the tree; the replacement `sysctl_sched_base_slice`
  (`fair.c:85-86`, 2.1 ms @4 vCPU per `:215-225`) has **no procname
  registration** (contrast `sched_cfs_bandwidth_slice_us`, `fair.c:143`), and its
  only interface is debugfs `sched/base_slice_ns` (`debug.c:724`) — dead,
  because `# CONFIG_DEBUG_FS is not set` (config `:3477`). And the interactive
  threads are already nice `-20` (`runtime/priority.rs:11`); note 36 measured
  that working (PTY p99 24.1 → 7.5 ms, max 1 353 → 59 ms, no sample >250 ms).
- **zram `idle`, `recompress`, `recomp_algorithm`, `writeback*`,
  `compressed_writeback`.** In the driver, compiled out here (`zram_drv.c:452-486`
  needs `CONFIG_ZRAM_TRACK_ENTRY_ACTIME` off at config `:1503`;
  `:1841,2551,3015` need `CONFIG_ZRAM_MULTI_COMP` off at `:1504`; the writeback
  block needs `CONFIG_ZRAM_WRITEBACK` off at `:1502`). `idle` accepts only `all`
  and has no consumer. Dead knobs.
- **`vm.overcommit_memory`.** Default 0 = guess (`mm/util.c:794`); 1 makes the
  storm die later (at first touch, via OOM), 2 makes rustc's allocations fail.
  That is ticket 01's failure ladder, not a latency lever — hand it to 01.
- **`vm.max_map_count`** (65530, `include/linux/mm.h:209`): rustc's mapping
  count could reach it, nothing observed does, and raising it buys no latency.
- **Adjacent, non-knob (record only):** preemption mode (`CONFIG_PREEMPT_LAZY`
  under `PREEMPT_DYNAMIC`, `:130-136`) needs a guest boot cmdline (firmware
  plumbing, out of scope); `CONFIG_HZ=250` (`:442`) caps granularity at 4 ms.

## 4. Ranking ticket 06 can decide from

| # | lever | expected value | cost | why here |
|---|---|---|---|---|
| 1 | `vm.watermark_scale_factor` 10 → 50..100 | high, cheap | reclaim CPU, no RAM | only lever that keeps *direct* reclaim off the exec/fork path |
| 2 | `vm.min_free_kbytes` 11 → 64..128 MiB | high | 50–120 MiB RAM, early-OOM risk | raises WMARK_MIN, which nothing else can; composes with #1 |
| 3 | MGLRU `min_ttl_ms` 0 → 1000..3000 | high, decisive | premature OOM kills | livelock → "workload killed, session survives" = the map's own clause |
| 4 | zram `mem_limit` + zstd level | medium-high | ceiling vs CPU, 1:1 blowup risk | decides *when* the wall arrives; ticket 06's core |
| 5 | `vm.page-cluster` 3 → 0..3 | medium, unknown sign | more faults | cheap; only meaningful on a guest that has really swapped |
| 6 | `vm.dirty_background_ratio` 10 → 3 | medium-low | writeback CPU, worse coalescing | manufactures freeable pages at the moment they are wanted |
| 7 | `vm.swappiness` 0/60/100 | low, sign unknown | reclaim CPU, budget burn | no disk swap: both directions lose; measure, do not assume |
| 8 | PSI triggers | policy input, not a knob | a watcher | lets the guest react to its own pressure; feeds 01/06 |
| 9 | `vm.compaction_proactiveness` 20 → 0 | small CPU only | fewer huge pages | one line in a sweep, never a ticket |

For 06: #1–#3 are *stall/threshold* levers, #4–#7 *capacity/cost* levers, and the
25 % zram budget is binding — watermarks only make the wall's approach orderly.

## 5. What this does not establish

No arm was run (no VM here): every "buys" is a source reading, and the hardened
patch's effect on these initialisers is unproven (expected: Kconfig only). Before
an arm trusts §1, read back `/proc/sys/vm/*`,
`/sys/kernel/mm/{ksm,lru_gen,transparent_hugepage}` and `/proc/pressure/*` once in
a live guest and diff.
