---
label: wayfinder:task
title: the workload bounds are applied twice on the session payload
status: in-progress
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
