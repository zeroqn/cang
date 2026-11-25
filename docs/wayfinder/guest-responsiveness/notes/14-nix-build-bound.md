---
label: wayfinder:note
ticket: 14-nix-build-inside-the-bound
---

# `nix build` is inside the CPU bound; the same change narrows the session payload by four cores

Six boots of this worktree (one guest at a time, every gate `foreign_workers=0`) answer the ticket.
The four claims the ticket asks for are **positive on a default launch**: the guest's `nix-daemon`
runs on `0-23` with its `oom_score_adj` still `-1000`, the builder the daemon forks runs on `0-23`
with its parent chain recorded, the build still produces its output, and the supervisor and a
`cang exec` child keep `0-25` / `0-23`. The Off control (`CANG_GUEST_RESPONSIVE=0`) moves the daemon
and the builder back to `0-25`, which is what makes the confinement a property of the new code
rather than of the launch shape.

One finding is **not** in the ticket's favour: the refactor that carries the bounds in
`process::WorkloadBounds` made the **managed session's payload** apply the mask **twice** - it reads
`Cpus_allowed_list 0-21` (22 vCPUs, four cores reserved) in every ON boot, while
`/run/cang/responsive.status` and `CARGO_BUILD_JOBS=24` still describe `0-23`. Details in §3.
That is a follow-up, not this ticket's question.

