---
label: wayfinder:task
title: exec'd work inherits the supervisor's -1000 and the guest panics for want of a victim
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

The live ladder arm ([01](01-memory-failure-ladder.md), §6) found a defect, not a
hypothesis: every command started through `cang exec` inherits the session
supervisor's `oom_score_adj = -1000`, because the exec listener runs on a thread
of that process and its child's `pre_exec` reset the process group, the nice
level and the credentials - but not the OOM score
(`runtime/exec.rs:143-165` before this work). `allow_workload_kill` had exactly
one caller, the session's PTY child (`runtime/session.rs:1522-1526`).

The measured consequence, on a 4 GiB guest, in ~16 guest-seconds:

```
proc 730 sleep    0 264702 1891     # session payload: score 0
proc 745 sh   -1000 …               # the exec'd script
proc 980 presser -1000 263626 62661 # the 3.6 GB anonymous consumer
Out of memory: Killed process 730 (sleep) … oom_score_adj:0
Kernel panic - not syncing: System is deadlocked on memory
```

The kernel skipped the real consumer (unkillable), took the only score-0 process
with an mm, and then had nothing left to pick (`oom_kill.c:1159-1166`), so
`is_global_init` was the only survivor and the kernel panicked. That is the
"multi-victim hazard" ticket [08](08-guard-guest-init-processes.md) asked about,
answered: **reachable**, and it inverts the guard's whole purpose - the protected
process becomes the aggressor.

Do the work:

1. **Reset the score on the way in.** `exec.rs`'s child preparation now calls
   `process::allow_workload_kill()` as its first act (`prepare_exec_child`), and
   `process::drop_to_identity_and_exec` does the same, because the `as-dev` path
   is reached from a protected session process too. One rule: any path that turns
   a guest-init process into the workload resets the score.
2. **Prove it without a VM.** `exec_child_does_not_inherit_a_protected_oom_score`
   raises the test process's own score to +1000, spawns a prepared child and an
   unprepared control child, and reads each child's `/proc/self/oom_score_adj`:
   the prepared child reports 0, the control reports 1000. The reset is the same
   write the defect needed, so the sign of the inherited value does not matter.
   The pre-fix control run fails with `left: "1000", right: "0"`.
3. **Prove it live, and that is the claim.** Re-run arm 2's shape - a managed
   session with an idle payload and the pressure arriving through `cang exec` -
   and record: the presser is killed (not the payload), no panic in the console,
   the session survives, and the guest's census shows the exec'd work at score 0.

Resolution = the note `notes/12-exec-oom-inheritance.md` with the live arm's
console excerpt, the census, and the before/after. Also correct
[08](08-guard-guest-init-processes.md)'s resolution, which claimed an exec launch
"protects nothing but the workers": it protected the *listener*, and therefore
every command it started.

## Resolution

Closed 2026-11-25. The score reset is in the worktree
(`runtime/exec.rs`'s `prepare_exec_child` resets the forked child's
`oom_score_adj` as its first act, and `process::drop_to_identity_and_exec` does
the same for the `as-dev` path), and the live re-arm of [01](01-memory-failure-ladder.md)
§6's arm 2 is in [`notes/12-exec-oom-inheritance.md`](../notes/12-exec-oom-inheritance.md):
one boot, `--mem 4`, the same host CLI binary and image as the arm that failed,
a managed session whose payload is an idle `sleep`, and the pressure arriving
through `cang exec`.

**Measured, in the arm that reached the same wall** (`mem_used_total` =
`mem_limit` = 1 031 000 064, 158 350 refused page stores, `oom_kill` 1):

| | before (ticket 01 arm 2) | after (this arm) |
|---|---|---|
| the exec'd script's score | `proc 745 sh -1000` | `proc 862 sh 0`, own readback `0` |
| the 3.6 GB presser's score | `proc 980 presser -1000` | `proc 1008 presser 0` (116 own readbacks, all `0`) |
| console victim | `Killed process 730 (sleep)` | `Killed process 1008 (presser) … oom_score_adj:0` |
| console 25 ms later | `Kernel panic - not syncing: System is deadlocked on memory` | nothing (`grep -c 'deadlocked\|Kernel panic'` = 0) |
| `cang exec` | rc=1, `connection closed before exit status` | rc=0, the script's own log and a `done` sentinel |
| the session | gone (`no active cang task …`) | `running`; a second `cang exec` answered in-guest 10 s after the kill, and `cang ps` still listed it 114 s after |

So the claim this ticket asked for holds: the exec'd work sits at score 0 in the
guest's own census and in its own `/proc/self/oom_score_adj`, the kernel kills
**the presser** rather than the payload, the console has no "deadlocked on
memory", and the session survives and keeps serving `cang exec`.

Two limits worth keeping with the number: the console copy in that arm ends 27 s
after the kill (the `--preserve-debug` task dir was empty by the time it could be
copied, because the harness's own teardown ended the launcher first), so the
absence of a *later* panic rests on the guest state measured up to t+42.9 plus the
live `cang ps` row, not on console lines; and the guest-init binary was built
with `doCheck = false` (another agent's in-flight `cang-guest-relay` test fails
that build's check phase), so **no VM-free test claim comes from this session** -
the `exec_child_does_not_inherit_a_protected_oom_score` test in the same diff was
not re-run here.

[08](08-guard-guest-init-processes.md)'s corrected resolution is now backed by
measurement rather than reading: the protected set (five `cang-guest-init`
processes, `nix-daemon`, the `timeout`/`nix` pair) is at -1000 in the same census
that has the listener's child at 0.
