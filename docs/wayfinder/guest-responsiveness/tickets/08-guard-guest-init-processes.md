---
label: wayfinder:task
title: Guard every cang-guest-init process the guest cannot afford to lose
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

Today exactly one process is out of the OOM killer's reach: the managed-session
supervisor at `oom_score_adj = -1000` (`process.rs:349,363`, from
`runtime/session.rs:80`), with the workload child restored to `0`
(`:352,375`, `session.rs:1521`). The detached workers are at `0` and are as
killable as `cargo`:

- `internal swap ensure --after-exec` (`components/swap/zram.rs:134`) - if it
  dies the session has **no swap at all**, which is the resilience this map is
  about;
- `internal nix prep` (`components/nix/root.rs:191`) - mid-seed death leaves a
  half-prepared overlay;
- `internal podman prep` (`components/podman/root.rs:71`);
- the Pulse bridge (`components/pulse.rs:28`).

Do the work:

1. **One helper, one score.** Put the protection where `process.rs` already keeps
   `set_oom_score_adj`, so there is one definition of "protected" and one of
   "killable", and assert the ordering invariant the existing test asserts
   (`process.rs:778`).
2. **Apply it at the right moment per process**: each worker sets its own score
   as its first act (it inherits the parent's, and the parent may be the
   unprotected boot process), rather than the spawner pre-setting it on a pid it
   does not yet own. State why (a `fork`-then-write race, and `pre_exec`'s
   async-signal-safe constraint).
3. **Know the second killer.** `earlyoom` skips processes at
   `oom_score_adj -1000` (its changelog, v1.6.1), and
   [11](11-earlyoom.md) puts it in this guest - so one score protects a process
   from *both* killers, and a worker left at 0 is killable by either. earlyoom's
   own `-p` gives it only -100, so it needs the score set explicitly too. Settle
   the shared definition here; the daemon's policy belongs to 11.
4. **Keep the exec path sane.** An exec launch has no resident supervisor: the
   process that runs `cang enter` becomes the workload, so protecting it would
   leave the kernel no victim. Confirm from the code which process each launch
   shape leaves protected, and write the rule down.
5. **Prove it live**: a storm that drives the guest to its ceiling, with the VM
   surviving, the workload killed, and `/proc/<pid>/oom_score_adj` of each
   worker read back *during* the load (the note-36 `nice.txt` precedent). The
   arm kit comes from [05](05-response-arm-kit.md) if it has landed; otherwise
   run the simplest storm that reaches pressure and quote the console.

Deliverable: the code change plus `notes/08-oom-guard.md` with the read-backs and
the console excerpt. VM-free tests where the seam allows; the live arm is the
claim.

## Resolution

Closed 2026-11-25, code landed; the live arm is the one claim this ticket does
not earn.

**What shipped.** One definition of "the guest cannot lose this":
`process.rs`'s `SUPERVISOR_OOM_SCORE_ADJ` became `PROTECTED_OOM_SCORE_ADJ` and
`protect_session_supervisor()` became `protect_from_oom(what)`, which writes
-1000 to `/proc/self/oom_score_adj` and reports the caller's name if it cannot
("guest swap worker is not protected from the OOM killer: ..."), so a guest
console says which protection was lost. The workload keeps
`WORKLOAD_OOM_SCORE_ADJ = 0` via the unchanged `allow_workload_kill()`, and the
ordering invariant test still asserts the two are distinct.

**Who protects itself.** The guard runs in `runtime::run`'s `Internal` dispatch,
through one table (`protected_worker_label`), so the list of protected workers
lives in a single place and a worker is covered from its first instruction with
no `fork`-to-write window:

| command | label |
|---|---|
| `internal swap ensure [--after-exec]` | guest swap worker |
| `internal nix prep` | guest /nix overlay prep worker |
| `internal podman prep` | guest Podman prep worker |
| `internal pulse PORT UID GID` | guest Pulse bridge |
| the managed-session supervisor | guest session supervisor (`session.rs`) |

Left at score 0 on purpose: `internal nix gc`, `internal resize`,
`internal workspace free-bytes` (a maintenance VM's own death *is* the answer the
host reads), `internal nix wait` / `podman wait` / `podman service-wait`
(client-side waits, as killable as any foreground process), and the whole
workload subtree.

**Corrected 2026-11-25 (the live arm from [01](01-memory-failure-ladder.md) §6
proved this passage wrong):** an exec launch does *not* protect only the workers.
The exec listener is a thread of the protected supervisor, so **every `cang exec`
child inherited -1000** and became unkillable; the kernel then took the session's
idle payload and panicked with "System is deadlocked on memory". The fix and its
live re-arm live in
[12](12-exec-inherits-oom-protection.md) - `runtime/exec.rs`'s child preparation
and `process::drop_to_identity_and_exec` now reset the score, so an exec launch
keeps the workers protected and everything the listener starts killable, which is
what this passage was describing in intent.

**Why in-process, not in the spawner.** A worker inherits its parent's score; the
parent here is the unprotected `cang enter` process, so the write has to happen
after the `fork`. Doing it in the worker (rather than from the spawner writing
the child's `/proc/<pid>/` entry) removes the window and needs no
post-`fork` async-signal-safe work in `pre_exec`.

**Tests** (`cargo test -p cang-guest-init`: 420 passed, 0 failed; clippy
`--all-targets --all-features -D warnings` clean; `cargo fmt --check` clean):
- `process::tests::{a_detached_worker_protects_itself_with_the_same_score_as_the_supervisor,
  a_worker_that_cannot_be_protected_reports_itself_by_name}`;
- `runtime::tests::{the_workers_a_session_depends_on_are_protected,
  maintenance_and_client_commands_stay_killable}` - the second is the table's
  control arm, parsed through the real CLI spellings `cang enter` passes.
- `docs/diagnostics.md`'s OOM section now says which processes are protected and
  why (unprotected workers leave "no swap at all, an unseeded overlay").

**Not established.** No VM was booted, so nothing here shows a worker surviving a
storm: the arm is
[05](05-response-arm-kit.md)'s storm with `/proc/<pid>/oom_score_adj` read back
from *every* guest-init process during the load (the note-36 `nice.txt`
precedent), and the guest console showing the victim was `rustc` rather than a
worker. That belongs to [01](01-memory-failure-ladder.md) /
[11](11-earlyoom.md)'s boots. `cargo deny check` was not run: no dependency
changed.
