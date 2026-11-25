---
label: wayfinder:research
title: Is `vm.overcommit_memory=2` the right answer here, with a zram swap device?
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

Bob's question: should the guest run with `vm.overcommit_memory=2` (never
overcommit)? Answer it with this guest's arithmetic and a live arm, not with the
usual "OOM killer versus malloc failure" summary.

The specific hazards to settle:

1. **The accounting double-counts RAM.** In mode 2 the kernel allows commits up
   to `CommitLimit = swap total + ram * overcommit_ratio / 100`. This guest's
   swap *is* zram: `SwapTotal` = 100% of `MemTotal`, backed by at most 25% of
   RAM in `mem_limit`. So `CommitLimit` (≈ `1.5 x MemTotal` at the default
   `ratio=50`) counts memory capacity that is physically the same RAM. Work out
   what that admits, and whether `overcommit_ratio` can be chosen so the limit
   means anything at all (with `overcommit_memory=2` the ratio still applies;
   check whether the guest has a ratio that makes the limit ≈ the effective
   ceiling ticket [01](01-memory-failure-ladder.md) measures).
2. **Mode 2 refuses commits, not touches.** A workload can still be OOM-killed
   at page-fault time once reclaim fails. So state precisely which failures move
   from "OOM kill" to "allocation returns NULL/fails", and which do not. A
   refused `malloc` in Rust aborts the process (`SIGABRT`), which for a build is
   an attributed failure - is that better than losing the session, worse than
   losing one `rustc`, or irrelevant?
3. **What breaks in the image.** Warehouse the guest's own machinery through
   mode 2: the nix daemon and its build users, podman/buildah's forks and
   mappings, `swapon`/zram sizing, and the balloon's free-page reporting. Large
   `MAP_NORESERVE` reservations are exempt - list what the guest actually does.
   A guest that cannot start a `nix build` at all is a worse outcome than an
   OOM kill.
4. **Numeric default at this pin**: `vm.overcommit_memory`, `vm.overcommit_ratio`,
   `vm.overcommit_kbytes` as the guest boots, and whether anything in the image
   sets them (nothing does today - see the map's starting facts).

Deliverable: `notes/03-overcommit-mode.md` with the arithmetic, one live arm per
interesting mode (`0`, and `2` with at least one `ratio`), and a verdict
recommendation: ship, ship-behind-the-component-with-a-caveat, or refuse by
name. A "no" is a full answer if the numbers say so.

## Resolution

Closed 2026-11-25. Answer in
[../notes/03-overcommit-mode.md](../notes/03-overcommit-mode.md), every kernel
citation from the extracted `linux-7.3.5` tree, and the citations re-checked
independently (`mm/util.c:924-926,977-1015`, `fs/proc/meminfo.c:125`,
`mm/mmap.c:563-566`, `mm/page_alloc.c:4127-4128`).

**Refused as a guest default. High confidence on the accounting, medium on
breakage.**

- `CommitLimit(r) = MemTotal*r/100 + SwapTotal` (`mm/util.c:924-926`), and this
  guest's `SwapTotal` **is** `MemTotal` (zram capacity 100% of RAM,
  `swap/zram.rs:62,302-304`) backed by **25% of the same RAM**
  (`mem_limit`, `:71,308-310`). At the default ratio 50 the limit is 1.5 M of
  which 0.5 M is RAM already counted; the physical ceiling is
  `M + 0.25 M*(c-1)`, so the only meaningful ratio is `r ~ 25*(c-1)` - and `c`
  (the compression ratio the workload actually achieves) is unknowable at boot.
- Mode 2 ignores `MAP_NORESERVE` (`mm/mmap.c:563-566`), so every "just in case"
  reservation becomes a hard commitment; it does **not** remove the OOM killer
  (no charge in the fault path or in `fork`; touch-time exhaustion still reaches
  `mm/page_alloc.c:4127-4128`); and it creates an activation self-lock - the
  limit is `0.5 M` until the gate-released `swapon` succeeds, and `mkswap`/
  `swapon` are themselves charged.
- The two hazards that would decide it (nix daemon + 32 build users; rootless
  podman/buildah's reservations) are named with evidence but marked *unverified -
  needs the live arm*, so a bug report about them is an arm, not an argument.
- What serves "resilience first" better: note 02's MGLRU `min_ttl_ms`, a decisive
  kill with the supervisor already at `oom_score_adj -1000` - not a
  reservation-time veto.

The settling arm is one boot, shared with
[01](01-memory-failure-ladder.md): A (mode 0, control) / B (mode 2, `r=50`) /
C (ratio sweep including `25*(c-1)` from B's own zram `mm_stat`). Note that A and
B measure `c`, which ticket 01 needs anyway - so the arm is not a cost of this
question.
