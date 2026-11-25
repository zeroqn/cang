---
label: wayfinder:task
title: the workload bounds are applied twice on the session payload
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

The `process::WorkloadBounds` refactor (commit `d3250106`'s follow-on, in the
worktree) added one bounds application in `runtime/session.rs::spawn_pty_child`
and kept the one `process::become_workload()` makes later on the same path, so a
default launch's payload is narrowed **twice**: `26 -> 0-23` in the fork child,
then `24 -> 0-21` when `become_workload()` re-resolves its plan inside the
already-masked child and musl's `sysconf(_SC_NPROCESSORS_ONLN)` counts the
affinity bits rather than the online CPUs.

Measured in [note 14](../notes/14-nix-build-bound.md) §3: five ON boots of a
default launch all read `Cpus_allowed_list 0-21`, `nproc 22`, on a 26-vCPU guest,
while `/run/cang/responsive.status` reads `mask=0-23` and the shell gets
`CARGO_BUILD_JOBS=24` - i.e. four cores reserved instead of the policy's two, and
a job count that oversubscribes the payload by two. The two seams that apply
once are correct in the same boots: `cang exec` reads `0-23`, and the
`nix-daemon` this ticket series confined reads `0-23`.

The fix and the two candidate shapes are in note 14 §3.2: pass the
parent-resolved bounds down to `child_main_result` and let
`drop_to_identity_and_exec`/`exec_command` take `Option<WorkloadBounds>` (`None`
meaning "already applied"), or restore `allow_workload_kill()` in
`spawn_pty_child` and let `become_workload()` be the single apply (which puts the
env read and `sysconf` back between `fork` and `exec`). Whichever lands should
also be read back on a `--root` launch, which this arm did not measure but takes
the same double path.

Resolution = a note that reads the payload's mask and the status file on one
boot of the fixed tree, agreeing at `0-23`.

## Resolution

**Fixed and read back on live boots** - [note 15](../notes/15-bound-applied-once.md).
The two seams now apply the bound once, and a default launch's payload reads the
policy's mask instead of two cores narrower:

```text
RESULT workload self pid=745 comm=python3.13 mask=0-23 adj=0 py_sched_getaffinity=24
RESULT guest nproc_cli nproc=24 nproc_all=26
RESULT mask_watch label=payload samples=2283 masks=0-23:2283 first_t=1.02 last_t=234.28 distinct_masks=1
RESULT responsive_status state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24
RESULT shell env SH_CARGO_BUILD_JOBS=24 SH_NPROC=24 rc=0
RESULT supervisor pid=710 comm=cang-guest-init mask=0-25 adj=-1000
EXECMASK=0-23 EXECNPROC=24 EXECNPROC_ALL=26 EXECADJ=0
```

- **Which fix landed**: the first candidate shape, and both halves of it -
  `guest_cpu_count()` reads `/sys/devices/system/cpu/online` instead of musl's
  affinity-based `sysconf` (`runtime/cpu_bound.rs:68`, `:78`, `:84`), and
  `apply_workload_range` is idempotent so the layered seams
  (`runtime/session.rs::spawn_pty_child`, then `process::WorkloadBounds::apply`)
  cannot reserve twice (`:108`, `:150`). Three default launches (no `--root`),
  `--mem 4`, 26 vCPUs: every boot reads `0-23` / `24`, incl. the first readback at
  t = 0.16-0.75 s and the last at t = 234 s.
- **The record now agrees with the mask**: `mask=0-23` (24 CPUs) =
  `cpus 26 - reserved 2` = `cargo_build_jobs 24`, so the job count no longer
  oversubscribes the payload by two, and `nproc 24` (not 22) is what the payload
  and a fresh `/bin/sh -lc` see.
- **The seams that were already correct stayed correct in the same boots**:
  supervisor `0-25` (2283 samples), `cang exec` child `0-23` / 24, the
  `nix-daemon` `0-23` with its `-1000`, and a builder it forked `0-23` (848
  samples, chain child-first).
- **The reserve is visible**: a 30 s saturation window leaves exactly cpu24 and
  cpu25 near-idle (`0.005` / `0.004` busy) while cpu0-cpu23 are `0.987`-`1.000`
  busy - the arm-14 "four reserved" shape does not appear.
- **The booted bytes are the checked-build bytes**: a fresh `nix build
  .#cang-musl` from this worktree ran the check phase (`441 passed; 0 failed`) and
  produced a `cang-guest-init` that `cmp`s identical (sha256 `26967224…`) to the
  one boot `ws15e` used. The check phase did fail on the tests as first written
  (the sandbox has no `/sys`); note 15 §7 records that finding and the fixture
  rewrite that repaired it - `crates/` was out of scope for this arm.

> **Correction, 2026-11-25 (parent session).** The check-phase blocker this note
> records at §5 is fixed in the shipped tree, so the `doCheck = false` workaround
> it describes is no longer needed: the two environment-dependent tests were
> replaced by a tempfile fixture that asserts the CPU count comes from the kernel's
> list rather than this process's affinity, and
> `nix build .#cang-musl` with the check phase on now reports
> `running 441 tests` / `test result: ok. 441 passed` plus the relay's
> `31 passed`. The arm's own clean-binary boot (`ws15e`) is unaffected - it already
> used a build whose check phase passed.
