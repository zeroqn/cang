---
label: wayfinder:note
ticket: 09-rustc-job-cap
---

# The job-count bound ships and is live-proven; the affinity half reaches only two of the three seams

Every number here came from a guest this arm booted (four boots, 14:34-14:55 local,
06:34-06:55 UTC). The ticket's question - is bob's rule really applied to the *workload* and not
to the session supervisor, and does the build still finish at least as fast - is answered below
for the shipped code in this worktree, and the answer has two halves that do not agree:

- **`CARGO_BUILD_JOBS=24` (default-only) is applied on every launch shape**, and live cargo
  honours it: 24 concurrent `rustc`, never 26, in every arm including the default (dev-identity)
  launch.
- **The affinity mask `0..N-3` is applied at two of the three "become the workload" seams and is
  MISSING at the default-launch seam.** A `--root` launch payload, and every `cang exec` child, run
  on `0-23` with two cores genuinely idle. A **default** `cang` launch (payload dropped to the dev
  identity) keeps `0-25`, leaves **no core idle** (least busy 54 %), and every `rustc` inherits that
  full mask - i.e. exactly the shape ticket 04 measured and called *not a CPU bound* (arm `A1b`).
  §6 names the file:line and the one-line fix.

Identity of the booted bytes: guest-init `/nix/store/dym3w8q1ga4kq68w2ry83877pb1n9l1d-cang-static-x86_64-unknown-linux-musl-0.23.6`
(derivation `/nix/store/laimbii5lpdq1h902cn85m4mv5xn0ax2-cang-static-x86_64-unknown-linux-musl-0.23.6.drv`,
src `/nix/store/d2y7wnmakxnvyhrkznvsry6nmpiz4sip-cang-workspace-source`), built from this worktree
with the check phase **on**: `running 428 tests` / `test result: ok. 428 passed` for
`cang_guest_init` and `31 passed` for `cang_guest_relay`, so no `doCheck = false` build was needed.
The src snapshot's `cpu_bound.rs` sha256 `1de25ffd…40f5b` and `shell/env.rs` `f714e3ae…ca410` are
byte-identical to the worktree files at build time; the ELF carries all three new literals
(`workload CPU bound not applied`, `failed to confine the workload to CPUs`,
`CARGO_BUILD_JOBS`). Host CLI: `/home/dev/cang/cang/result/bin/cang` 0.23.6 (`x9dflild0…`).
Guest image `localhost/cang:nixone` (`d755ca7fe741…`, 8.36 GiB, not rebuilt - same image ticket 04
booted). `--mem 8` in every arm; guest `MemTotal 8 149 960 kB`; host 28 CPUs so the guest is
**26 vCPUs** (`resolve_cpu_count` in `crates/cang/src/runtime/launch/config/components/resources.rs:8-15`
takes `available - 2`), kernel `7.3.5-hardened1` in all four boots, `vcpus_online=26`,
`nproc --all = 26`.

## 1. Where the shipped bound is called, and where it is not

Three "this process becomes the workload" seams exist in `crates/cang-guest-init`;
`runtime/cpu_bound.rs::confine_workload()` is called at two of them.

| seam | call site | carries | live evidence (§3) |
|---|---|---|---|
| `process::exec_command` (`process.rs:190`) | the raw `enter` path **and** the managed-session PTY child when `--root` keeps the payload as root (`cang.rs:445-448` -> `session.rs:1582`) | 0-23 | payload `Cpus_allowed_list 0-23`, `nproc 24` |
| `runtime/exec.rs::prepare_exec_child` (`exec.rs:117`, inside `pre_exec`) | every `cang exec` child, on **every** launch shape | 0-23 | 88/88 and 59/59 exec probes reported `EXECMASK=0-23 EXECNPROC=24 rc=0` |
| **`process::drop_to_identity_and_exec` (`process.rs:194-213`)** | the managed-session PTY child of a **default** launch (`should_drop_to_identity = is_root && !enter_as_root`, `cang.rs:641-643` -> `session.rs:1579-1580`) and `as-dev` (`as_dev.rs:48`) | **nothing** - it calls `allow_workload_kill()` and `apply_dev_credentials()` but not `confine_workload()` | payload `Cpus_allowed_list 0-25`, `nproc 26`, `sched_getaffinity 26` |

