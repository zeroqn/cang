---
label: wayfinder:research
title: Which memory and CPU knobs this kernel actually offers, ranked
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

Produce the shortlist of guest-wall knobs worth a ticket, with the ones that are
*not* worth one named and dismissed. Source: the pinned kernel's effective config
(`deps/libkrunfw/config-libkrunfw_x86_64-kvm`, which `nix/pkgs/libkrunfw.nix`
copies over `config-libkrunfw_x86_64`) and the `linux-7.3.5` source at
`/nix/store/1pmrvbg56rly33v67d14sghsxcdzp2pw-linux-7.3.5.tar.xz` (+ the
`linux-hardened` v7.3.5 patch) - **the submodule's own
`deps/libkrunfw/tarballs/linux-7.3.1.tar.xz` is stale, do not cite it.** Where
the map is silent on a default, the tarball's
`Documentation/admin-guide/sysctl/*.rst` and the initialising C are the authority.

For each knob: what it would buy under this map's failure mode, what it costs,
and how it would be proven in an arm.

At least these, and the ones the kernel has that this list misses:

- **Reclaim aggression**: `vm.min_free_kbytes`, `vm.watermark_scale_factor`
  (default 10), `vm.watermark_boost_factor` (default 15000 - a burst boost that
  can turn one allocation into a reclaim storm), `vm.vfs_cache_pressure`.
- **Swap behaviour**: `vm.swappiness` (default 60; what it means when the only
  device is zram and every page written costs CPU), swap priority
  (`zram.rs:56` sets 100), `vm.page-cluster`, and whether `vm.swap_async`-style
  batching even exists at this pin.
- **Writeback**: `vm.dirty_ratio`/`dirty_background_ratio` and the cgroup
  writeback path - a workspace build writes a lot, and dirty pages are
  unreclaimable pages.
- **MGLRU** (`CONFIG_LRU_GEN_ENABLED=y`, `:1049`): `min_ttl_ms`, and what
  enabling stats would cost (`CONFIG_LRU_GEN_STATS` is off).
- **KSM** (`CONFIG_KSM=y`, `:990`): off by default; what it would save on a
  guest running many `rustc`s from one store, and what the scanning CPU costs.
- **THP** (`MADVISE`, `:998`): whether a build's heaps would benefit from
  `always` for this workload, and the memory cost of the tail.
- **Scheduler**: `sched_latency_ns`, `sched_min_granularity_ns`,
  `sched_wakeup_granularity_ns` with `# CONFIG_SCHED_AUTOGROUP is not set`
  (`:237`) - and whether anything here beats the nice `-20` already shipped
  (`runtime/priority.rs:11`).
- **PSI**: `CONFIG_PSI=y` (`:152`) means the guest can *see* its own stalls. Note
  what a guest-side pressure watcher could trigger, and leave the policy to the
  map's later tickets.

Deliverable: `notes/02-kernel-knobs.md`, a ranked table with a kernel citation
per row, an explicit "not worth a ticket" list with reasons, and a ranking that
ticket [06](06-zram-and-swap-tunables.md) can decide from.

## Resolution

Closed 2026-11-25. Answer in
[../notes/02-kernel-knobs.md](../notes/02-kernel-knobs.md), every default cited
`path:line` from the extracted `linux-7.3.5` tree (`Documentation/` where the
default is documented rather than initialised).

The three worth a ticket, in order: `vm.watermark_scale_factor` 10 -> 50..100
(keeps allocations off *direct* reclaim, the path behind slow `cang exec` under
load), `vm.min_free_kbytes` 11 -> 64..128 MiB @8 GiB (the only lever that raises
`WMARK_MIN`; costs RAM - test as a pair with the first), MGLRU `min_ttl_ms`
0 -> 1000..3000 (turns thrash livelock into a decisive OOM, which lands on the
workload because the supervisor is at `oom_score_adj -1000`). Then zram
`mem_limit` + zstd `level` (ticket [06](06-zram-and-swap-tunables.md)) and a
cheap, unknown-sign `vm.page-cluster` / `vm.dirty_background_ratio` pair.

Dismissed with reasons: `watermark_boost_factor` (capped at 1.5 x `WMARK_HIGH`,
~42 MiB - not a storm), `vfs_cache_pressure`, KSM (merges only
`MADV_MERGEABLE`; nothing here marks it), THP `always`, and **all three CFS
tunables the ticket named** - `sched_latency_ns`/`sched_min_granularity_ns`/
`sched_wakeup_granularity_ns` do not exist in 7.3.5, and their replacement
`sysctl_sched_base_slice` is reachable only through debugfs, which
`# CONFIG_DEBUG_FS is not set` removes. Also dead at this pin: zram `idle`,
`recompress`, `recomp_algorithm`, `writeback*`.

Nothing measured: every "buys" is a source reading. Before an arm trusts the
defaults, read them back once from a live guest (`/proc/sys/vm/*`,
`/sys/kernel/mm/{ksm,lru_gen,transparent_hugepage}`) and diff.
