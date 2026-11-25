---
label: wayfinder:map
title: Guest responsiveness under a fixed RAM budget
---

## Destination

Under a memory-heavy build (a rustc storm) on a fixed `--mem`, a cang guest
keeps the microVM alive **and** keeps its interactive paths - the keystroke
round trip and command start - inside a stated latency budget, with nothing but
RAM-backed zram swap to compress into. Reached when all four hold:

- **Survival**: every `cang-guest-init` process that must outlive the storm is
  out of the guest OOM killer's reach, and a live arm shows the VM alive with
  the workload killed instead of the session.
- **Cores**: build parallelism leaves the cores the rule names free -
  `jobs = max(1, nproc - 2)` (`nproc=2` -> 1, `nproc=1` -> 1), or an equivalent
  workload CPU bound that covers *any* workload (rustc, nix, make) - and an arm
  shows the latency that buys.
- **Memory**: the levers the measurement justified ship as one new default-on
  `cang-guest-init` component, with `CANG_GUEST_RESPONSIVE=0` as the documented
  opt-out.
- **Record**: before/after numbers *and* every lever that was refused, with raw
  log paths, in `docs/images-and-storage.md` and `README.md`.

If an arm shows a lever costs more than it buys, the map ends by recording that
number and leaving the lever out - that is a reached destination, not a
failure. "Reached" means live guests, never a green `cargo test`.

## Notes

- **Destination decisions (bob, 2026-11-25):**
  - **measure, then ship** - live A/B arms decide; this map carries execution
    (code, docs, release), like [the /nix upper map](../nix-upper-on-block/MAP.md).
  - **resilience first, latency second** - never lose the microVM, then cut
    interactive latency. Both are in scope; the order is the priority.
  - **one guest component, default-on, env opt-out** - no host `--flag`, no
    per-launch decision. `CANG_GUEST_RESPONSIVE=0` is the escape hatch.
  - **a CPU bound that covers any workload, if it works** - bob's answer to the
    job-count question: prefer a cgroup/affinity bound over `CARGO_BUILD_JOBS`
    alone *if* it really holds for rustc, nix and make alike. Ticket
    [04](tickets/04-workload-cpu-bound.md) settles that; the job count is the
    fallback that ships either way, because it is also the only one that bounds
    concurrent-compiler *memory*.