The half that *is* universal is the job count: `components/shell/env.rs::derive` adds
`CARGO_BUILD_JOBS` whenever the guest has not set it, and `derive` runs on the common enter path
(`cang.rs:203`) before either seam is chosen - so even the unbounded default launch gets
`CARGO_BUILD_JOBS=24`. That is why the default launch below looks *partly* bounded and is the
one shape where the ticket's two halves disagree.

Nothing in this change touches the supervisor: `session::run` still calls
`process::protect_from_oom("guest session supervisor")` and never `confine_workload()`, and the
`cang exec` listener thread it forks from keeps the full mask.

## 2. The measurement design (one guest at a time, control in the same window)

Same 1067-crate storm as ticket 04, byte-for-byte the same tree (`tools/rust-compile-bench/gen-workload.sh`
`LEAVES=800 MODULES=6 LINES=3000`, `700 MB` of source, copied from `.scratch/arm04-cpu-bound/ws04/wl`),
built cold into a fresh `/dev/shm/t-<arm>-<phase>` with `RUSTC_WRAPPER=` empty and
`CARGO_INCREMENTAL=0`, `cargo build --offline`. The telemetry is ticket 04's storm plus three
things ticket 09 needs: the storm process's own mask, **every observed `rustc` mask** (sampled
every 0.4 s), and the **supervisor's** mask sampled from inside a child of the storm for the whole
build.

Because the shipped code cannot be toggled per launch, one guest cannot be both bounded and
unbounded *as shipped*. Each boot therefore runs one or more phases; the pre-change shape is
reproduced **inside the guest** by the payload widening itself (`taskset -c 0-25`, `env -u
CARGO_BUILD_JOBS`) - process-level equivalence to the old code, in the same boot and the same
window:

- `wide` = control: full 26-CPU mask, `CARGO_BUILD_JOBS` unset (cargo picks 26).
- `bound` = as shipped: whatever mask/env the payload inherited.
- `intended` = the `0..N-3` rule applied by hand (`taskset -c 0-23`) in a default launch, to price
  the reserve that the missing seam costs.

