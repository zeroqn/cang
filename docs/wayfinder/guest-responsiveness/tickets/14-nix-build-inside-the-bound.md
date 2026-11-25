---
label: wayfinder:task
title: nix build is inside the CPU bound
status: closed
blocked_by: []
claimed_by: "pi arm-14 (child, agent, 2026-11-25)"
---

## Question

[04](04-workload-cpu-bound.md) §5.5 and [09](09-rustc-job-cap.md) both recorded the
same gap: `nix build` was outside the CPU bound. The mechanism, from this tree: the
guest starts `nix-daemon --daemon` from its `/nix` prep worker
(`components/nix/root.rs::start_daemon`), not from the session, so the daemon kept
every vCPU; and because the shell is given
`NIX_REMOTE=unix:///run/cang/nix-daemon/socket` (`runtime/cang.rs:356-357`),
`nix build` delegates to that daemon, so the **builders are the daemon's children**
and inherit its affinity. Neither `CARGO_BUILD_JOBS` nor `NIX_BUILD_CORES` covers
that path (`cores` is per-builder cores; the concurrent-builder count is the
daemon's `max-jobs`).

The fix confines the daemon at spawn. Prove it, on a default launch:

1. `nix-daemon` reads `Cpus_allowed_list 0-23` and keeps `oom_score_adj -1000`.
2. A builder that the **daemon** forks reads the same mask - caught mid-build with a
   slow, fetch-free derivation so the process exists long enough to sample, with its
   parent chain recorded (`nix-daemon` -> builder).
3. The build still succeeds.
4. The supervisor stays `0-25` and a `cang exec` child stays `0-23`, because the
   bounds are now applied from a parent-resolved plan (`process::WorkloadBounds`)
   rather than resolved between `fork` and `exec` - a refactor this arm also has to
   clear.

Resolution = `notes/14-nix-build-bound.md`, and the OFF control
(`CANG_GUEST_RESPONSIVE=0` -> daemon `0-25`) if the boot budget allows.

## Resolution

`nix build` is inside the CPU bound, proven on a default launch in
[notes/14-nix-build-bound.md](../../notes/14-nix-build-bound.md) - five ON boots (one of them the failed OFF attempt, §4.1) and
one OFF control, one guest at a time:

- `nix-daemon` reads `Cpus_allowed=0ffffff` / `Cpus_allowed_list=0-23` and keeps
  `oom_score_adj=-1000` (`0-25` and still `-1000` in the OFF arm).
- The builder the daemon forks is caught mid-build: `sh` pid 1128, `masks=0-23`,
  `adj=-1000`, 318 samples, chain `sh -> nix-daemon(build) -> nix-daemon -> init.krun`
  (`0-25` in the OFF arm, same chain shape).
- The build still succeeds and writes its output (`content=probe`).
- The supervisor stays `0-25` (350 samples) and `cang exec` reads `EXECMASK=0-23
  EXECNPROC=24` with `rc=0` (`0-25`/`26` in the OFF arm), so the `WorkloadBounds`
  refactor did not regress either interactive seam.
- `/run/cang/responsive.status` reads `state=on cpus=26 reserved=2 mask=0-23
  cargo_build_jobs=24` (`state=off … mask=all` in the OFF arm).

Two things this ticket does **not** close, both recorded in note 14:

1. **A regression the same refactor introduced**: the managed session's payload applies
   the bounds twice (`spawn_pty_child`'s new `bounds.apply()` and the later
   `become_workload()` in `drop_to_identity_and_exec`), and the second resolve reads
   musl's affinity-aware `sysconf`, so the payload lands on `Cpus_allowed_list 0-21`
   (22 vCPUs, four reserved) in every ON boot while the status file and
   `CARGO_BUILD_JOBS=24` still say 24. Fix shape in note 14 §3.2 (apply once per seam);
   not made there (this arm may not edit `crates/`). Needs its own ticket.
2. **The OFF control needs a host CLI built from this worktree**: the installed
   `result/bin/cang` (0.23.6, `/nix/store/x9dflild0…`) contains no
   `CANG_GUEST_RESPONSIVE` literal, so the host-side flag does not cross into the guest
   with it (note 14 §4.1).
