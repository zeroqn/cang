---
label: wayfinder:grilling
title: The component's shape, its opt-out, and where it runs on the enter path
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25, delegated by bob)"
---

## Question

There is to be **one** new default-on component in `cang-guest-init` and one
opt-out. Settle its shape, with bob:

1. **Name and ownership.** `components/responsive/` (a module with submodules,
   the `hardening/` precedent: `hardening/mod.rs` holds `allocator`, `dmesg`,
   `io_uring`, `perf`) versus spreading the work into the modules that already
   own the interfaces - `swap/zram.rs` owns the device, `process.rs` owns
   `oom_score_adj`, `shell/env.rs` owns exported environment. Which facts stay
   where, and what does the component itself own?
2. **Where the opt-out lives.** `CANG_GUEST_RESPONSIVE=0` cannot be read in the
   guest by accident: the host builds the guest environment from a curated list
   (`runtime/launch/config/guest_env.rs`, `config/mod.rs:53-75`, e.g.
   `CANG_GUEST_DEBUG` at `model.rs:58`) and does not inherit the caller's
   environment. So the opt-out needs a host-side read that inserts the guest
   variable (the `CANG_MEMORY_RECLAIM=0` precedent in
   `runtime/session/memory_reclaim.rs`), or a launch-config field. Which, and
   does the value need to survive into maintenance VMs too (`internal nix gc`,
   `internal resize`, `internal workspace free-bytes` - which run in a
   maintenance guest with no session)?
3. **When it runs, and what it costs.** The enter path is ~1.8 s and every step
   on it has been argued for (`runtime/cang.rs`'s profiler names each); sysctl
   writes are microseconds, a cgroup mount is not, and zram activation already
   left the path behind a `--after-exec` gate (`swap/zram.rs`,
   `cli.rs` `InternalSubcommand::Swap`). Which parts run in-process, which get
   a worker, and what is the measured cost of the in-process part?
4. **What it refuses.** A guest whose kernel lacks a knob (the maintenance VM,
   a different `--guest-init`) must not fail the session - the swap module's
   rule (`state=unavailable` in `/run/cang/swap.status`). Does the component
   write a status file of its own, and to which path under `/run/cang/`?
5. **Opt-out semantics.** "Default-on, off with `=0`": does an unparseable
   value keep the default (the `parse_env_flag` rule in
   `runtime/vm/libkrun/launcher.rs:240`) or refuse? Say it once, for this knob.

Resolution = a short design note `notes/07-component-shape.md` with the module
layout, the env contract both sides honour, the enter-path ordering, and the
VM-free test seams (every knob write must be testable against a temp path, as
`perf.rs`/`io_uring.rs` already are).

## Resolution

Closed 2026-11-25 with bob's delegation ("keep going alone"). The design note is
[`notes/07-component-shape.md`](../notes/07-component-shape.md); this is what
shipped.

**One module, at the seams.** `components/responsive.rs` owns the switch, the
boot-time policy application and the status file. The two shipped levers stay in
the modules that own their seams - `process::protect_from_oom` in `process.rs`
(every fork/exec transition lives there, with the score invariant's test) and
`runtime::cpu_bound::confine_workload` in `runtime/cpu_bound.rs` (it computes from
the guest's own vCPU count). Moving them for symmetry would churn code four arms
have measured.

**The switch.** `CANG_GUEST_RESPONSIVE`: absent/empty means on,
`0`/`false`/`no`/`off` means off, and **an unrecognised value keeps the default** -
the same matrix as the host's `parse_env_flag` (`launcher.rs:240-246`) and
`CANG_MEMORY_RECLAIM`, so a typo cannot silently move the guest either way. The
host reads it (`components/responsive.rs` in the host launch config) and sends
`CANG_GUEST_RESPONSIVE=0` into the guest environment **only when it disables** - the
guest's default is on, so that is the only value that has to cross, and the default
launch's guest environment stays byte-identical to before.

**The line the switch does not cross.** The OOM guard stays unconditional: a lever
trades throughput for latency or survival, the guard only prevents a crash, and
without it the guest loses its only victim and panics (`oom_kill.c:1159-1166`,
measured in [01](01-memory-failure-ladder.md) §6). `=0` disables what makes the
guest responsive, never what keeps it alive.

**Where it runs.** The switch is read in the guest at the seams, so a workload
start needs no host round trip; the status file is written once on the enter path
as its own profiled step (`record-responsiveness-policy`), after the hardening
steps and before the workload exists. `/run/cang/responsive.status` is
`key=value`: `state`, `cpus`, `reserved`, `mask`, `cargo_build_jobs`.

**Tests** (`cargo test -p cang-guest-init -p cang --lib`: 433 + 795 passed, 0
failed; rustfmt clean on every touched file): the switch's whole matrix including
the unrecognised-value case, the status body at 26/2/1 vCPUs, the directory
creation and the unwritable-path report, the host side's "only an explicit off
reaches the guest" (unset, empty, `1`, `true`, `banana` all send nothing), and the
existing shell-environment test now also proving the export is gated.

**Not established.** No live arm has booted with `CANG_GUEST_RESPONSIVE=0`: the
status file, the gated mask and the silent job-count export are VM-free claims
until an arm reads them from a guest. That arm is
[09](09-rustc-job-cap.md)'s (it boots the same binary with the switch on) plus one
`=0` boot, which the map's remaining grilling tickets can carry.