Facts about the windows: one guest at a time, and `foreign_vms=0` on every sample in the three
boots the window sampler covers (`B-root`, `C-root`, `D-dev`; `logs/window.log`, whose gate excludes
this workspace's own `/home/dev/disp/cang/ws09/` task path). The host load of 18-25 seen while each
guest ran is **this guest's own 26 vCPU threads**: over 90 samples, load 0.38-25.36 while
`vm_workers=2` and 0.24-2.81 while `vm_workers=0`. `A-root` predates that sampler, so its only
foreign-VM record is the earlier filter's `FOREIGN_VMS=2`, which is *this* workspace's own VM worker
matching a pattern that did not yet exclude it - the same self-match defect note 04 §8 records, and
not evidence of a co-tenant.

## 3. Read-backs: the bound is on the workload, not the supervisor

`--root` launch (boot `A-root`, phases: one `bound` storm) - the seam the ticket's rule is meant
for, and the one a `--root` user gets:

```text
RESULT workload self pid=731 comm=bash nproc=24 cpus_allowed_list=0-23 py_sched_getaffinity=24 oom_score_adj=0
RESULT workload env CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=<unset> MAKEFLAGS=<unset>
RESULT supervisor pid=712 comm=cang-guest-init cpus_allowed_list=0-25 threads=4
RESULT ancestry_child_first 731:bash:0-23 712:cang-guest-init:0-25 1:init.krun:0-25
RESULT rustc_first pid=987 mask=0-23 chain_child_first 987:rustc:0-23 978:bwrap:0-23 944:cargo:0-23 940:python3.13:0-23 731:bash:0-23 712:cang-guest-init:0-25
RESULT storm max_concurrent_rustc=24 rustc_masks={"0-23": 1287}
RESULT storm supervisor pid=712 masks_during_storm={"0-25": 170}
```

- Workload: `nproc 24`, `Cpus_allowed_list 0-23`, `len(os.sched_getaffinity(0)) = 24`.
- Supervisor: `0-25` in every one of 170 samples during the storm, in the mid-storm lines
  (`supervisor_mid t=10s..80s pid=712 cpus_allowed_list=0-25`) and after it. **The reserve is
  correctly *not* on the interactive path.**
- Inheritance across fork+exec is proven on a real `rustc` (the same shape in every bounded arm -
  `C-root`'s `rustc 14018 -> bwrap 14009 -> cargo 13976 -> python3.13 13973 -> bash 733 (0-23)
  -> cang-guest-init 715 (0-25)`): 1287 mask samples of `rustc` all read
  `0-23`, and its ancestor chain goes `rustc -> bwrap -> cargo -> python3.13 -> bash (0-23) ->
  cang-guest-init (0-25)`: the payload and everything below it are confined, the supervisor above
  it is not. In the mid-storm and post-storm `supervisor_mid`/`supervisor_after` lines the
  supervisor never moved.
- `CARGO_BUILD_JOBS=24` is what the workload sees, `NIX_BUILD_CORES`/`MAKEFLAGS` are **not** set
  (matching the shipped change: one export only), and peak concurrent `rustc` is **24**, from two
  independent samplers (`storm9.py` 0.4 s: 24; the payload shell's own 1 s `pgrep`: 24).

Default launch (boot `D-dev`, no `--root`; the shape a user gets):

```text
RESULT guest kernel=7.3.5-hardened1 vcpus_online=26 cpuinfo_processors=26 nproc=26 nproc_all=26 uid=1000 user=dev
RESULT workload self pid=731 comm=bash nproc=26 cpus_allowed_list=0-25 py_sched_getaffinity=26 oom_score_adj=0
RESULT workload env CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=<unset> MAKEFLAGS=<unset>
RESULT supervisor pid=713 comm=cang-guest-init cpus_allowed_list=0-25 threads=4
RESULT phase=bound1 rustc_first pid=1004 mask=0-25 chain_child_first 1004:rustc:0-25 994:bwrap:0-25 958:cargo:0-25 955:python3.13:0-25 731:bash:0-25 713:cang-guest-init:0-25
RESULT storm max_concurrent_rustc=24 rustc_masks={"0-25": 1145}
RESULT storm cpus_busy_under_10pct=0 least_busy=('cpu0', 0.542)
```

The payload *and* every `rustc` run on **all 26 vCPUs**; only the job count is bounded. And
`cang exec` children are bounded even in this launch (`EXECMASK=0-23`, `EXECNPROC=24`, 59/59
`rc=0`) - which is the asymmetry that tells a user the bound is placement-dependent, not
launch-shape-dependent.

What the reserve is worth when it *is* there, measured in the same boot (`D-dev`, `intended2` =
`taskset -c 0-23` by hand, same `CARGO_BUILD_JOBS=24`): two cores drop to 5.2 % and 6.3 % busy
(`cpus_busy_under_10pct=2`, `least_busy=('cpu24', 0.052)`) while the other 24 sit at 0.90-0.92 -
against the shipped phase's `0 CPUs under 10 %` with `least_busy 0.542` on the same cores.

## 4. The build still finishes, and it is not slower

Every phase: `cargo_exit=0`, all 1067 crates compiled (`build.log` "Finished"), no arm discarded
for a short build. Wall = `wait4` window around `cargo build`; CPU = that child's rusage ticks
(10 ms); peak anon = 0.4 s `/proc/meminfo` sampler.

| boot | phase | shape | payload mask | max `rustc` | wall (s) | CPU ticks | peak anon (kB) | cores <10 % busy |
|---|---|---|---|---|---|---|---|---|
| A-root | bounded | `--root`, cold source | 0-23 | 24 | 71.459 | 148 040 | 3 123 500 | **2** (cpu24 .033, cpu25 .052) |
| B-root | wide1 | control, cold source | 0-25 | **26** | 68.265 | 151 715 | 3 317 488 | 0 (least .860) |
| B-root | bound2 | bounded, warm source | 0-23 | 24 | **65.043** | 139 441 | 3 156 612 | **2** (cpu25 .024) |
| C-root | wide1 | control, cold source | 0-25 | **26** | 67.090 | 149 829 | 3 335 400 | 0 (least .868) |
| C-root | bound2 | bounded, warm source | 0-23 | 24 | **62.475** | 133 753 | 3 093 280 | **2** (cpu24 .030) |
| C-root | wide3 | control, warm source | 0-25 | **26** | 63.589 | 146 585 | 3 309 036 | 0 (least .887) |
| D-dev | bound1 | **as shipped**, default launch | **0-25** | 24 | 62.411 | 133 054 | 3 104 708 | **0** (least .542) |
| D-dev | intended2 | 0-23 by hand, default launch | 0-23 | 24 | 61.832 | 133 820 | 3 091 520 | **2** (cpu24 .052) |

Cargo's own output agrees with the wall clock and with the workload's shape: every phase's
`build.log` holds `1067` `Compiling` lines and a `Finished \`dev\` profile [unoptimized] target(s)
in …` line - `1m 11s` (A-root), `1m 08s` / `1m 04s` (B-root wide1/bound2), `1m 02s` / `1m 03s`
(C-root bound2/wide3), `1m 02s` / `1m 01s` (D-dev bound1/intended2). No arm built a short tree and
none was warm-targeted: each phase was a cold `CARGO_TARGET_DIR`.

Read it in pairs, since each pair shares a boot and a window:

- **Equal warmth, in one boot** (`C-root`): bounded `62.475` vs control `63.589` = **-1.8 % wall**,
  `-8.7 %` CPU ticks (133 753 vs 146 585), `-6.5 %` peak anonymous (3.09 vs 3.31 GB). The bounded
  phase ran *first*, so the control had the warmer source cache - the comparison is conservative
  in the direction that matters (a slower bounded build could not hide behind warmth).
- **Cold source, in one boot** (`B-root`): control `68.265` -> bounded (warm) `65.043` = -4.7 %.
- **Cross-boot cold/cold**: bounded `71.459` (`A-root`) vs control `68.265`/`67.090`
  (`B-root`/`C-root`) is +4.6 %/+6.5 % for the bounded arm - i.e. the two cross-boot pairs disagree
  with the in-boot pair, which is the point of insisting on same-window controls: ticket 04's own
  two control boots span `68.41` and `59.64` s (14.7 %), so a cross-boot difference of this size
  is not an arm effect.
- **The default launch** (`D-dev`): with the shipped job-count-only bound `62.411`, with the
  intended mask `61.832` (-0.9 %). Adding the missing affinity seam therefore costs the default
  launch **nothing** in build time and buys the two idle cores.

Against ticket 04's recorded numbers: my cold bounded build is `71.5 s` and my cold control
builds are `67.1-68.3 s`, against note 04's control `59.64 s` and affinity arm `57.40 s`. My arms
are 5-20 % slower in absolute seconds and I do **not** read that as a regression, for three
reasons stated rather than hidden: (1) ticket 04's own control boots were `68.41` and `59.64`,
so single-boot wall time on this host already varies by ~15 %; (2) my guest carries work note 04's
did not - a `pgrep`/`awk`/`/proc` sampler every second on the same cores, and an `cang exec` every
2 s whose command was a `/bin/sh -c` running `awk` + `nproc` (note 04 exec'd `/bin/true`); (3) the
question ticket 09 asks is *relative* (does the bound slow the build down), and every in-window
pair says no. If a reviewer wants the ticket-04-scale absolute number, it needs note 04's exact
harness, not this one.

## 5. Nothing that depends on the full CPU set regressed

- **`cang exec`** (the second child of the same supervisor): 88/88 probes in `C-root`, 59/59 in
  `D-dev`, 32/32 in `A-root`, every one `rc=0`, `EXECMASK=0-23`, `EXECNPROC=24`, run **during**
  the storm (that is the ticket's "second exec while the storm runs"). Quiet-window exec
  (`PRE-STORM`) cost 51.0-65.3 ms; during-storm p50 106.9-140.9 ms, p90 165-253 ms. Those
  during-storm figures are **not** comparable to note 04's 37.6/52.1 ms: note 04 exec'd `/bin/true`,
  this arm exec'd a shell that forks `awk` and `nproc` inside a 24-`rustc` storm. `rc=0` on every
  probe is the regression check that matters here.
- **Console/PTY**: every arm answered its pings (88, 59, 32 samples; one lost ping per arm, on the
  storm's last second), `cang_exit=0`, and the payload kept reading stdin while the storm ran.
  Round-trip p50 was 6.3-24.0 ms (max 452-4450 ms); like the exec figures this is inflated by
  this arm's sampler living in the same shell loop that answers the ping, so it is a
  "the console still answers" check, not a latency comparison. Ticket 04's 1.95-2.17 ms p50 came
  from an arm whose responder loop did nothing but read.
- **Podman** (`D-dev`, the default launch): `podman load -i registry.tar` then
  `podman run --rm --network=none <id> /bin/registry --version` -> `load=ok run=ok
  /bin/registry github.com/docker/distribution 2.8.3`. The image's container/cgroup assumptions
  are untouched by the bound.
- **Nix**: present and answering in both launch shapes (`nix (Nix) 2.34.9`, `nix-daemon` pid,
  `show-config cores = 0 max-jobs = 1`). As the dev user the client prints Nix's own
  `warning: ignoring the client-specified setting 'store', because it is a restricted setting and
  you are not a trusted user` - pre-existing `NIX_REMOTE`/trust behaviour, not produced by this
  change, and the same daemon answer (`max-jobs = 1`) came back.
- **The cgroup tree is untouched**: `controllers=cpuset cpu io memory hugetlb pids`,
  `subtree_control=''` in every boot, and no arm used or needed a cgroup write - the shipped
  change adds none, exactly as note 04's §6 asked.
- **Kernel reclaim threads were left alone**: `kswapd0`/`kcompactd0` stayed `0-25` before and
  after every arm. Ticket 04 proved they are re-affinable (`taskset -pc 24-25` accepted); the
  shipped change does **not** do it, so that half of the ticket is prepared but not shipped (§7).
- No VM survived an arm: `cang ps` shows only the older `virtiofs-… pid-reused` record from
  another workspace after every arm, and `pgrep -af 'libkrun-vm-worker'` was empty at each gate.

## 6. What is not shipped: the reserve on the default launch

`process::drop_to_identity_and_exec` (`crates/cang-guest-init/src/guest_init/process.rs:194-213`)
is the only workload seam without the call, and it is the seam the **default** launch takes:

```text
session.rs:1579   if drop_to_identity {
session.rs:1580       process::drop_to_identity_and_exec(identity, permissions, command)
                  } else {
session.rs:1582       process::exec_command(command)          <- confined
                  }
cang.rs:641-643   fn should_drop_to_identity(is_root, enter_as_root) -> bool { is_root && !enter_as_root }
```

so `cang` with no `--root` (the documented default: "Enter the task shell as root instead of
dropping to the host/dev identity. By default, cang drops privileges") forks the payload onto the
PTY, drops it to the dev identity, and `exec`s it with the supervisor's own mask. Live proof is
§3's `D-dev` read-back (`0-25`, no idle core, 26-CPU `rustc` masks, `CARGO_BUILD_JOBS=24` still
exported).

The fix is the one line the other two seams already call, placed as `exec.rs:117` places it - after
`allow_workload_kill()`, before `apply_dev_credentials()` (the credentials drop is irrelevant to
affinity, but the affinity write must not be able to fail after the privilege drop):

```rust
    allow_workload_kill();
    // This process becomes the workload: off the cores the guest reserves, and
    // every child it forks or execs inherits the mask.
    crate::guest_init::runtime::cpu_bound::confine_workload();

    apply_dev_credentials(identity, permissions)?;
```

One call covers **both** remaining consumers - the managed-session PTY child of a default launch
(`session.rs:1580`) and `as-dev` (`as_dev.rs:48`) - and it is cheap: the `D-dev` `intended2` phase
shows the 0-23 mask costs the build 0.9 % wall and leaves the two cores idle, and `confine_workload`
is a single `sched_setaffinity(0)`. Two-unit-test additions follow from it for free: the existing
`cpu_bound` tests already exercise the syscall, and the seam becomes uniform, so a source-level
invariant test ("every caller of `drop_to_identity_and_exec`/`exec_command` is preceded by
`confine_workload`") becomes assertable.

This arm did not make that edit: the brief forbids editing `crates/` while other arms have
in-flight work there (`cang-guest-relay/src/server.rs`, the OOM-score rewrite in the same
`process.rs`, and a new `components/responsive` module landed in `cang.rs` during this campaign).
It is recorded as the ticket's one unmet ship condition, with the measurement that prices it.

## 7. The exports ticket 10 should list

Exactly what the shipped code sets, verified live in both launch shapes:

| variable | value | when | live read-back |
|---|---|---|---|
| `CARGO_BUILD_JOBS` | `max(1, nproc-2)` = **24** on this 26-vCPU guest | only when the guest has not set it (`shell/env.rs::bounded_job_count_default`); nothing on a 1-vCPU guest | `RESULT workload env CARGO_BUILD_JOBS=24`; `cargo` then held 24 `rustc` |
| `NIX_BUILD_CORES` | not set by cang | - (note 04 §5.5: it is per-builder cores, not a job count) | `<unset>` |
| `MAKEFLAGS` | not set by cang | - (`make` is MISSING from this image) | `<unset>` |
| the affinity mask | `0..N-3` (0-23 here) | at `exec_command` and `exec.rs::prepare_exec_child` only, **not** yet at `drop_to_identity_and_exec` | §3 |

An explicit `CARGO_BUILD_JOBS` from the user wins and changes nothing about the reservation: the
mask is applied regardless, and the export is skipped (`env_tests.rs` pins the default-only
behaviour; `sccache.rs` is the precedent).

## 8. What this does not establish

- **One boot per arm, and the arms ran in sequence, not interleaved.** The in-boot pairs are
  same-window; anything compared *across* boots here is a comparison, not a distribution, and
  ticket 04's own 14.7 % control spread is the scale of that noise.
- **`B-root`'s third phase was lost to this arm's own harness, not to the guest.** I killed the
  host harness with an errant `pkill -c -f harness9`; the `cang` launcher died of a closed stdout
  pipe mid-`wide3`, so `B-root` contributes one full control (`wide1` 68.265) and one full bounded
  phase (`bound2` 65.043) plus their results, and no third sample. The guest itself is not at
  fault and the two collected phases are complete builds (`cargo_exit=0`): `bound2`'s `storm.json`
  is stamped 14:44:15 and the ad-hoc host sampler's next sample, 14:45:09, already read
  `vm_workers=0` (`logs/window-B-root.log`) - a host-side teardown, not a guest fault.
- **The control shape is reproduced in-guest, not by old code.** `wide` = `taskset -c 0-25` +
  `env -u CARGO_BUILD_JOBS` inside the same guest, i.e. process-level equivalence to the pre-change
  26-CPU/26-job storm; it is not a run of the previous guest-init binary.
- **The source cache is cold only in the first phase of each boot.** Phases 2-3 read a warm
  source tree, which is why the same-boot comparison is stated with its ordering.
- **Host window:** no foreign VM in any sample and the 18-25 load was this guest's own vCPUs, but
  another agent on this host was building/booting intermittently (the source tree changed under
  this arm: `4f5332b8` landed during the build, and `components/responsive` appeared in `cang.rs`
  during the campaign). The build's src snapshot is named above; a rebuild now would be different
  bytes.
- **The interactive numbers are this arm's harness, not note 04's.** See §5: heavier proposer
  (`/bin/sh` + `awk` + `nproc`) and a sampler in the responder loop, so exec p50 107-141 ms and PTY
  p50 6-24 ms must not be read as regressions against note 04's 37-39 / 1.9-2.2 ms.
- **Same `--mem 8`, same image, same 26 vCPUs** as note 04, so the comparison is like-for-like in
  configuration; the *guest-init* binary differs (this worktree's build vs `az5idng9…`), which is
  the change under test, and the host CLI is the same 0.23.6.
- **A 2-vCPU guest was not booted.** `reserved_cores` gives mask `0-0`/`jobs 1` there and a 1-vCPU
  guest gets no bound and no export; that is unit-tested (`cpu_bound.rs::the_reserve_follows_the_guest_size`,
  `a_coreless_or_single_core_guest_gets_no_bound`) and not live-verified here.
- **No memory pressure in any arm**: `SwapFree` never moved and zram did nothing, so the kswapd
  half of ticket 04's rule is neither shipped nor priced here (tickets 06/11 own it).

## 9. Raw artifacts

`.scratch/arm09-job-bound/`:

| path | holds |
|---|---|
| `ws09/arm09/arm.sh`, `ws09/arm09/storm9.py` | the guest arm script (phases `bound`/`wide`/`intended`) and the storm with mask/supervisor telemetry |
| `harness9.py`, `run-arm9.sh` | host driver (PTY pings, `cang exec` probes, VMM log copier) and per-arm gate/runner |
| `out/<arm>/results.txt` | every in-guest `RESULT` line quoted above |
| `out/<arm>/<phase>/storm.json`, `build.log` | per-phase raw arrays, masks, rusage, cargo's own output |
| `out/<arm>/host-<arm>.json`, `console-<arm>.log` | host-timed ping/exec samples and the guest console |
| `out/<arm>/vmm/` | `helper.stderr.log` / `guest-kernel-console.log` copies taken while the VM was alive |
| `logs/gate.log`, `window.log`, `contamination.log`, `series.log` | exclusivity gate, host window (load and VM-worker count), foreign-VM samples, arm order |
| `logs/arm09-build-cang-musl.log`, `../arm09-build-cang-musl.log` | the build that produced the booted guest-init |
| `ws09/wl`, `ws09/arm09/registry.tar` | the 1067-crate workload tree and the podman image staged for the load/run check |
