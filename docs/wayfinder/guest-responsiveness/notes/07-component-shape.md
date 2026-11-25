---
label: wayfinder:note
ticket: 07-component-boundary-and-opt-out
---

# The responsive component's shape: one switch at the levers' seams, a status file, and a line the switch does not cross

Decision note. bob delegated the call on 2026-11-25 ("keep going alone"), so this
records the shape and the reasons a reviewer can argue with, not a proposal.

## 1. One module, at the seams, not a framework

`components/responsive.rs` owns three things and nothing else:

1. **the switch** - `CANG_GUEST_RESPONSIVE`, resolved once per process;
2. **the boot-time policy application** - today a status file; later ticket
   [06](../tickets/06-zram-and-swap-tunables.md)'s sysctls and
   [11](../tickets/11-earlyoom.md)'s responder start;
3. **`/run/cang/responsive.status`** - what the guest decided, readable by an arm.

The two levers already shipped stay where they are:

| lever | home | why not here |
|---|---|---|
| OOM protection (`protect_from_oom`) | `process.rs` | that file owns every fork/exec transition and the score's invariant test; the seam *is* the interface |
| CPU bound (`confine_workload`) | `runtime/cpu_bound.rs` | it computes from the guest's own vCPU count, and `runtime/` is where the launch mode lives |

Moving either for symmetry would churn code that four arms have now measured,
and the decision ladder's first rung is "does this need to exist". A module that
gates two functions and writes one file earns its place; a module that relocates
them does not.

## 2. The switch, and the rule it shares with `CANG_MEMORY_RECLAIM`

`CANG_GUEST_RESPONSIVE=0` (or `false`/`no`/`off`) turns the levers off; unset means
on; **an unrecognised value keeps the default**, matching the host's own
`parse_env_flag` matrix (`crates/cang/src/runtime/vm/libkrun/launcher.rs:240-246`)
and `CANG_MEMORY_RECLAIM`'s documented behaviour. A typo must not silently disable
a guest's safety net, and it must not silently enable one either - the default is
what an operator gets when the knob is not understood.

The host reads it (host environment, like `CANG_MEMORY_RECLAIM`) and inserts it
into the curated guest environment **only when it disables**: the guest's default
is on, so `CANG_GUEST_RESPONSIVE=0` is the only value that has to cross the
boundary. That keeps the default launch's env byte-identical to today's, which is
also what keeps the existing guest-env tests meaningful. It applies to every
launch, maintenance VMs included, where the levers are simply inert (no workload,
so no mask to apply) and the status file records `state=off` or `state=on` so a
maintenance arm can tell why its `nproc` is the full set.

## 3. What the switch does **not** cover

**The OOM guard stays unconditional.** A lever trades some build throughput for
latency or survival; the guard only prevents a crash. Concretely, without it the
guest loses its only OOM victim and panics (`oom_kill.c:1159-1166`, measured in
[note 01](01-memory-failure-ladder.md) §6 and [note 12](12-exec-oom-inheritance.md)).
Nothing an operator would want from `=0` includes "let the microVM panic instead of
killing my compiler", and a switch that can be set wrong into a kernel panic is the
kind of knob this repo does not ship. So the line is:

> `CANG_GUEST_RESPONSIVE=0` disables the things that make the guest *responsive*.
> It never disables the things that keep it *alive*.

## 4. Where it runs, and what it costs

- The **switch is read in the guest**, at the seams, so no host round trip is
  needed for a workload start to be bounded: `confine_workload` and the
  `CARGO_BUILD_JOBS` derivation each ask `responsive::enabled()` first.
- The **status file is written once at boot**, on the enter path. It is one small
  file under `/run/cang/`, next to `swap.status` and `nix-prep.status`; the enter
  path's profiler already attributes each step, so a regression there would show up
  as a named stage rather than a mystery.
- Both are best effort, in the same sense as the levers themselves: a guest that
  cannot write `/run/cang` still runs, and the levers still apply.

## 5. Refusals and edges

- A guest whose kernel lacks something a lever needs applies what it can and
  records the rest - never fails the session (the `state=unavailable` rule the swap
  module already uses).
- `nproc` 1 reserves nothing, so with the switch on the status file still says
  `state=on` and `reserved=0`; the guest's `nproc` is the whole story there.
- The status file's format is `key=value` lines, like `/run/cang/swap.status`:
  `state`, `cpus`, `reserved`, `mask`, `cargo_build_jobs`, and the levers' own
  fields as [06](../tickets/06-zram-and-swap-tunables.md) /
  [11](../tickets/11-earlyoom.md) add them.

## 6. Test seams

Every decision here is a pure function of an input string, a CPU count and a path,
so the VM-free tests are the usual shape for this crate: `enabled_from(Option<&str>)`
over the whole matrix, `status_body(cpus)` over 1/2/26 vCPUs, and the host-side
guest-env test that asserts `CANG_GUEST_RESPONSIVE` appears **only** when the host
sets it to a falsy value. The live claim stays the arm's.
