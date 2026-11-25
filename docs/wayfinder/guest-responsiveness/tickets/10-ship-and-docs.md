---
label: wayfinder:task
title: Ship the defaults, the docs, and the acceptance numbers
status: closed
blocked_by: ["06-zram-and-swap-tunables", "07-component-boundary-and-opt-out", "08-guard-guest-init-processes", "09-rustc-job-cap"]
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

Land the component as the guest's default and record what it bought.

1. **Code**: the new component from [07](07-component-boundary-and-opt-out.md) wired into
   `runtime/cang.rs`'s profiled enter sequence (and the maintenance paths if the
   design says so), the numbers from [06](06-zram-and-swap-tunables.md), the
   guard from [08](08-guard-guest-init-processes.md), the bound from
   [09](09-rustc-job-cap.md). Every knob write testable against a temp path; the
   `cargo test` suite green.
2. **Gates**: `cargo fmt --check`, `cargo clippy --all-targets --all-features --
   -D warnings`, `cargo deny check`, `cargo test`, and `nixfmt` on any `.nix`
   touched (`AGENTS.md`).
3. **Docs**: `docs/images-and-storage.md`'s "Guest memory and zram swap"
   section gains the new policy and the measured numbers; `README.md` gains the
   one-line user-visible statement and the opt-out; `GLOSSARY.md` gains the
   terms this map introduced (*guest memory budget*, *zram budget*, *effective
   anonymous ceiling*, *responsiveness*) - implementation-free definitions only.
4. **Acceptance, on a live guest**: the arm kit's final run - the storm at the
   shipped defaults, VM alive, PTY and command-start distributions inside the
   bar set by [07](07-component-boundary-and-opt-out.md)/[05](05-response-arm-kit.md), and the
   numbers written down with raw log paths.
5. **Release note**: what a user notices, and what regressed (if anything) -
   a lever that was measured and refused belongs here, not in a note nobody
   reads.

The map is reached when this ticket closes with the numbers recorded.

## Resolution

Closed 2026-11-25. The map's destination is reached: the levers ship as one
default-on component with one opt-out, and the numbers are recorded in the
README, the docs and the glossary.

**Code (all in this worktree, uncommitted at close):**

| file | what |
|---|---|
| `crates/cang-guest-init/src/guest_init/runtime/cpu_bound.rs` (new) | `reserved_cores` (2 at >=3 vCPUs, 1 at 2, 0 at 1), the `0..N-3` mask, `confine_workload` |
| `crates/cang-guest-init/src/guest_init/components/responsive.rs` (new) | the `CANG_GUEST_RESPONSIVE` switch, the boot-time application, `/run/cang/responsive.status`, the early responder |
| `crates/cang-guest-init/src/guest_init/process.rs` | `become_workload()` - score reset **and** CPU bound in one call, at every seam; `protect_from_oom` |
| `.../runtime/exec.rs`, `.../runtime/session.rs`, `.../runtime/cang.rs`, `.../runtime/as_dev.rs`, `.../runtime/mod.rs` | the seams, the worker table, the two profiled boot steps |
| `crates/cang-guest-init/src/guest_init/components/shell/env.rs` | `CARGO_BUILD_JOBS` default-only, gated by the switch |
| `crates/cang/src/runtime/launch/config/components/responsive.rs` (new) | the host side: `CANG_GUEST_RESPONSIVE=0` crosses into the guest, nothing else does |
| `nix/image/layers.nix` | `pkgs.earlyoom` in the tooling layer |
| `tools/responsive-arm/` | the arm kit |
| `docs/diagnostics.md`, `docs/images-and-storage.md`, `README.md`, `GLOSSARY.md` | the user-facing record |

**Gates:** `cargo test -p cang-guest-init` **437 passed**, `-p cang --lib` **795
passed**, `-p cang-repository-tests` **67 passed**, clippy
`--all-targets --all-features -D warnings` clean, `rustfmt --check` clean on every
touched file, `cmd.deny` not run (no dependency changed; the one new package,
`pkgs.earlyoom`, comes from the pinned nixpkgs).

**Acceptance, on live guests** ([13](13-default-bound-and-earlyoom.md)): default
launch `nproc 24` / `0-23` with the supervisor on `0-25` and two cores near-idle;
`CARGO_BUILD_JOBS=24` and `/run/cang/responsive.status` agreeing; the early
responder killing a 3.2 GB presser at **9.96 %** of free memory with the zram
budget at **2 %** of its cap, `failed_writes` 0, `oom_kill` 0, the session surviving
and answering a command; and the survival side from [01](01-memory-failure-ladder.md)
§5-§6 plus [12](12-exec-inherits-oom-protection.md): a memory storm cannot end the
microVM through either launch shape.

**What the map ends with, recorded and refused** (the refusals are as much of the
answer as the adoptions):

| decision | verdict | ticket |
|---|---|---|
| affinity mask `0..N-3` for the workload | **shipped** | [04](04-workload-cpu-bound.md), [09](09-rustc-job-cap.md) |
| `CARGO_BUILD_JOBS = max(1, N-2)`, default-only | **shipped** (cannot cover nix) | [09](09-rustc-job-cap.md) |
| `cpu.max` / `cpu.weight` / whole-guest quota | **refused** (no idle cores; 2.15x wall) | [04](04-workload-cpu-bound.md) |
| earlyoom at 10 % of free memory, swap ignored | **shipped** | [11](11-earlyoom.md) |
| `vm.overcommit_memory=2` | **refused** (counts the zram device as capacity; no path to kswapd) | [03](03-overcommit-mode-verdict.md) |
| zram geometry, `swappiness`, watermark levers, MGLRU `min_ttl_ms` | **no change** (unearned by any arm) | [06](06-zram-and-swap-tunables.md), [02](02-kernel-knob-inventory.md) |
| sched_ext / scx | **out of scope** (firmware rebuild: BPF-JIT + BTF over a DWARF build) | MAP |
| kswapd/kcompactd placement, zstd `level` | **named, unearned** | [04](04-workload-cpu-bound.md), [06](06-zram-and-swap-tunables.md) |

**What is still owed, and is not a defect in this map** (they are the map's
*Not yet specified* items, each needing an arm no one has run): whether the guest
should reclaim its own page cache under its own pressure; whether swap should be up
before the workload starts; whether the host's reclaim thresholds should move now
that the guest answers pressure itself; and whether container workloads belong
inside the bound.
