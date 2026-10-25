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
