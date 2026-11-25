---
label: wayfinder:research
title: What the guest's memory failure ladder actually is
status: closed
blocked_by: []
claimed_by: "pi arm-01 (child, agent, 2026-11-25)"
---

## Question

Before any lever is chosen, establish what happens - in order, with numbers -
between "the guest is comfortable" and "the guest is dead", under a rustc storm
in a guest whose only swap is zram.

Settle each rung on a live guest, not by argument:

1. **The `1.25x` claim.** The device's capacity is 100% of `MemTotal` and its
   `mem_limit` is 25% (`components/swap/zram.rs:62,71`), and the capacity counts
   *uncompressed* bytes. What is the real effective anonymous ceiling? Read
   `MemTotal`, `SwapTotal`/`SwapFree`, `/sys/block/zram0/{disksize,mem_limit,
   comp_algorithm}`, `/sys/block/zram0/mm_stat` (compr_data_size, mem_used_total)
   as a storm grows, and find the point where `SwapFree` stops absorbing pages
   and `mem_used_total` pins at the limit. State the observed compressibility of
   a real build's anonymous pages (rustc heaps, cargo) rather than assuming it.
2. **What the kernel does at the `mem_limit`.** zram refuses the write; does the
   page stay resident, does reclaim retry and then stall in direct reclaim, and
   what does `/proc/vmstat` show (`allocstall_*`, `pgscan_*`, `pswpout`,
   `pgmajfault`)? Is it kswapd thrash or a hard wall?
3. **Who dies, and does the microVM survive it?** Under OOM, the badness score
   picks by RSS and `oom_score_adj`; with the supervisor at `-1000`
   (`process.rs:349,363`) and the workload at `0` (`:352,375`), the workload
   should be the victim. Prove it from `guest-kernel-console.log` and state the
   two launch shapes separately: a **managed session** (supervisor survives, so
   the VM lives) and an **exec launch** (the exec'd process *is* the image
   entrypoint child of libkrun's init, so check what its death does to the VM -
   this is the case where "the guest dies" and "the build failed" are the same
   console line).
4. **Does a storm even reach the wall?** Note 36 saw `MemAvailable` stay at
   6 564-7 418 MB on a 12.7 GiB guest with 13 concurrent `rustc`s. Reproduce a
   representative storm (`CARGO_BUILD_JOBS` at every vCPU, a real workspace
   build) and report peak `MemAvailable`, peak `AnonPages`, peak swap used, and
   PSI `full avg10` in the guest. If the ladder is never reached, say so: that
   changes which tickets matter.
5. **The multi-victim hazard.** A supervisor at `-1000` and *no* other protected
   process means the OOM killer's only candidates are the workload; if the
   workload is the only task left, the kernel panics with "no killable
   processes". Which processes are left when the workload dies, and can that
   panic be reached?

Deliverable: `notes/01-memory-failure-ladder.md` with the rungs, the counters,
raw log paths, and the launch-shape table. Every number from a live guest;
kernel behaviour cited from the pinned `libkrunfw` tarball, not from memory.

## Resolution

Settled on six live guests, 2026-11-25, one at a time, `--mem` 4/2/1, fresh workspace per arm; full
evidence, raw paths and the counters behind every sentence are in
[notes/01-memory-failure-ladder.md](../notes/01-memory-failure-ladder.md). Headlines:

1. **The `1.25x` claim is a special case, not the rule.** The device's capacity is 1× `MemTotal` and
   its budget 0.25×, so the ceiling is `MemTotal·(1 + 0.25·c)` capped at `2× MemTotal` by the
   capacity. Measured `c`: **8.99-9.8** for a real 26-job rustc/cargo build's pages (the only arm
   where they reached zram, `--mem 1`), **1.09-1.25** for synthetic incompressible pages, and
   effectively unbounded for identical zero pages, which zram stores as a fill value with no
   allocation (`zram_drv.c:2302-2305`; arm 1's 8.2 GiB presser cost 16 MiB). Worst case measured:
   **1.31× `MemTotal`** at `c` = 1.25, `MemAvailable` 8 MB.
2. **At the budget the kernel does not fall back gracefully.** `mem_used_total` pins at
   `mem_limit` = 983 MiB on a 4 GiB guest while `failed_writes` reaches **175 997 refused page
   stores** and swap *slots* drain to 3738 MB of a 3932 MB device — the refused page keeps its slot
   (`page_io.c:499-506`), so the address space runs out ~3× faster than the memory budget does.
   One second of the wall: 2.2 M scan attempts, 3.6 M swap write attempts, 2.8 k pages stolen
   (~780:1), PSI `full avg10` 0 → 2.53 → 12.58, then an OOM kill.
3. **Who dies: the biggest *swapent* holder, not the biggest RSS.** The victim held 38 MB resident
   and 3.6 GB swapped (`oom_badness` = rss + swapents + pgtables, `oom_kill.c:228-229`).
   Managed session (`--mem 4`): the workload died, **the microVM survived**. Exec listener: see 5.
4. **A storm does not reach the wall.** A full 26-job build of the repo's own 2401-crate workload
   completes at 4 GiB (47.4 s), at 2 GiB (1 m 25 s) and at 1 GiB (1 m 03 s) with `PSI full avg10`
   0.00 / 1.59 and swap either untouched (2 GiB, 4 GiB) or 74 MB (1 GiB). The ladder is only
   reachable by an anonymous consumer that outgrows `MemTotal`, so "which lever matters" is about
   the guest's response to *that*, not about build parallelism.
5. **The multi-victim hazard is reachable, and `cang exec` is how**: every command the exec listener
   starts inherits the session supervisor's `oom_score_adj -1000` (`exec.rs` resets pgid, nice and
   credentials but never calls `allow_workload_kill`, which has one caller,
   `session.rs:1526`). Measured: the 3.6 GB presser ran at -1000, the OOM killer took the 7 MB
   `sleep` instead, and 25 ms later the guest hit
   `Kernel panic - not syncing: System is deadlocked on memory` — the microVM died.
   **This is a defect for [08](08-guard-guest-init-processes.md)** (and it blocks the map's
   survival goal for anything run through `cang exec`), not a knob question.

Follow-ups this note hands over: zram geometry (the capacity is 4× what the budget can hold at
`c ≈ 1`) → [06](06-zram-and-swap-tunables.md); the ~2 s guest-local PSI window and the
"count swapents, not RSS" requirement → [11](11-earlyoom.md); the missing
`allow_workload_kill()` on the exec path → [12](12-exec-inherits-oom-protection.md), which carries
the fix, the VM-free test with its failing control, and the live re-arm.