Identity of the booted bytes (this campaign overlapped other agents' edits, so each is named):

| | |
|---|---|
| guest-init | `/nix/store/zhkshlvwpznskv2afbhvw7d55j53m8r5-cang-static-x86_64-unknown-linux-musl-0.23.6/bin/cang-guest-init`, sha256 `f4489b882130add0e8db7c9a21dc6439cd28e550e2516796e4a6586aea552bde`; drv `/nix/store/171wq3dlbc49yqbsnc02y4x3mnb05n66-…drv`; src `/nix/store/xfjxixdddy4mppmki05jcn6y6zmf3nsf-cang-workspace-source` |
| check phase | on: `running 438 tests` / `test result: ok. 438 passed` and `running 31 tests` / `ok. 31 passed` (`nix log <drv>`), so no `doCheck = false` build was needed |
| src binding | sha256 src == worktree for `components/nix/root.rs a5eafe18…`, `process.rs 1c6b50be…`, `runtime/cpu_bound.rs 780d0d30…`, `runtime/session.rs f0a3c626…`, `runtime/exec.rs 07bdc365…` |
| image | `localhost/cang:nixone` ID `3f2be435d370`, digest `sha256:04b32a0fc16d3bd9664ead63f892d269e26641f658ac2beb8482e43a0eea96f8` - loaded, **not rebuilt** (the daemon and `nix` live in this image; no layer changed in this arm) |
| host CLI | boots A-E used the installed `result/bin/cang` (`/nix/store/x9dflild0m1ppfwl4l4pdhzhzdsa0zk3-cang-0.23.6`, 0.23.6) - see §4 for why boot F needed a rebuilt one |
| guest kernel | `7.3.5-hardened1`, 26 vCPUs (`/sys/devices/system/cpu/online=0-25`, `nproc --all 26`), host 28 CPUs |
| launch | `cang --image localhost/cang:nixone --guest-init <guest-init> --mem 4 -- sh /workspace/arm14.sh`, **no `--root`**: the default launch, payload dropped to the dev identity |

## 1. The change under test, as this tree spells it

`components/nix/root.rs::start_daemon` (`:674-687` at this checkout) resolves the range in the parent
and applies it in a `pre_exec` hook:

```rust
let cpu_range = crate::guest_init::runtime::cpu_bound::planned_workload_range();
let mut command = Command::new("nix-daemon");
command.arg("--daemon").env(DAEMON_SOCKET_ENV, NIX_DAEMON_SOCKET_PATH);
unsafe { command.pre_exec(move || crate::guest_init::runtime::cpu_bound::apply_workload_range(cpu_range)); }
command.spawn().context("failed to start nix-daemon")
```

The daemon is spawned by the `/nix` prep worker (`start_background_prep` -> `internal nix prep`,
`root.rs:167-181`), not by the session, which is exactly why it was outside the bound (note 04 §5.5).
`NIX_REMOTE=unix:///run/cang/nix-daemon/socket` is exported on the enter path (`runtime/cang.rs:356-357`)
so `nix build` from the payload **delegates** to that daemon and the builders are its children.

`planned_workload_range()` is `responsive::enabled()` then `workload_cpu_range(guest_cpu_count())`,
and `guest_cpu_count()` is `sysconf(_SC_NPROCESSORS_ONLN)`
(`runtime/cpu_bound.rs:66-73, 55-59`). `apply_workload_range` is one `sched_setaffinity` and allocates
nothing (`:77-95`). Neither the daemon's OOM score nor anything else about it is touched: it inherits
the prep worker's `-1000`.

## 2. The five readbacks, one row per boot

All arms are default launches with `--mem 4`, 26 vCPUs, and `foreign_workers=0` at the gate and after
teardown. `MASK` is `/proc/<pid>/status`'s `Cpus_allowed_list`, `adj` is `/proc/<pid>/oom_score_adj`.

| boot | arm | payload | supervisor | `nix-daemon` | builder the daemon forked | build | `cang exec` child |
|---|---|---|---|---|---|---|---|
| A `ws14` | ON | `0-21` | `0-25` (713) | **`0-23`** adj **-1000** (914) | not catchable (see §5.1) | rc=0 in 3.65 s, output `probe` (no wait, §5.1) | `0-23`, `nproc 24`, adj 0 |
| B `ws14b` | ON | `0-21` | `0-25` (716) | **`0-23`** -1000 (916) | `nix` 1054 `0-23` -1000 (builder failed to exec, §5.1) | rc=1 (instrument) | `0-23` |
| C `ws14c` | ON | `0-21` | `0-25` (722) | **`0-23`** -1000 (922) | `nix` 1063 `0-23` (same instrument failure) | rc=1 (instrument) | `0-23` |
| **D `ws14d`** | ON | `0-21` | `0-25` (715) | **`0-23`** -1000 (919) | **`sh` 1128 `0-23` -1000, 318 samples** | **rc=0, 36.9 s, output `probe`** | `0-23`, `nproc 24` |
| E `ws14e` | *intended OFF, flag never crossed* | `0-21` | `0-25` | `0-23` | `sh` 1125 `0-23`, 325 samples | rc=0, output `probe` | `0-23` (§4.1) |
| **F `ws14f`** | **OFF** | `0-25` | `0-25` (703) | **`0-25`** -1000 (911) | **`sh` 1123 `0-25` -1000, 321 samples** | **rc=0, 37.2 s, output `probe`** | **`0-25`, `nproc 26`** |

### 2.1 Claim 1 - the daemon is on the workload's mask, its protection is not

Boot D, verbatim:

```text
RESULT daemon pid=919 ppid=1 mask=0-23 adj=-1000 comm=nix-daemon cmd=nix-daemon --daemon
RESULT daemon_status Cpus_allowed=0ffffff Cpus_allowed_list=0-23 oom_score_adj=-1000 oom_adj=?
RESULT daemon_chain_child_first 919:nix-daemon:0-23:adj-1000 1:init.krun:0-25:adj0
```

`Cpus_allowed=0ffffff` is bits 0-23, i.e. 24 CPUs of the guest's 26 - the mask, not a quota. The
`adj -1000` is the prep worker's protection kept: it is the number the kernel uses to skip the daemon
when the guest is out of memory, and this change deliberately leaves it alone.
`ppid=1` is expected: `start_daemon` is called from the prep worker, which exits once `/nix` is
composed, so the daemon reparents to `init.krun` - the daemon is *not* reachable by walking up from
the payload, which is why the ticket's mechanism ("spawned from prep, not from the session") needed
its own readback rather than an ancestor chain.

The same line, OFF (boot F): `RESULT daemon_status Cpus_allowed=3ffffff Cpus_allowed_list=0-25
oom_score_adj=-1000`. The two arms differ in exactly the mask, and `-1000` holds in both - the OOM
guard is not a responsiveness lever (`components/responsive.rs:12-19` says so; this is the live
readback).

### 2.2 Claim 2 - a builder the daemon forks runs on the same mask

The daemon's descendants were sampled every 0.1 s for the whole build. Boot D:

```text
RESULT builder pid=1128 comm=sh masks=0-23 adj=-1000 samples=318 first_t=0.32 chain=1128:sh:0-23:adj-1000 1103:nix-daemon:0-23:adj-1000 919:nix-daemon:0-23:adj-1000 1:init.krun:0-25:adj0
RESULT sample_pair comm=nix-daemon mask=0-23 adj=-1000 pids=3 pids_list=919,1054,1103
RESULT build_samples count=990 window_s=36.83
```

Read the chain child-first: the builder `sh` (0-23) is the child of a `nix-daemon` *build* process
(1103, 0-23), which is a child of the daemon itself (919, 0-23), which hangs off `init.krun` (0-25).
All 318 samples of the builder read `0-23`; there is no second value in the set. The daemon also
forks an intermediate `nix` process for the build (seen in B/C/E, `nix 0-23`), and that one is
confined too.

The control is the same measurement in boot F: `builder pid=1123 comm=sh masks=0-25 adj=-1000
samples=321 first_t=0.38 chain=1123:sh:0-25:adj-1000 1098:nix-daemon:0-25:adj-1000
911:nix-daemon:0-25:adj-1000 1:init.krun:0-25:adj0`. Same chain shape, full mask. **The builder's
mask is the daemon's mask, and the daemon's mask is the new `pre_exec` write.**

### 2.3 Claim 3 - the confinement does not break nix

```text
RESULT build_exit rc=0 wall_s=36.874 stdout=/nix/store/li3ja44dbcll2ph7gy65y0nlawa8x5gg-cang-probe
RESULT build_output paths=/nix/store/li3ja44dbcll2ph7gy65y0nlawa8x5gg-cang-probe
RESULT build_output_check path=… is_file=True content=probe
```

The derivation is fetch-free (`builder = "/bin/sh"`, no inputs) and the client ran it with `--offline`
because this image's `/etc/nix/nix.conf` carries `substituters` the daemon would otherwise consult;
`nix build` still printed `this derivation will be built` and `building '<drv>'` in both arms, and the
output file holds `probe`. Boot A's build (`rc=0`, 3.65 s) produced its own output path with the same
content.

### 2.4 Claim 4 - the interactive seams did not regress

```text
RESULT supervisor pid=715 comm=cang-guest-init mask=0-25 adj=-1000
RESULT mask_watch label=supervisor samples=350 masks=0-25:350
RESULT mask_watch label=payload samples=350 masks=0-21:350
```

350 samples during boot D's build, one value: the supervisor never took the mask. The `cang exec`
probe is the driver's, run from the host against the live task (`host/cang-exec-1.log`):

```text
EXECMASK=0-23 EXECNPROC=24 EXECADJ=0          (boot D, rc=0)
EXECMASK=0-25 EXECNPROC=26 EXECADJ=0          (boot F, rc=0)
```

so the exec seam is confined ON and not confined OFF, and returns `rc=0` in both. The refactor's
`pre_exec` half is what carries it (`runtime/exec.rs:185-190` resolves, `:117-119` applies), and it
worked on every boot.

### 2.5 Claim 5 - the policy record agrees with the policy

```text
RESULT responsive_status state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24      (ON)
RESULT responsive_status_after state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24
RESULT responsive_status state=off cpus=26 reserved=0 mask=all cargo_build_jobs=        (OFF)
```

What it does **not** agree with is the payload's own mask - §3.

## 3. The finding: the session payload is confined twice and lands on `0-21`

Every ON boot read the same thing of the payload (boots B-F: 350-357 mask samples each, one
value):

```text
RESULT workload self pid=750 comm=python3.13 mask=0-21 adj=0 py_sched_getaffinity=22
RESULT guest nproc_cli nproc=22 nproc_all=26
RESULT ancestry_child_first 750:python3.13:0-21:adj0 735:sh:0-21:adj0 716:cang-guest-init:0-25:adj-1000 1:init.krun:0-25:adj0
RESULT guest env NIX_REMOTE=unix:///run/cang/nix-daemon/socket CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=<unset>
```

24 CPUs is the policy (`mask=0-23`); 22 is what the payload got - four cores reserved instead of
two, two fewer than `CARGO_BUILD_JOBS=24` assumes, and the `nix` client (`build_started
client_chain=1084:nix:0-21 …`) narrower than the daemon it delegates to (`0-23`). `nproc --all` is
still 26 and `/sys/devices/system/cpu/{online,possible,present}=0-25`, so this is a mask, not an
offline-CPU lie.

### 3.1 Mechanism, from this tree

The change adds one apply in the fork child and **keeps** the one the later seam makes:

```text
runtime/session.rs:1518   let bounds = process::WorkloadBounds::prepare();   // supervisor, mask 0-25 -> (0,24)
runtime/session.rs:1530   if let Err(err) = bounds.apply() { … }             // fork child: mask 0-23
runtime/session.rs:1586   process::drop_to_identity_and_exec(…)              // default launch
runtime/session.rs:1588   process::exec_command(…)                           // --root launch
process.rs:272            become_workload();                                 // inside drop_to_identity_and_exec
process.rs:253            become_workload();                                 // inside exec_command
process.rs:217-221        become_workload() { WorkloadBounds::prepare().apply() … }
```

`git diff` on `session.rs` shows the refactor *added* `bounds.apply()` where the code previously had
only `process::allow_workload_kill()`; `become_workload()` still resolves its own plan
(`prepare()` -> `planned_workload_range()` -> `guest_cpu_count()` -> `sysconf`), and it runs in the
child that has just been narrowed. That second resolve is where the second two cores go: **musl's
`sysconf(_SC_NPROCESSORS_ONLN)` counts the bits of `sched_getaffinity`**, so a process already masked
to 24 CPUs reports 24, `workload_cpu_range(24)` is `(0,22)`, and `Cpus_allowed_list` becomes `0-21`.

The libc fact is not folklore: `src/conf/sysconf.c` in musl v1.2.6 handles
`_SC_NPROCESSORS_ONLN`/`_SC_NPROCESSORS_CONF` by calling `__syscall(SYS_sched_getaffinity, 0, sizeof
set, set)` and counting the set bits (fetched from git.musl-libc.org at tag `v1.2.6`; the guest-init
binary is the musl target's static link). The live discrimination between the two libc behaviours is
in the same boot: **glibc reads 26** (`RESULT sysconf_view
glibc_SC_NPROCESSORS_ONLN=26 cpu_count=26 affinity=22`) while the payload is on 22 CPUs - i.e. the
python child, glibc, is blind to the mask; guest-init, musl, is not.

Why the mask is `0-21` rather than `0-23` is then a two-step arithmetic with no other candidate:
26 -> `(0,24)` in the supervisor, 24 -> `(0,22)` in the child. The exec seam shows the single-apply
value in the same boot (`EXECMASK=0-23`), and the OFF boot shows `0-25` in both seams because every
plan is `None` there - so neither a wider default nor an inherited mask explains `0-21`.

### 3.2 What it costs, and the shape of the fix

- The workload loses **two cores beyond the policy**: 22 instead of 24 on a 26-vCPU guest, in every
default launch. On a 4-vCPU guest the first apply gives `0-1` and the second gives `0-0` (one core);
on a 3-vCPU guest, `0-0` then `reserved_cores(1) == 0` -> no further narrowing, so the compounding
stops at one core. `reserved_cores` is `2/1/0` for `>=3 / 2 / 1` (`runtime/cpu_bound.rs:27-35`),
so the second step is a no-op only at the small end.
- `/run/cang/responsive.status` reports `mask=0-23` (it is written once at boot in the supervisor,
`components/responsive.rs:60-86, 89-96`) and `CARGO_BUILD_JOBS=24` is exported from the same 26-CPU
reading (`components/shell/env.rs:57-66`) - so the record, the job count and the payload disagree,
and a job count of 24 against 22 allowed CPUs is a 2-job oversubscription.
- The two seams that are *correct* are the ones that apply once: `cang exec` (0-23, proven above) and
  the daemon's `pre_exec` (0-23).

The fix shape (not made here - `crates/` is off-limits to this arm): apply the bounds **once per
seam**. Either pass the resolved `WorkloadBounds` down to `child_main_result` and have
`drop_to_identity_and_exec`/`exec_command` take `Option<WorkloadBounds>` (`None` = "the fork child
already applied it", the raw `enter`/`as-dev` paths keep resolving in-process), or restore
`allow_workload_kill()` in `spawn_pty_child` and let `become_workload()` be the single apply as it
was before the refactor (which reintroduces the env read + `sysconf` between `fork` and `exec` that
the refactor exists to remove). The first keeps the refactor's rule and fixes the mask; a raw
`--root` launch was **not** measured here, but it takes `exec_command` after the same fork-child
apply, so it should read `0-21` as well until one of these lands.

## 4. The OFF control, and why it needed a rebuilt host CLI

### 4.1 Boot E: the flag did not cross

The installed `result/bin/cang` (`/nix/store/x9dflild0m1ppfwl4l4pdhzhzdsa0zk3-cang-0.23.6`) is one
build older than the forwarding code: `grep -c CANG_GUEST_RESPONSIVE result/bin/cang` = **0**. The
host half that should turn a host-side `CANG_GUEST_RESPONSIVE=0` into a *guest* environment pair is
`crates/cang/src/runtime/launch/config/mod.rs:68-70` (`components::responsive::guest_env_pair`), and
that literal is not in that binary. So boot E - launched with `CANG_GUEST_RESPONSIVE=0` in the
launcher's environment - read exactly the ON shape (`payload 0-21`, daemon `0-23`, status `state=on`).
**Boot E is not an OFF arm**; it is the record that the installed CLI predates the forwarding, and it
is why §2's table lists it as such rather than as a control.

### 4.2 Boot F: rebuilt from this worktree

`nix build .#cang` (70 s) produced `/nix/store/qd0sl4inzvxsxfyb3b2l3xw4balbl3rb-cang-0.23.6`, whose
derivation references the same source drv (`sgx2slkm…-cang-workspace-source.drv` -> src
`xfjxixdddy…`), with `grep -c CANG_GUEST_RESPONSIVE <bin>/cang` = 2 and src-vs-worktree sha256 equal
for `config/mod.rs` and `config/components/responsive.rs`. That binary, with
`CANG_GUEST_RESPONSIVE=0`, produced the OFF arm of §2: `state=off cpus=26 reserved=0 mask=all
cargo_build_jobs=`, payload and supervisor and daemon and builder and exec all `0-25`, the daemon and
builder still `-1000`. The build ran and produced `probe`.

Note the asymmetry the OFF arm also proves cheaply: no `CARGO_BUILD_JOBS` is exported (`<unset>`),
which is the "off means off" half of `components/shell/env.rs:57`.

## 5. What this arm does not establish

1. **The failed waits are an instrument failure, not a nix finding.** The prescribed wait
   (`sleep 8`) is not reachable from a raw derivation's builder in this guest: a bare `sleep` gets no
   `PATH` (boot A, `sleep 8; echo probe > $out`: the build "succeeded" in 3.65 s because the shell
   finished the line anyway), and `/bin/sleep` (a dangling `-> coreutils` symlink in this image)
   plus the coreutils store path behind it both fail `sh: <path>: not found` inside the builder
   (boots B and C, `sleep 8 && …`, exit 127). The `nix`-only catches in B/C are still readbacks of
   the daemon's subtree. Boots D/E/F
   use a shell-only spin loop (`i=0; while [ $i -lt 30000000 ] …`, 36.9-37.7 s measured) and are the
   builder evidence. A diagnostic derivation that dumps the builder's `/`, `/bin` and `/nix/store`
   view also failed 127 (boots D/F), so **what the sandboxed builder can see is not characterised
   here** - only that it runs `/bin/sh` and nothing from coreutils that this arm tried.
2. **One boot per shape, and the host was not idle.** Gates were `foreign_workers=0` on every boot
   (and `pgrep_x_cang` 0 except boot C's 5, i.e. winding-down launcher processes with no VM worker;
   the driver's `post cang ps` is empty after each arm), but host load at boot ranged
   `0.49`-`8.05` (`host/driver.log` records it per boot). No timing claim is made anywhere here;
   the build walls (36.9/37.2 s, both arms, identical spin) are quoted as "the builder lived long
   enough to sample", not as an arm comparison.
3. **The daemon's *concurrency* half is untouched.** `nix show-config` in this image reports
   `max-jobs = 1`, so the "many concurrent builders" worry behind note 04 §5.5 is not exercised -
   what is proven is that the one builder and every process under the daemon inherit its mask.
4. **No `--root` arm, no `as-dev` arm, no `--workspace-disk` arm.** The default launch is the shape
   measured, and §3.2 says what the `--root`/`as-dev` paths should read, from source, not from a boot.
5. **No per-CPU busy fraction and no memory counters.** This arm establishes masks, scores and process
   parentage; the reserve's *value* (idle cores, throughput, peak anon) is note 04/09/13's measurement,
   and the early responder was not in play here.
6. **Boot A's probe revision mislabels one line.** Its `RESULT supervisor pid=732 comm=sh
   mask=0-21` is the payload's own parent, not the supervisor; the real supervisor is the
   `cang-guest-init` entry of the same boot's `ancestry_child_first`
   (`713:cang-guest-init:0-25:adj-1000`), and boots B-F label it correctly (`comm=cang-guest-init`).
7. **Nothing about the kernel.** `guest-kernel-console.log` is 0 bytes in every arm and the captured
   `helper.stderr.log` copies hold only libkrun's recurring `vsock::muxer unexpected dgram pkt: 3`
   line (the same harmless noise note 09/13 record).

## 6. Raw artifacts

`.scratch/arm14/` (one directory per boot):

| path | holds |
|---|---|
| `ws14/`, `ws14b/`, `ws14c/`, `ws14d/`, `ws14e/`, `ws14f/out/results.txt` | every `RESULT` line of that boot, read from the guest's `/workspace/out/results.txt` |
| `.../host/driver.log` | gate (`foreign_workers=`, host load), launch line, task id, `cang exec` rc + output, done sentinel, teardown, `foreign_workers_after` |
| `.../host/cang-session.log` | the guest console for that arm (the payload's own `tee` of every RESULT line) |
| `.../host/cang-exec-1.log` | the exec seam probe's `EXECMASK=… EXECNPROC=… EXECADJ=…` |
| `.../host/vmm/*.helper.stderr.log`, `*.guest-kernel-console.log` | the live VMM console copies (0-byte kernel log, vsock noise only) |
| `probe14.py`, `probe14d.py`, `arm14.sh`, `run.sh` | the guest probe (both revisions), the payload, and the host driver (adapted from `tools/responsive-arm/run-arm.sh` with the exec probe added) |

The arm directories are scratch, not repo content: `ws14`-`ws14f` are per-boot workspaces and can be
deleted with `rm -rf .scratch/arm14` once the numbers are no longer needed (ask before doing it -
the boot evidence only exists there).
