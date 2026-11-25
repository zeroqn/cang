---
label: wayfinder:grilling
title: Decide the zram geometry and the swap-related sysctls
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25, delegated by bob)"
---

## Question

The device is sized at 100% of guest RAM with `mem_limit` at 25%
(`components/swap/zram.rs:62,71`), priority 100 (`:56`), zstd only
(`config-libkrunfw_x86_64:1501`), and no swap sysctl is set anywhere. With the
ladder from [01](01-memory-failure-ladder.md) and the arms from
[05](05-response-arm-kit.md), decide the numbers, with bob:

1. **Capacity vs budget.** 100% / 25% means a hard-ish ceiling near `1.25x`
   RAM. Is a larger budget better (more compressible pages parked, less OOM) or
   worse (RAM spent on the store, less for the build)? What did the measured
   compressibility say? Note the capacity costs ~1.4 s of CPU to allocate
   metadata for in a 12.7 GiB guest - does a smaller capacity buy that back?
2. **`vm.swappiness`.** Every swapped page costs compression CPU on a device
   that is itself RAM. For a zram-only guest: high (swap aggressively, keep
   cache), low (avoid compression CPU), or the default 60? Refer to the arm's
   numbers, not the folklore about disk swap.
3. **`vm.min_free_kbytes` / `vm.watermark_scale_factor` /
   `vm.watermark_boost_factor`.** Pinch points are latency (direct reclaim in
   the interactive path) versus usable memory (watermarks are unavailable RAM).
   Which of these is worth writing, and at what value?
4. **Tiny working set vs many-**`rustc`**- heaps:** measure whether KSM or THP
   changes belong here at all, or stay in
   [02](02-kernel-knob-inventory.md)'s "not worth a ticket" list.
5. **What a parked page costs.** With zram, compression runs in the context
   that writes the page (`zcomp_compress`, `zcomp.c:142`; `zram_submit_bio` is
   synchronous), so a page parked by `kswapd` costs that CPU a zstd run. Decide
   whether `algorithm_params level=` (a lower level trades ratio for per-page
   CPU) belongs with the geometry, given the arms.
6. **The trade in one sentence** for the docs: what does the guest give up for
   what it buys? If the answer is "nothing measurable", say that instead.

Start from the ranked shortlist in
[../notes/02-kernel-knobs.md](../notes/02-kernel-knobs.md) (`watermark_scale_factor`,
`min_free_kbytes`, MGLRU `min_ttl_ms`, then this ticket's own
`mem_limit`/zstd `level`, `page-cluster`, dirty ratios, swappiness sweep), and
note there that the three CFS tunables are not settable at this pin.

Resolution = the chosen numbers with the arm that justifies each, recorded in
`notes/06-zram-and-swap.md`, and the exact constant changes handed to
[10](10-ship-and-docs.md).

## Resolution

Closed 2026-11-25 with bob's delegation. **The decision is: change nothing, and
say why with the numbers.** No geometry change, no swap sysctl, no zstd level.
The one candidate worth an arm is named below with the arm it needs.

### Capacity and budget stay at 100% / 25% of `MemTotal`

The two limits are not interchangeable, and the ladder measured both sides:

| workload | `c` (orig/compr) | which limit binds | what it allows |
|---|---|---|---|
| a real 26-job build (`--mem` 1) | **8.99-9.8** | neither; the device never leaves `orig_data_size` 72 MiB of a 253 MiB budget | the build completes with 74 MB of swap used ([01](01-memory-failure-ladder.md) §1, §7) |
| incompressible pages | **1.09-1.25** | `mem_limit` exactly (`mem_used_total` = 1031000064 at `--mem` 4) | 1222 MB stored, then 175 997 refused stores ([01](01-memory-failure-ladder.md) §3-§4) |

- **Raising the budget does not buy capacity where it is missing.** At `c ~ 1.2`
  storing 2.4 GB would spend 2 GB of the guest's own RAM to hold it - a wash that
  also starves the page cache the build is living on. Where the budget would help
  (compressible pages) it is already 30x away from binding.
