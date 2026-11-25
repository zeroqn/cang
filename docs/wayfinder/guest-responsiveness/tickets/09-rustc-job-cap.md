---
label: wayfinder:task
title: Leave the cores free - the build parallelism bound that ships
status: closed
blocked_by: ["04-workload-cpu-bound"]
claimed_by: "pi (agent, 2026-11-25)"
---

> **Correction, 2026-11-25 (later the same day).** The `nix build` gap this note
> records is closed: the guest confines `nix-daemon` at spawn
> (`components/nix/root.rs::start_daemon`), so the daemon and every builder it
> forks for `nix build` run on the workload's cores. Live proof - five ON boots
> and one OFF control - in [note 14](14-nix-build-bound.md); what it costs
> (`nix eval`/`nix develop` are confined too) is in
> [docs/images-and-storage.md](../../../../docs/images-and-storage.md).
>
## Question

Ship bob's rule - at least 2 cores free of rustc, `max(1, nproc - 2)`, with
`nproc = 2` giving 1 and `nproc = 1` giving none - in the shape
[04](04-workload-cpu-bound.md) recommends, and prove it does not slow the build
down more than it buys in responsiveness.

Whichever shape wins, this ticket also settles:

1. **Where the value is computed.** In the guest, from its own `nproc` (it
   equals the vCPU count the host chose, `resources.rs:8-15`) - not from a host
   number, because the guest is what spawns the compilers.
2. **Reclaim runs on the reserved cores too.** The cores left free should not
   be taken by kernel reclaim either: when the guest swaps, the page is
   compressed in the *writing* context (`zram_submit_bio` is synchronous,
   `zram_bio_write` -> `zcomp_compress`, `zcomp.c:142`), and `kswapd0`
   is a normal-priority per-node kthread (`kthread_create_on_node`,
   `mm/vmscan.c:7702`, no nice, no affinity) - so its zstd work lands on whatever
   CPU rustc is on. `taskset`-style placement of `kswapd0` (and `kcompactd0`)
   onto the reserved cores is the matching half of the rule; prove it is
   settable in this guest (a kthread that bound itself with
   `PF_NO_SETAFFINITY` cannot be) and measure it, because it is cheap and
   untested.
3. **Default-only, never overridden.** Export only when the variable is unset,
   so a user's explicit `CARGO_BUILD_JOBS` wins (`components/sccache.rs` is the
   precedent for the "only when absent" rule), and say what an explicit value
   does to the reservation (nothing - it is the user's call).
3. **Which tools.** `cargo` alone, or `NIX_BUILD_CORES`/`MAKEFLAGS` too. Bob's
   preference is coverage of any workload; `nix build` is the other big CPU
   consumer in this guest, so state the evidence either way.
4. **The build cost.** An arm with the same workspace build at the
   unrestricted job count and at the bound: wall time, plus the interactive
   distributions from [05](05-response-arm-kit.md). A bound that slows the build
   by a third to protect a keystroke nobody typed is a bad trade and must be
   recorded as one.

Deliverable: code plus `notes/09-job-bound.md` with the before/after table, and
the exact exports listed for [10](10-ship-and-docs.md).

## Resolution

Shipped and live-proven: **`CARGO_BUILD_JOBS = max(1, nproc-2)` (24 on this 26-vCPU guest), set only
when the guest has not, plus the `0..N-3` affinity mask at the `exec_command` and `cang exec`
seams.** Not shipped, and measured as missing: **the same mask on a default launch** (the
`drop_to_identity_and_exec` seam). Full evidence, four boots, 14:34-14:55 local:
[notes/09-job-bound.md](../notes/09-job-bound.md).

1. **Where the value is computed** - in the guest, from its own CPU count
   (`runtime/cpu_bound.rs::guest_cpu_count`), not from a host number; on this host that is 26
   (`resources.rs:8-15` takes `available - 2` of 28), so the mask is `0-23` and the job count 24.
   Live: `--root` payload `nproc 24 / Cpus_allowed_list 0-23 / sched_getaffinity 24`;
   default-launch payload `nproc 26 / 0-25 / 26` (§3).
2. **The reserve is on the workload and only on it.** The supervisor `cang-guest-init` read
   `Cpus_allowed_list 0-25` in **170 samples during a bounded storm** and in every mid-storm line;
   a real `rustc` read `0-23` with its chain `rustc -> bwrap -> cargo -> python3.13 -> bash (0-23)
   -> cang-guest-init (0-25)`, and 1287 `rustc` mask samples were all `0-23`. The `cang exec` child
   is `0-23` while the listener thread that forked it stays `0-25` (88/88 probes `rc=0`,
   `EXECMASK=0-23`, during the storm).
3. **Default-only export, never overridden.** `CARGO_BUILD_JOBS=24` is what the workload sees;
   `NIX_BUILD_CORES` and `MAKEFLAGS` are not set by cang (`<unset>` live), matching §5.5 of note 04
   (nix's job count is the daemon's `max-jobs = 1`, not `cores`). An explicit user value wins and
   says nothing about the reservation; the mask applies either way.
4. **The build does not pay for it.** Same 1067-crate storm as note 04, same boot, same window:
   bounded `62.475 s` / 133 753 ticks / 3.09 GB peak anon vs control (wide, `CARGO_BUILD_JOBS`
   unset, 26 `rustc`) `63.589 s` / 146 585 ticks / 3.31 GB = **-1.8 % wall, -8.7 % CPU, -6.5 %
   anonymous**, with the two reserved cores at 2.4-6.3 % busy against the control's least-busy
   86-89 %. Cold-source cross-boot numbers (bounded `71.5`, controls `67.1`/`68.3`) are inside
   ticket 04's own 14.7 % two-control spread and are reported as a comparison, not a regression.
   The default launch's missing mask costs nothing to add: `intended` (0-23 by hand) `61.832 s`
   against as-shipped `62.411 s` in one boot.
5. **The unmet ship condition, named so it can be fixed in one line:**
   `crates/cang-guest-init/src/guest_init/process.rs:194-213` (`drop_to_identity_and_exec`) calls
   `allow_workload_kill()` and `apply_dev_credentials()` but not `confine_workload()`; add it before
   the credential drop (as `runtime/exec.rs:117` already does). One call fixes both consumers of
   that seam - the managed-session PTY child of a no-`--root` launch (`session.rs:1580`) and
   `as-dev` (`as_dev.rs:48`). Live consequence today: a default launch keeps `0-25`, leaves **no**
   core idle (least busy 54 %), and its `rustc` children inherit the full mask - note 04's `A1b`
   shape, which is why the job-count export alone cannot be called a CPU bound.
6. **Left for 10 (`ship-and-docs`)**: state the two half-bounds above separately in the docs (the
   job count covers every launch, the mask covers `--root`/`cang exec` until item 5 lands), and do
   **not** advertise `NIX_BUILD_CORES`/`MAKEFLAGS` - neither is set. The kswapd half of note 04
   (`kswapd0`/`kcompactd0` -> `24-25`) is settable in this guest but not shipped here.