- **Settled 2026-11-25 (bob, after the overcommit verdict):**
  - **earlyoom is accepted** as the guest's early responder, whose job is to kill
    *before* the guest reaches the wall so kswapd never enters the long thrash -
    policy ticket [11](tickets/11-earlyoom.md). It skips `oom_score_adj -1000`,
    so [08](tickets/08-guard-guest-init-processes.md)'s protection is what makes
    both killers ignore the workers, and earlyoom's own `-p` sets only -100.
  - **`vm.overcommit_memory=2` was aimed at one thing: reducing kswapd kick-in,
    so the interactive path stays responsive. That linkage does not exist and is
    now settled as refuted** - commit accounting runs at `mmap`/`brk`
    (`mm/util.c:977-1015`, charge sites only; zero `vm_enough_memory` in
    `mm/page_alloc.c`, `mm/memory.c`, `mm/vmscan.c`, `kernel/fork.c`), while
    kswapd is woken by the allocator's watermark checks
    (`mm/page_alloc.c:4506` `wake_all_kswapds` -> `:4526`, and the boosted-watermark
    fast path `:3447-3451`). Nothing in the mode changes which of those paths
    runs. Detail: [notes/03-overcommit-mode.md](notes/03-overcommit-mode.md).
  - **The levers that do reduce the cost of reclaim**, in the order the map now
    carries them: earlyoom ([11](tickets/11-earlyoom.md)), bounding demand
    ([04](tickets/04-workload-cpu-bound.md)/[09](tickets/09-rustc-job-cap.md)),
    keeping `kswapd0`/`kcompactd0` on the cores left free
    ([09](tickets/09-rustc-job-cap.md)) - because zram compression happens in
    the writing context - cheaper per-page compression and the zram geometry
    ([06](tickets/06-zram-and-swap-tunables.md)), and MGLRU `min_ttl_ms`
    ([06](tickets/06-zram-and-swap-tunables.md)).
  - **Direction worth remembering about watermarks** (note 02's #1/#2): raising
    them makes kswapd start *earlier*, i.e. **more** kswapd activity, on purpose -
    the point is to move page compression out of the allocating thread
    (`cang exec`'s direct-reclaim path) and into a background thread. So
    "fewer kswapd kicks" and "less latency" are opposite pulls on that knob; the
    arm decides, and [06](tickets/06-zram-and-swap-tunables.md) owns it.
- **Tracker: local markdown**, this directory (`gh` is unauthenticated here;
  bob pushes branches and tags). Land through `main`.
- **One ticket per session**, except research tickets, which may run in
  parallel. A session claims a ticket by filling `claimed_by` before any work.
- **Skills every session should consult**: `cang-live-guest-evidence-harness`
  (boot, delegate an arm, fresh-log check, clean window, kill strays),
  `evidence-capture-reliability` (arm discipline, counters vs causes),
  `guest-snapshot-offline-reanalysis` (retained captures, unavailable-is-not-zero),
  `cang-wayfinder-corpus-work` (this corpus), `cang-feature-question-source-verdicts`
  (source verdicts without launching), `fake-only-test-boundary-audit` and
  `cang-change-verification-gates` (VM-free tests and the fmt/clippy gates).
- **Vocabulary** (`GLOSSARY.md` has no entry yet; ticket
  [10](tickets/10-ship-and-docs.md) adds them): *guest memory budget* = the
  guest's `MemTotal` (`--mem`), fixed for the VM's life; *zram budget* = the
  device's `mem_limit`, the RAM the compressed store may spend; *effective
  anonymous ceiling* = what the guest can actually hold = `MemTotal` + the
  compressible share of the zram budget; *responsiveness* = the interactive paths
  (PTY round trip, exec handshake) staying inside a latency budget while a
  workload saturates CPU and memory.

- **Starting facts (verified 2026-11-25, this checkout):**
  - **The guest's only swap is zram, sized at 100% of guest RAM** with a
    compressed-memory budget (`mem_limit`) of 25% of it -
    `components/swap/zram.rs:62,71` (`SWAP_CAPACITY_PERCENT`,
    `SWAP_MEM_LIMIT_PERCENT`), floor `MIN_SWAP_BYTES` 256 MiB (`:73`),
    priority `100` (`:56`). Capacity counts *uncompressed* bytes, so the
    effective anonymous ceiling is roughly `MemTotal + a compressible fraction
    of 25%`.
  - **The pinned `libkrunfw` kernel is `linux-7.3.5` + `v7.3.5-hardened1`**
    (`nix/pkgs/libkrunfw.nix`, `kernelVersion`/`kernelHardenedVersion`), built
    from **`config-libkrunfw_x86_64-kvm`** - which that derivation's `preBuild`
    copies *over* `config-libkrunfw_x86_64`, so the two spellings are one file
    for cang's build (they differ in exactly three symbols:
    `KERNEL_GZIP`/`KERNEL_LZ4`/`DEFERRED_STRUCT_PAGE_INIT`). All symbols below
    are identical in both, and the line numbers hold for the `-kvm` file:
    `CONFIG_PSI=y` (`:152`), `CONFIG_MEMCG=y` with `# CONFIG_MEMCG_V1 is not set`
    (`:202`), `CONFIG_FAIR_GROUP_SCHED=y`/`CONFIG_CFS_BANDWIDTH=y` (`:208-209`),
    `# CONFIG_SCHED_AUTOGROUP is not set` (`:237`), `CONFIG_KSM=y` (`:990`),
    `CONFIG_TRANSPARENT_HUGEPAGE_MADVISE=y` (`:998`), `CONFIG_LRU_GEN_ENABLED=y`
    (`:1049`), `# CONFIG_ZSWAP is not set` (`:923`), `CONFIG_ZRAM=y` with
    **zstd and lzo only** and **`# CONFIG_ZRAM_WRITEBACK is not set`**
    (`:1492-1502`), `# CONFIG_DAMON is not set` (`:1061`). **Cite kernel source
    from `/nix/store/1pmrvbg56rly33v67d14sghsxcdzp2pw-linux-7.3.5.tar.xz`** -
    the submodule's own `deps/libkrunfw/tarballs/linux-7.3.1.tar.xz` is stale.
  - **No guest memory sysctl is written anywhere**: no `vm.swappiness`,
    `vm.min_free_kbytes`, `vm.watermark_boost_factor`, no overcommit policy.
    **`/sys/fs/cgroup` *is* mounted** - by the pinned libkrun's init
    (`deps/libkrun/init/init-binary/src/fs.rs:52-62` mounts `cgroup2` there), with
    `cpuset cpu io memory hugetlb pids` at the root; `cgroup.subtree_control` is
    empty, so a fresh child cgroup sees no controllers until someone writes
    `+cpu` (measured in [04](tickets/04-workload-cpu-bound.md) §3, which corrects
    this map's earlier "nothing mounts cgroup2").
    `components/podman/config.rs:56` sets `cgroups = "disabled"`, which is what
    keeps podman out of that tree.
  - **OOM protection today covers exactly one process**: the managed-session
    supervisor, `oom_score_adj = -1000` (`process.rs:349,363`, called from
    `runtime/session.rs:80`), restored to `0` in the workload child
    (`process.rs:352,375`, `session.rs:1521`). The detached workers -
    `internal swap ensure` (`components/swap/zram.rs:134`), `internal nix prep`
    (`components/nix/root.rs:191`), `internal podman prep`
    (`components/podman/root.rs:71`), the Pulse bridge (`components/pulse.rs:28`)
    - are spawned at score 0 and are as killable as cargo.
  - **Interactive threads are already prioritised**: nice `-20`
    (`runtime/priority.rs:11`) on the session PTY pump/input threads
    (`session.rs:854,865`) and the exec listener (`exec.rs:45,60`). Note 36 in
    [the /nix upper map](../nix-upper-on-block/notes/36-pty-responsiveness-under-load.md)
    measured what that bought: PTY p99 24.1 -> 7.5 ms, max 1 353 -> 59 ms under
    13 concurrent `rustc`s, and *no* sample over 250 ms. It did **not** fix
    `cang exec` start cost under load (397 ms p50 / 13.2 s p95, note 32) - that
    is what "a new command is still slow while the guest compiles" means.
  - **Nothing sets build parallelism**: no `CARGO_BUILD_JOBS`, `NIX_BUILD_CORES`
    or `MAKEFLAGS` anywhere in `crates/` or `nix/`. `cargo` therefore uses every
    vCPU, i.e. every core the guest was given.
  - **The guest's vCPU count is host-derived and has no flag**:
    `resolve_cpu_count` (`runtime/launch/config/components/resources.rs:8-15`)
    gives all CPUs when the host has <= 6 and `available - 2` above that; guest
    `nproc` equals that count.
  - **Guest RAM defaults to 80% of host RAM** (`resources.rs:134-138`), with a
    warning above 80% and a refusal above 90% (`:43,49,63`) - so the guest is
    allowed to hold the host's memory while the host itself has no disk swap.
  - **The host half of reclaim already exists**: a host-side watcher polls
    `/proc/pressure/memory` + swap-out rate and sends `ReclaimMemory` over the
    exec channel, which the guest answers by writing `1` to `vm.drop_caches`
    (`components/memory.rs`, `docs/images-and-storage.md` "Guest memory and zram
    swap"). It reacts to *host* pressure only; nothing in the guest reacts to
    *guest* pressure.

## Decisions so far

<!-- the index: one line per closed ticket, enough to judge relevance, then zoom the link for the detail the ticket holds -->

- [nix build is inside the CPU bound](tickets/14-nix-build-inside-the-bound.md):
  proven on a default launch, five ON boots and one OFF control. `nix-daemon` reads
  `Cpus_allowed_list 0-23` with `oom_score_adj -1000` still on it, the builder the
  daemon forks is caught mid-build at `0-23` (318 samples, chain
  `sh -> nix-daemon(build) -> nix-daemon -> init.krun`), the build produces its
  output, the supervisor stays `0-25` and `cang exec` stays `0-23`/`rc=0`; the OFF
  control (`CANG_GUEST_RESPONSIVE=0`) moves daemon and builder to `0-25` and keeps
  `-1000`. **One regression recorded, not closed**: the same `WorkloadBounds`
  refactor made the session payload apply the mask twice, so it reads `0-21`
  (22 vCPUs) in every ON boot while the status file and `CARGO_BUILD_JOBS=24` say
  `0-23` - a follow-up ticket. Also: the installed host CLI predates the
  `CANG_GUEST_RESPONSIVE` forwarding, so the OFF arm needed a rebuilt `.#cang`.
  Detail: [notes/14-nix-build-bound.md](notes/14-nix-build-bound.md).
- [The default launch takes both bounds, and the early responder fires before the wall](tickets/13-default-bound-and-earlyoom.md):
  both proven, on the **default** launch (no `--root` - the shape a user gets). **A**:
  payload `nproc 24`, `Cpus_allowed_list 0-23`, 4 426 `rustc` mask samples all `0-23`,
  supervisor `0-25` in 586/586, the 24 mask cores at 0.77-0.82 busy; control was arm
  09's unbounded `0-25`/`nproc 26`. Caveat kept as the arm recorded it: the two
  excluded cores are **not idle in the strict sense** - they carry the guest's own
  unconfined kernel threads (expedited RCU 207 CPU-s, `khugepaged` 70, workers),
  0.156-0.210 busy in two boots, where arm 04's task-scaled measurement read 2-4% -
  so the accurate claim is "the workload never uses them", not "they are idle". **B**: `earlyoom` fires
  at **9.96 % MemAvailable (368 MB of 3 697 free) and kills the 3.2 GB presser 100 ms
  later**, with `mem_used_total` **20 480 kB of a 1 031 000 064 budget in all 210 blocks**,
  `failed_writes` **0**, `pgscan_direct`/`pswpout` **0 for the whole arm**, `oom_kill` **0**,
  no kernel line on the console at all, and the session answering `cang exec` rc=0 after
  the kill. Control (arm 01 arm 4): budget pinned, 175 997 refusals, 2.2M scans in one
  second. A census read-back also confirms the responder at `oom_score_adj -1000`, which
  is why its own `-p` flag is refused (it would rewrite the score to -100 after start).
- [Decide the zram geometry and the swap-related sysctls](tickets/06-zram-and-swap-tunables.md):
  **change nothing, and record why.** Capacity 100% / budget 25% stay: at `c ~ 1.2`
  raising the budget would spend a gigabyte of the guest's own RAM to store two, and
  at `c = 9` (what a build actually gets) it is 30x from binding, while the capacity
  is the only thing that lets compressible pages park at all and costs metadata, not
  pages. `swappiness` stays 60 because no arm could price it (a build never swaps);
  note 02's watermark levers stay **unshipped** because the storm that would justify
  them neither reclaimed nor stalled; MGLRU `min_ttl_ms` waits for
  [11](tickets/11-earlyoom.md)'s ordering decision. The `SwapTotal`-overstates-usable-swap
  gap is real and is not a geometry problem - only deciding earlier not to try acts
  inside it. One candidate named for an arm: zram `algorithm_params` zstd level
  (build pages compress 9-10x, so a faster level loses little ratio and cuts
  compression CPU where it is paid).
- [The component's shape, its opt-out, and where it runs](tickets/07-component-boundary-and-opt-out.md):
  one module, at the levers' seams. `components/responsive.rs` owns the switch
  (`CANG_GUEST_RESPONSIVE`, default on, the `parse_env_flag` matrix so an
  unrecognised value changes nothing), the boot-time application and
  `/run/cang/responsive.status` (`state`, `cpus`, `reserved`, `mask`,
  `cargo_build_jobs`); the levers stay where their seams are
  (`process::protect_from_oom`, `runtime::cpu_bound::confine_workload`). The host
  sends `CANG_GUEST_RESPONSIVE=0` into the guest env **only when it disables**, so
  the default launch's environment is unchanged. **The switch never reaches the OOM
  guard**: a lever trades throughput for latency or survival, the guard only
  prevents a crash. Detail: [notes/07-component-shape.md](notes/07-component-shape.md).
- [The responsiveness-and-survival arm kit](tickets/05-response-arm-kit.md):
  `tools/responsive-arm/` - the sampler, the distinct-page presser and the one-boot
  host driver, promoted from the artefacts that measured tickets 01/04/12 rather
  than re-derived (the fourth arm to build this instrument hit bugs its
  predecessors had fixed: `printf '--'` under dash, root-only zram sysfs fields, and
  a zero-filled presser that spends nothing because zram stores identical pages as
  a flag). Parameterised for `MODE=session|exec`, `MEM`, `PAYLOAD`, `GUEST_INIT`;
  its gate matches the VM worker's **comm**, not a command line, which the first
  trial proved necessary. Acceptance is the first arm that uses it (06 or 11) - no
  end-to-end run through the shipped kit exists yet.
- [What bounds the workload's CPU - job counts, a cgroup quota, or affinity?](tickets/04-workload-cpu-bound.md):
  **affinity, and it is the only candidate that actually leaves cores free.**
  Mask `0..N-3` (N=26 here -> `0-23`), applied to the managed-session payload child
  before its `exec` and to each `cang exec` child in its `pre_exec`, never to the
  supervisor or the listener threads (they are the nice `-20` interactive path).
  Measured reserve: cpu25 115/5780 jiffies busy = 2.0 %, cpu24 243/5950 = 4.1 %,
  against the control's least-busy CPU 87.8 %; the build *gained* (-3.8 % wall,
  -10.4 % CPU ticks) and peak anon fell 13.2 % because cargo's own job count
  follows the mask. `cpu.max` is refused (bounds throughput but leaves the least
  busy CPU at 73 % - throttling spreads, it does not vacate), `cpu.weight` refused
  (zero throttles, zero free cores), and a whole-guest quota refused at
  **2.15x wall** with the campaign's worst interactive numbers (PTY p50 12.3 ms,
  exec p50 125 ms). `CARGO_BUILD_JOBS` ships only as a default-when-unset
  companion, and **cannot cover nix** (`nix-daemon` is not a descendant of the
  shell; `cores` is not `max-jobs`). `kswapd0`/`kcompactd0` are re-affinable
  (`taskset -pc 24-25` accepted and held) but unearned here - that storm never
  reclaimed. Detail: [notes/04-cpu-bound.md](notes/04-cpu-bound.md).
- [exec'd work inherits the supervisor's -1000 and the guest panics for want of a victim](tickets/12-exec-inherits-oom-protection.md):
  fixed and proven live. `runtime/exec.rs`'s child preparation
  (`prepare_exec_child`) resets the forked child's `oom_score_adj` as its first
  act, and `process::drop_to_identity_and_exec` does the same for the `as-dev`
  path, so an exec launch now keeps the workers protected and everything the
  listener starts killable. Re-running [01](tickets/01-memory-failure-ladder.md)'s
  arm 2 on one boot with the same host CLI and image: the exec'd script and its
  3.6 GB presser read **0** (116 in-process readbacks), the console kills
  *`presser`* instead of the idle `sleep`, there is **no** `deadlocked`/panic line
  where the pre-fix arm had both, `cang exec` returns rc=0, and the session
  answered a second `cang exec` 10 s after the kill (`cang ps` still `running`
  114 s after). The arm reached the same wall (`mem_used_total` = `mem_limit`,
  158 350 refused stores), so it is not a null result. VM-free: a test that
  raises its own score to +1000 and reads the prepared child (0) against an
  unprepared control (1000), with a failing pre-fix control run. Detail:
  [notes/12-exec-oom-inheritance.md](notes/12-exec-oom-inheritance.md).
- [What the guest's memory failure ladder actually is](tickets/01-memory-failure-ladder.md):
  measured over seven boots, and the map's working assumption was wrong in two
  ways. **A build storm never reaches the wall** - 26 `rustc`s at the shipped
  default completed at `--mem` 4, 2 *and* 1 GiB, with PSI `full avg10` 0.00 and
  zero swap-outs at 2 GiB; the ladder's early rungs are a page-cache story, so a
  storm has to be *manufactured* to test the wall. The ceiling is
  `MemTotal·(1 + 0.25·c)`: `c` = **8.99-9.8** for a real build's pages (the budget
  never binds), **1.09-1.25** for incompressible ones (the budget pins at exactly
  `mem_limit` after storing 1222 MB, then the device refuses 175 997 stores while
  its 3932 MB of *address space* drains at ~3x the budget rate - a zero-fill
  presser measures nothing at all, `same_filled`, `zram_drv.c:2302-2305`). The
  OOM killer picks by `rss + swapents` (`oom_kill.c:228-229`), so the victim had
  38 MB resident and 3.6 GB swapped. And a **live defect**: `cang exec` children
  inherited the supervisor's -1000, so the guest panicked for want of a victim
  (now [12](tickets/12-exec-inherits-oom-protection.md)). Detail:
  [notes/01-memory-failure-ladder.md](notes/01-memory-failure-ladder.md).
- [Leave the cores free - the build parallelism bound that ships](tickets/09-rustc-job-cap.md):
  **the job count ships and is live-proven; the affinity half reached two of three
  seams**, which a live arm caught rather than the tests. `CARGO_BUILD_JOBS = max(1, N-2)`
  (24 of 26) is default-only and honoured by cargo on **every** launch shape (24
  concurrent `rustc`, never 26); the mask `0..N-3` bounds the `--root` payload, every
  `cang exec` child (88/88 probes `rc=0`), and every `rustc` under it (1287 mask samples),
  while the supervisor stays `0-25` (170 samples). A **default** launch kept `0-25` and
  left no core idle - the seam `drop_to_identity_and_exec` took the killable half and
  missed the CPU half, now one `process::become_workload()` call at all three seams
  ([13](tickets/13-default-bound-and-earlyoom.md) proves the fix). Bounded builds were
  *faster*: -1.8 % wall, -8.7 % CPU, -6.5 % peak anon at equal warmth.
- [earlyoom as the guest's early responder](tickets/11-earlyoom.md):
  shipped as code - `earlyoom` in the image's tooling layer, started by the
  responsive component on the enter path, gated by `CANG_GUEST_RESPONSIVE`, with
  `-m 10` (below a healthy build's ~25% floor, above the wall's ~2.4%), `-s 100`
  (this guest's swap *is* zram, so free-swap is not a pressure signal), `-p`, and
  its own `oom_score_adj -1000` written **in the child** - protecting the parent
  would hand the score to the shell it execs, the defect [12](tickets/12-exec-inherits-oom-protection.md)
  fixed. It skips `-1000` processes, so the guard and the responder share one
  definition of "the guest cannot lose this". **The live arm is owed**: the claim
  "fires before the wall, kills the pressure's owner, session survives" needs one
  arm-4-shape boot on a rebuilt image, with arm 4's 175 997 refused stores as the
  control.
- [Guard every cang-guest-init process the guest cannot afford to lose](tickets/08-guard-guest-init-processes.md):
  shipped one definition of "protected" (`process.rs`'s `protect_from_oom`, score
  -1000) and one table of who calls it (`runtime::protected_worker_label`: the
  zram swap worker, the `/nix` overlay and Podman prep workers, the Pulse bridge,
  plus the managed-session supervisor). Maintenance commands and client-side
  waits stay at 0, and so does the whole workload subtree, so the kernel always
  has a victim; an exec launch protects only the workers, because the enter
  process *becomes* the workload. 420 crate tests, clippy and fmt clean. The live
  storm read-back is still owed to tickets 01/05/11.
- [Is `vm.overcommit_memory=2` the right answer here, with a zram swap device?](tickets/03-overcommit-mode-verdict.md):
  **no** - `CommitLimit` is `MemTotal*r/100 + SwapTotal` (`mm/util.c:924-926`)
  and this guest's `SwapTotal` *is* `MemTotal` backed by 25% of the same RAM, so
  at the default ratio 50 the limit counts RAM twice; only `r ~ 25*(c-1)` means
  anything, and the compression ratio `c` is unknowable at boot. Mode 2 also
  ignores `MAP_NORESERVE`, does not remove the OOM killer at touch time, and self-
  locks swap activation (`CommitLimit` = 0.5 M until `swapon` succeeds, and
  `mkswap`/`swapon` are charged). MGLRU `min_ttl_ms` (ticket 02) serves
  "resilience first" better. Settling arm = one boot shared with
  [01](tickets/01-memory-failure-ladder.md), which also measures `c`. Detail:
  [notes/03-overcommit-mode.md](notes/03-overcommit-mode.md).
- [Which memory and CPU knobs this kernel actually offers, ranked](tickets/02-kernel-knob-inventory.md):
  three levers are worth a ticket - `vm.watermark_scale_factor` 10 -> 50..100
  (keeps `cang exec`/fork off *direct* reclaim), `vm.min_free_kbytes` 11 ->
  64..128 MiB @8 GiB (the only way to raise `WMARK_MIN`, costs RAM, test as a
  pair), MGLRU `min_ttl_ms` 0 -> 1000..3000 (thrash livelock becomes a decisive
  OOM that lands on the workload) - then zram `mem_limit`/zstd `level` for
  [06](tickets/06-zram-and-swap-tunables.md). KSM, THP `always`,
  `watermark_boost_factor`, `vfs_cache_pressure`, zram `idle`/`recompress`/`writeback`
  are dismissed with reasons, and **the three CFS tunables do not exist at this
  pin** (no `sched_latency_ns` et al. in 7.3.5; their replacement is debugfs-only
  and `CONFIG_DEBUG_FS` is off). Detail:
  [notes/02-kernel-knobs.md](notes/02-kernel-knobs.md).
- [Ship the defaults, the docs, and the acceptance numbers](tickets/10-ship-and-docs.md):
  the map's destination, reached. One default-on component (`components/responsive.rs`)
  with one opt-out (`CANG_GUEST_RESPONSIVE=0`) carries the levers; the OOM guard stays
  unconditional. Gates: guest-init 437, cang 795, repository-tests 67, clippy and rustfmt
  clean. The refusal list is part of the answer: `cpu.max`/`cpu.weight`/whole-guest quota,
  `vm.overcommit_memory=2`, the watermark levers, MGLRU `min_ttl_ms`, and sched_ext are all
  recorded with the number that refused them.

## Not yet specified

<!-- in-scope fog that cannot be pinned to a question yet; graduates as the frontier advances -->

- **The host half of the same trade.** The guest's honest ceiling is now measured
  as `MemTotal*(1 + 0.25*c)` with `c` = 8.99-9.8 for build pages and 1.09-1.25 for
  incompressible ones ([01](tickets/01-memory-failure-ladder.md) §7), and a build
  storm never swaps at all - so the host's 90% `--mem` refusal line and its
  reclaim thresholds (`psi_full` 1%, swap-out 64 pages/s, 30 s cooldown) are now
  asking a *different* question than they were charted with: not "how much can the
  guest hold" but "how much RAM should a host with no disk swap lend to a VM whose
  resident set is mostly page cache". Host policy, so out of this map's scope, but
  the fog is sharp now.
- **Whether swap should be up before the workload starts**, now that the early
  responder exists. The device is a detached, gate-released worker, so the first
  ~1 s of a session has no swap at all (`docs/images-and-storage.md`, 0.6-1.3 s of
  the 1.8 s enter path); [13](tickets/13-default-bound-and-earlyoom.md) also caught
  earlyoom reading `swap total: 0 MiB` in its own startup banner for exactly that
  window, though with `-s 100` the swap half of its condition is satisfied either
  way. If a storm can start inside that second, the trade-off between enter
  latency and early-swap insurance needs its own number.
- **Whether the guest should reclaim its own page cache under guest pressure**
  (the mirror image of the host's `ReclaimMemory`), and whether that competes
  with the zram path for the same pages.
- **What the acceptance bar is**, in numbers. Settled as far as this map can: the
  interactive bar is note 36's (PTY p99 7.5 ms, no sample over 250 ms) and it holds
  with the bound in place; `cang exec` under a full storm measured p50 39-46 ms on
  26 vCPUs ([04](tickets/04-workload-cpu-bound.md)), while note 32's 13.2 s p95 never
  reproduced - so a *small-guest* target is still unset, because no arm has run a
  storm on a 2-4 vCPU guest.
- **Containers.** Whether a `podman run` inside the guest belongs inside the
  workload bound, and whether mounting cgroup2 for the CPU bound changes
  podman's `cgroups = "disabled"` detection.
- **Whether the workload's own RSS should be bounded** (a per-process
  `RLIMIT_AS` or `memory.high` on the workload cgroup) rather than left to the
  OOM killer, once the ladder is known.

## Out of scope

- **Host-side swap.** The premise of this map is that neither the host nor the
  guest has disk swap; provisioning it on the host is a different effort.
- **Balloon inflate/deflate.** libkrun implements only free-page reporting
  (`docs/design/2026-11-04-cang-balloon-inflate-design.md`); a host-side inflate
  path is that map's business, not this one.
- **virtio-mem / elastic `--mem`.** The pinned kernel builds it off and libkrun
  has no path.
- **`zswap`, zram writeback, a second compression algorithm.** Not in the pinned
  kernel (`CONFIG_ZSWAP` off, `ZRAM_WRITEBACK` off, zstd/lzo only); changing that
  is a `libkrunfw` firmware effort.
- **sched_ext (scx) in the guest.** Decided out of scope by bob, 2026-11-25,
  with the price recorded so it is not re-derived: the pinned
  `config-libkrunfw_x86_64-kvm` has `CONFIG_BPF=y`/`CONFIG_BPF_SYSCALL=y` but
  **`# CONFIG_BPF_JIT is not set`** (`:123`), no `BPF_JIT_ALWAYS_ON`/
  `BPF_JIT_DEFAULT_ON`, no `CONFIG_DEBUG_INFO_BTF`, no `CONFIG_SCHED_CLASS_EXT`,
  and **`CONFIG_DEBUG_INFO_NONE=y`** (`:3456`) - while
  `kernel/Kconfig.preempt:173-176` makes `SCHED_CLASS_EXT` depend on
  `BPF_SYSCALL && BPF_JIT && DEBUG_INFO_BTF`, and
  `Documentation/scheduler/sched-ext.rst:26-35` asks for two more. BTF is
  generated *from* DWARF (`scripts/link-vmlinux.sh:214-230`,
  `scripts/Makefile.vmlinux:73-75`), so enabling it means compiling the whole
  kernel with DWARF and adding pahole (nixpkgs `pahole-1.31`, absent from
  `nix/pkgs/libkrunfw.nix`'s inputs): a firmware rebuild plus the fork
  tag -> gitlink -> `flake.lock` pin cascade, a scheduler binary in an image
  that ships none (`scx_full-1.1.4` and `libbpf-1.7.0` exist in nixpkgs, zero
  hits for `scx`/`sched_ext`/`libbpf` in `nix/` and `crates/`), a boot-time
  loader in `cang-guest-init` with a `/run/cang/` status file and ticket
  [08](tickets/08-guard-guest-init-processes.md)'s OOM guard, and a host knob -
  including the `SCX_OPS_SWITCH_PARTIAL` decision, where the global mode would
  re-open the note-36 nice `-20` result from scratch. **Re-open only if** the
  arms from tickets [04](tickets/04-workload-cpu-bound.md)/[09](tickets/09-rustc-job-cap.md)
  show the residual latency is scheduler-shaped rather than process-creation
  shaped; until then the kernel-side answer is "no" and the lever that exists is
  the CPU bound. Safety fact worth keeping: a BPF scheduler that errors or stalls
  a runnable task is reverted to the fair class by the kernel itself
  (`sched-ext.rst:9-13,60-64`), so the failure mode is "back to CFS", not "wedged
  guest".
- **Host launch defaults as such.** `--mem` at 80% of host RAM, the host-derived
  vCPU count, and adding a `--cpus` flag are host policy; this map may report
  how they interact (see the first *Not yet specified* item) but does not change
  them.
- **Retiring zram.** Guest swap stays.
