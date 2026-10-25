---
label: wayfinder:task
title: Does a cold full-size cargo build in /workspace still hit ENOENT, and if so why?
status: closed
blocked_by: []
claimed_by: pi (2026-10-25)
---

## Question

The original symptom that opened ticket 08 - `cargo`/`rustc` failing with
`ENOENT` on `target/` directories it had just created in `/workspace` - was never
reproduced in six clean arms (mini workload, one VM at a time). It may have been
an artefact of a session that left several VMs pointing at the same workspace (a
launch into a busy workspace does nothing, and an interrupted run leaves an
orphan VM behind), or it may be a genuine guest-side failure that only the
full-size workload provokes.

Settle it with the real workload:

1. One cold full-size `guest-virtiofs` arm (2401 crates, 7202 `target/debug/deps`
   files) with the harness, `GUEST_TIMEOUT=1200`, in a clean window, one VM only.
2. If it fails: capture the failing `cargo` output (the inner script already keeps
   `bench-build.log` next to the arm), the guest kernel console, and the VMM log;
   then reduce it to a guest-side shell repro (mkdir + immediate `openat` in the
   new dir; `readdir` right after `mkdir`; `rename` into a fresh dir; nested
   `mkdir -p`; single-threaded vs parallel writers) and name the layer:
   passthrough `mkdir`/`lookup`/`create` ordering, the 5 s negative-entry cache,
   or something outside virtio-fs.
3. If it passes three runs in a row, close this as "not reproducible outside a
   multi-VM session" and record the guard: the harness refuses a workspace with a
   running task, and every arm kills its own VM.

## Resolution (2026-10-25/26, pi)

**Not reproducible with the full-size workload: closed.** Three cold
`guest-virtiofs` arms over the real tree (2401 crates, 16801 sources, ~7200
`target/debug/deps` files) completed in **298.9 s / 306.5 s / 311.4 s**, each
writing its result file, each with `grep -c enqueue_req` = 0 in its task's VMM
log, no `ENOENT`, and `cang ps` clean afterwards.

The earlier `ENOENT` came from a session that ran several VMs against the same
workspace concurrently - and a launch into a workspace that already has a running
task **does nothing at all** (exit 0, no output), while an interrupted run leaves
its VM behind, so two "runs" can share one workspace and one of them builds into a
tree another VM is mutating. The guard is now in the harness
(`require_workspace_free`) and one VM at a time is a stated precondition of every
number in this map.

What did *not* explain it, checked and recorded so it is not re-derived: the
`virtio_fs_enqueue_req() failed -5` burst (guest shutdown noise, note 08), queue
saturation (that is `-ENOSPC`, deferred by the guest and never logged), and a
malformed used ring (`BAD_RING` never appears in any captured log).
