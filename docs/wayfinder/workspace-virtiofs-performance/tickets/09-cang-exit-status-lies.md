---
label: wayfinder:task
title: Why does a guest that is still working, or already dead, look like a clean exit?
status: open
blocked_by: []
claimed_by: ""
---

## Question

Found while building the baseline (ticket 08, note §4 and "what is still open"):
**cang reports success for a guest that has not finished or has died.** Three
distinct observations, one cang-side cause:

1. `cang --image X -- <cmd>` can return **exit 0 about a second after boot while
   the guest command keeps running for another ~20 s** (a bench arm's work
   continues after the CLI is gone).
2. A guest command's **stdout is not forwarded** and its **exit status is not
   propagated**: `cang -- echo hi` prints nothing, `cang -- false` exits 0.
3. A guest that powers off without reporting a status becomes exit **0**:
   `vmm/mod.rs:502-525` falls back to `FC_EXIT_CODE_OK`, and `exit_code` is only
   ever written by the FUSE ioctl `VIRTIO_IOC_EXIT_CODE_REQ` (`0x7602`), which no
   cang code sends (`grep -rn 7602 crates/` is empty).

Decide and implement what cang owes a caller that runs `cang -- cmd`:

- **Waiting.** Does the CLI block until the guest command finishes (and how does
  it learn that - the exec protocol already carries a status?), or is fire-and-
  forget the intended contract, in which case the docs must say so and the
  benchmark harness's result-file protocol is the only way to observe completion?
- **Exit status.** `cang -- cmd` must exit with the guest command's status; a
  guest that dies without reporting must be a failure, not 0. Name the mechanism
  (exec protocol status vs the `0x7602` ioctl the fork already implements - using
  it would also give the VMM the real status).
- **Output.** A command's stdout/stderr must reach the caller.
- **A second launch into a busy workspace** must fail loudly instead of doing
  nothing (`cang` exits 0 with no output when a task for that workspace is
  already running), because that silently poisons any measurement.

Acceptance: `cang -- false` exits 1; `cang -- sh -c 'echo hi'` prints `hi`;
a killed guest exits non-zero; a launch into a busy workspace errors out; a guest
that boots and powers off with a failing payload does not exit 0.

## Resolution

(to follow)

### Evidence added while building the benchmark (2026-10-25/26, pi)

Measured in a fresh workspace, same image, payload written by the harness:

| payload | `cang` returns after | payload's writes on the host |
|---|---|---|
| `sh -c 'echo hi > /workspace/t.txt'` | 6.9 s | **missing** |
| `sh -c 'echo hi > /workspace/t.txt; sleep 2'` | 7.0 s | appears late (only after the next run started) |
| `sh -c 'sleep 8'` | 6.9 s | n/a |
| `sh -c '...; sleep 40'` | (killed) | present *during* the run |

So `cang` returns ~7 s after launch regardless of the payload, and a payload that
exits immediately can lose its last writes to the teardown - the virtio-fs device
is reset while requests are in flight, which is the same window as the `-5` burst
in note 08. A *failing* payload is worse: its stderr is discarded, so a build that
dies at its first line (`missing virglrenderer.pc`) is indistinguishable from a VM
that never started.

The benchmark harness works around all of it (result files with unique tags, an
inner log written into the shared workspace, a 10 s linger before the payload
exits, and a pre-flight refusal when the workspace already has a running task) -
but these are workarounds for defects this ticket has to fix, not contracts.

### Mechanism (2026-10-26, code read; no behaviour changed yet)

Three separate code paths explain everything measured above, and only the third
is a design gap rather than a bug:

1. **A guest *command* never reports its status.** `cang -- cmd` puts `cmd` in
   `plan.guest_command` (`launch/plan.rs:51`) and guest-init runs it; the host
   then only waits for the VM process
   (`supervisor/command.rs:216-230`: `child.wait()` -> `ChildStatus::exited(code)`
   of the *helper*). A guest that powers off cleanly gives 0.
2. **The managed-session path already propagates the guest's code.** When there is
   no guest command, cang attaches (the interactive shell) and
   `attach::..._with_exit_observer` reports the guest's exit code; it is persisted
   as `managed-exit.status` (`supervisor/command.rs:169`) and used in
   `managed_helper_exit_result` (`:371-376`: `if status.success() { return
   Ok(ChildStatus::exited(guest_code)) }`). So the *shell* path is honest; the
   command path is not.
3. **The intended mechanism is half-built in the fork.** `augment_fs.rs:753-758`
   handles `VIRTIO_IOC_EXIT_CODE_REQ = 0x7602` by storing the argument into the
   VMM's `exit_code` atomic, which `vmm/mod.rs:517-519` prefers over the vCPU
   exit code. **Nothing in the guest ever sends that ioctl** (`grep -rn 7602
   crates/` is empty), which is why a dead guest still reports 0.

#### Proposed fix (small, one side at a time)

- **guest-init**: after the guest command exits with status `n`, open an fd on the
  virtio-fs mount (e.g. `/workspace`, `O_RDONLY|O_DIRECTORY`) and
  `ioctl(fd, 0x7602, n)` before powering off - the fork's device intercepts it and
  the VM exits with `n`, which cang already maps to its own exit code. Nothing in
  the host needs to change; the ioctl is one syscall, and the FUSE ioctl opcode is
  already served (`server.rs`, `HAS_IOCTL_DIR` is advertised).
- **cang**: make a launch into a workspace with a running task **fail** instead of
  doing nothing (today: exit 0, no output).
- **cang**: forward a guest command's stdout (it is captured into
  `helper.stdout.log` today, so the data exists; the launch just does not relay
  it).

Open decisions for bob, because (1) changes user-visible semantics:
- Should `cang -- cmd` exit with the command's status (scripts then see failures),
  or keep today's "the VM finished" semantics and document it? The first is
  almost certainly what everyone assumes, but it *is* a behaviour change.
- Should a failing payload abort the run, or still allow the guest to finish its
  own teardown? (It must not lose the status either way.)