- **Lowering it loses the good case.** At `c = 9` the budget could park 2.25x
  `MemTotal` of uncompressed anon; cutting the budget to 12.5% would halve that
  headroom for a saving nothing has measured a need for.
- **Capacity is cheap and is the only thing that lets compressible pages park.**
  Its kernel-side cost is the swap map and device metadata, not RAM for pages
  (the 1.4 s figure the map's Notes cite is its one-time CPU, paid once at boot
  by a detached worker, not on the enter path).
- **The `SwapTotal`-overstates-usable-swap problem is real but is not a geometry
  problem.** `SwapTotal` is the capacity (3932 MB at `--mem` 4) while the budget
  delivers 983 MB at `c ~ 1.2`, and that gap is exactly the 3x drain and the
  175 997 retries. There is no geometry that makes both true at once, because the
  capacity is reported in uncompressed bytes and the budget in compressed ones -
  the only lever that acts inside the gap is deciding *earlier* not to try, which
  is [11](11-earlyoom.md)'s job.

### The sysctls stay unset

- **`vm.swappiness` (default 60)**: no arm could price it. A build storm never
  swaps at all (`pswpout` 0 at 2 GiB and 4 GiB; 74 MB at 1 GiB), so swappiness
  has no measured effect on the workload this map exists for. Raising it would
  favour parking compressed anon over dropping file cache - right for `c = 9`,
  wrong for `c ~ 1.2`, and unmeasured either way.
- **`vm.min_free_kbytes` (11.3 MB) and `vm.watermark_scale_factor` (10)**: note
  [02](../notes/02-kernel-knobs.md) ranked these first on *source reading*, and the arms
  have since removed their use case in this workload: a full 26-job storm neither
  reclaimed nor stalled (`PSI full avg10` 0.00 at 2 GiB, zero `pgscan_*` deltas),
  and `cang exec` under that storm was p50 39-46 ms, not note 32's 13.2 s p95.
  Shipping them now would buy an unmeasured reclaim-habit change for 50-120 MB of
  a RAM-bound guest. **Not shipped; the ticket that re-opens them needs a
  reclaim-heavy arm** (a compressible anon consumer beside a page-cache-starved
  build), which no arm has run.
- **MGLRU `min_ttl_ms`** (note 02's third lever): the anti-thrash value is a
  *decision* the early responder makes first once [11](11-earlyoom.md) exists;
  setting it here would pre-empt that ticket's ordering question.

### The one candidate that is worth an arm, and is cheap

**zram `algorithm_params` zstd level.** The pinned kernel's zstd backend accepts
`level=` through the device's `algorithm_params`, and the default is the middle of
the range. Build pages compress 9-10x, so a faster level would lose very little
ratio and cut per-page CPU in the context that pays it (kswapd, or the allocating
thread in direct reclaim - `zram_drv.c` compresses synchronously in the submitter).
That is a plausible win and it is **unmeasured**, so it does not ship here.

The arm that would settle it, in the kit:

1. boot one guest, `echo level=1 > /sys/block/zram0/algorithm_params` before
   `swapon`;
2. run [the kit](../../../../tools/responsive-arm/README.md)'s presser with
   `CHUNK_MB=32` and the build storm beside it, sampling `mm_stat` (which gives
   `c` at that level) and `/proc/vmstat`'s `pswpin`/`pswpout`;
3. compare against the same arm at the default level: `c`, `mem_used_total` at the
   same stored bytes, and the CPU time in `pswpout`'s window.

Until that runs, the shipped geometry is the one the ladder measured, and this
ticket's answer is **no change**.

### What this ticket deliberately does not do

It does not touch `swap/zram.rs`'s constants (`SWAP_CAPACITY_PERCENT` 100,
`SWAP_MEM_LIMIT_PERCENT` 25, `SWAP_PRIORITY` 100, `MIN_SWAP_BYTES` 256 MiB, all
unchanged) and writes no sysctl, so there is no code change to test and no new
claim to verify: the constants are the ones four live arms ran against.
