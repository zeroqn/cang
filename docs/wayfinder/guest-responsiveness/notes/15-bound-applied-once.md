---
label: wayfinder:note
ticket: 15-workload-bounds-applied-twice
---

# The bound is applied once: payload `0-23` / `nproc 24`, and the record now agrees

The arm-14 defect is gone on a default launch. Three boots of the fixed tree on a
26-vCPU guest read the session payload at **`Cpus_allowed_list 0-23`, `nproc 24`**,
with `/run/cang/responsive.status` and `CARGO_BUILD_JOBS` saying the same 24 - the
"two extra cores" shape of
[note 14](../notes/14-nix-build-bound.md) §3 (`0-21`, `nproc 22`, four cores
reserved) does not appear in any boot. The supervisor keeps `0-25`, a `cang exec`
child and the `nix-daemon` and its builder are all `0-23`, and a 30-second
saturation window shows exactly the two reserved cores idle.

Identity of the booted bytes (the campaign overlapped other agents' commits, so
each boot is named):

| | |
|---|---|
| **boot `ws15e`** (the arm cited below) | `/nix/store/2pxjcgdhmg7dzsjpfa0bbc38k4xsxvh9-cang-static-x86_64-unknown-linux-musl-0.23.6/bin/cang-guest-init`, sha256 `26967224238458ba8f8aa6d04b7e2da6bc176015ef063d724ec262b8ce264c3d`; drv `/nix/store/f6dyqbxskfa7znz2c098sz5qak5qnspl-…drv`; src `/nix/store/5kd0gvq6a3hdwz5z5p64bw7k8w9xgfqr-cang-workspace-source` |
| boots `ws15b`, `ws15d` (replicates, one test-revision older) | `/nix/store/1nxgjf4bqb7dyh77i03nywvl4n11fgln-cang-static-x86_64-unknown-linux-musl-0.23.6`, sha256 `389e904ee5f34ececa7a90aee44eb1f1468cc8ebf80efd2d8c4a32bfa8569e8b`; src `/nix/store/8h01bwmlp68sfrp9ad0q4cvz1sw4dzh9-cang-workspace-source` |
| test revision of the two binaries | identical production code: `cpu_bound.rs` differs only inside `#[cfg(test)]` between the two srcs (§7). The `ws15e` binary's src is the worktree content that is now commit `2600892c` |
| src binding | store src == worktree == `git show 2600892c:` for `runtime/cpu_bound.rs` `91ea7ae7…`, `runtime/session.rs` `f0a3c626…`, `process.rs` `1c6b50be…`, `runtime/exec.rs` `07bdc365…`, `components/nix/root.rs` `a5eafe18…` |
| check phase, run for this note | `nix build .#cang-musl` from the worktree built drv `/nix/store/6mrnq27j95n3nmb3hq3hxsx7vqhk5xva-…drv` → `/nix/store/nj1gcw31w9qhvlc7fdjwknjv70i2wsvv-…`, whose `nix log` reads `running 441 tests` / `test result: ok. 441 passed; 0 failed` |
| **booted bytes == checked-build bytes** | `cmp /nix/store/2pxjcgdhmg7…/bin/cang-guest-init /nix/store/nj1gcw31w9q…/bin/cang-guest-init` is **identical** (both sha256 `26967224…`): the binary `ws15e` booted is the artifact a build with the check phase produces |
| image | `localhost/cang:nixone` ID `3f2be435d370`, digest `sha256:04b32a0fc16d3bd9664ead63f892d269e26641f658ac2beb8482e43a0eea96f8` - loaded, not rebuilt (the daemon and `nix` are in this image) |
| host CLI | installed `result/bin/cang` = `/nix/store/x9dflild0m1ppfwl4l4pdhzhzdsa0zk3-cang-0.23.6`, `cang 0.23.6` (no host rebuild was needed: this arm runs the **ON** default, so the missing `CANG_GUEST_RESPONSIVE` forwarding of note 14 §4 never matters) |
| guest kernel | `7.3.5-hardened1`, `/sys/devices/system/cpu/online=0-25`, `nproc --all 26`, `MemTotal 4027340 kB`; host 28 CPUs, 17.7 GiB, no disk swap |
| launch | `cang --image localhost/cang:nixone --guest-init <guest-init> --mem 4 -- sh /workspace/arm15.sh`, **no `--root`**: the default launch, payload dropped to `dev` |

## 1. The fix, as this tree spells it

Two changes in `runtime/cpu_bound.rs`, both the shapes note 14 §3.2 asked for:

- `guest_cpu_count()` (`:68`) reads `/sys/devices/system/cpu/online`
  (`ONLINE_CPUS_PATH`, `:52`) through `online_cpu_count_from`/`parse_cpu_list`
  (`:78`, `:84`) and falls back to `sysconf` only when the file is unreadable - so
  a process's own affinity no longer feeds back into the derivation.
- `apply_workload_range` (`:150`) is idempotent: `already_within` (`:108`) makes
  one `sched_getaffinity` and returns without a second `sched_setaffinity` when
  the mask is already exactly `start..end`. The layered seams
  (`runtime/session.rs::spawn_pty_child` -> fork child, then
  `process::WorkloadBounds::apply` inside the exec path) therefore reserve once.

The arithmetic that produced `0-21` in arm 14 is `26 -> (0,24)` then `24 -> (0,22)`
in the child. It is now `26 -> (0,24)` then a no-op, and the live readback is
`0-23` in every boot, at the first readback (t = 0.16-0.75 s in the three boots
below) and at the last.

## 2. The readbacks, one row per boot

`MASK` is `/proc/<pid>/status` `Cpus_allowed_list`, `adj` `/proc/<pid>/oom_score_adj`,
`n` the number of 10 Hz samples. All three boots: `--mem 4`, 26 vCPUs, gate clear,
`post cang ps` empty, no VM worker left.

| boot | payload | payload shell | supervisor | `nix-daemon` | builder the daemon forked | `cang exec` child | build |
|---|---|---|---|---|---|---|---|
| `ws15b` | `0-23`, `nproc 24` | `0-23` | `0-25` (n=277) | `0-23` adj `-1000` | `sh` 1061 `0-23` (n=841) | `0-23`, 24 | rc=0, 90.8 s |
| `ws15d` | `0-23`, `nproc 24` | `0-23` | `0-25` (**n=2332**) | `0-23` adj `-1000` | `sh` 1058 `0-23` (n=869) | `0-23`, 24 | rc=0, 93.9 s |
| **`ws15e`** | **`0-23`, `nproc 24`** | **`0-23`** | **`0-25` (n=2283)** | **`0-23` adj `-1000`** | **`sh` 1059 `0-23` (n=848)** | **`0-23`, 24** | **rc=0, 91.6 s** |

`ws15b`'s watch starts only in the saturation window (277 samples); `ws15d`/`ws15e`
sample from t≈0.3-1.0 s to the end of the boot, so their counts span the nix build
(≈94 s, >900 samples) **and** the saturation window.

## 3. Claim by claim, boot `ws15e` verbatim

**1. The payload is on the policy's mask, not two cores narrower.**

```text
RESULT workload self pid=745 comm=python3.13 mask=0-23 adj=0 py_sched_getaffinity=24
RESULT workload self t_early t=0.75
RESULT guest nproc_cli nproc=24 nproc_all=26
RESULT mask_watch label=payload samples=2283 masks=0-23:2283 first_t=1.02 last_t=234.28 distinct_masks=1
RESULT mask_watch label=payload_shell samples=2283 masks=0-23:2283 first_t=1.02 last_t=234.28 distinct_masks=1
RESULT workload_after pid=745 mask=0-23 py_sched_getaffinity=24
```

One mask value in 2283 samples of the python payload and 2283 of the `sh` its
supervisor exec'd: the seam that arm 14 read as `0-21` is `0-23` here, and
`nproc 24` / `nproc --all 26` say it is a mask over a 26-vCPU guest, not an
offline-CPU count.

**2. The record and the mask agree, which they did not in arm 14.**

```text
RESULT responsive_status state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24
RESULT responsive_status_after state=on cpus=26 reserved=2 mask=0-23 cargo_build_jobs=24
RESULT guest env NIX_REMOTE=unix:///run/cang/nix-daemon/socket CARGO_BUILD_JOBS=24 NIX_BUILD_CORES=<unset>
RESULT shell env SH_CARGO_BUILD_JOBS=24 SH_NPROC=24 rc=0
```

24 allowed CPUs = `26 - reserved 2` = `cargo_build_jobs 24`, so the job count no
longer oversubscribes the payload by two. A fresh `/bin/sh -lc` (not just the
inherited environment) reads the same pair.

**3. The session supervisor still owns every vCPU.**

```text
RESULT supervisor pid=710 comm=cang-guest-init mask=0-25 adj=-1000
RESULT mask_watch label=supervisor samples=2283 masks=0-25:2283 first_t=1.02 last_t=234.28 distinct_masks=1
RESULT supervisor_after pid=710 mask=0-25 adj=-1000
```

2283 samples covering the build and the saturation window, one value.

**4. The exec seam is unchanged (`0-23`), from the host (boot `ws15e`, `cang exec` rc=0):**

```text
EXECMASK=0-23 EXECNPROC=24 EXECNPROC_ALL=26 EXECADJ=0
```

**5. The daemon and a builder it forks are inside the mask, their OOM guard intact.**

```text
RESULT daemon pid=916 ppid=1 mask=0-23 adj=-1000 comm=nix-daemon Cpus_allowed=0ffffff
RESULT daemon_chain_child_first 916:nix-daemon:0-23:adj-1000 1:init.krun:0-25:adj0
RESULT build_started pid=1015 client_chain=1015:nix:0-23:adj0 745:python3.13:0-23:adj0 729:sh:0-23:adj0 710:cang-guest-init:0-25:adj-1000 1:init.krun:0-25:adj0 client_mask=0-23
RESULT build_samples pids=3 total_samples=1701
RESULT builder pid=1034 comm=nix-daemon masks=0-23 adj=-1000 samples=851 chain=1034:nix-daemon:0-23:adj-1000 916:nix-daemon:0-23:adj-1000 1:init.krun:0-25:adj0
RESULT builder pid=1059 comm=sh masks=0-23 adj=-1000 samples=848 chain=1059:sh:0-23:adj-1000 1034:nix-daemon:0-23:adj-1000 916:nix-daemon:0-23:adj-1000 1:init.krun:0-25:adj0
RESULT builder pid=1036 comm=nix masks=0-23 adj=-1000 samples=2 chain=1036:nix:0-23:adj-1000 1034:nix-daemon:0-23:adj-1000 916:nix-daemon:0-23:adj-1000 1:init.krun:0-25:adj0
RESULT build_output_check path=/nix/store/7jsj2d40hxgfknrmmd56ds5kn8wmj9rf-cang-probe is_file=True content=probe
```

Read child-first: the builder `sh` (0-23) is the child of a `nix-daemon` build
process (1034, 0-23), itself a child of the daemon (916, 0-23), which hangs off
`init.krun` (0-25). `Cpus_allowed=0ffffff` is bits 0-23. The derivation is
fetch-free (`builder = "/bin/sh"`, no inputs, shell-only spin - arm 14 §5.1's
instrument note still applies: `sleep` is not reachable from this builder), and
it built and produced its output with `rc=0`.

**6. Exactly the two reserved cores are idle.**

30 free-running children of the payload, all inside `0-23` (1188 spinner mask
samples, one value), sampled over 30 s of `/proc/stat` per-CPU jiffies. Four rows
are elided with `…` where they list 24 CPUs - the full lists are in
`ws15e/out/results.txt`; every other quoted row is byte-for-byte as the guest
wrote it:

```text
RESULT spinners started=30 alive=30 window_s=30.00 window_requested_s=30.0 mask_samples=0-23:1188
RESULT cpu_row spinners cpu=0 dtotal=2806 dbusy=2796 frac_busy=0.996
RESULT cpu_row spinners cpu=20 dtotal=2962 dbusy=2940 frac_busy=0.993
RESULT cpu_row spinners cpu=21 dtotal=2891 dbusy=2873 frac_busy=0.994
RESULT cpu_row spinners cpu=22 dtotal=2955 dbusy=2918 frac_busy=0.987
RESULT cpu_row spinners cpu=23 dtotal=2963 dbusy=2935 frac_busy=0.991
RESULT cpu_row spinners cpu=24 dtotal=3069 dbusy=16 frac_busy=0.005
RESULT cpu_row spinners cpu=25 dtotal=3004 dbusy=13 frac_busy=0.004
RESULT cpu_summary spinners window_s_per_cpu=28.99 ncpus=26 hot=0,…,23 near_idle=24,25 near_idle_count=2 hot_count=24
RESULT cpu_verdict spinners shape=saturated near_idle=24,25 hot=0,1,2,…,23 verdict=PASS
```

Busy fraction = `(total - idle - iowait) / total` per CPU, from two `/proc/stat`
snapshots; `hot` is `>= 0.90`, `near_idle` is `<= 0.10`. All 24 allowed cores are
`0.987`-`1.000` busy, cpu24 and cpu25 are `0.005` / `0.004` - two reserved, not
four. The same window in `ws15d` read cpu24 `0.004`, cpu25 `0.011`, and in
`ws15b` `0.020` / `0.043`; the "four reserved" shape of arm 14 is not present in
any of the three.

The nix build's own window (91.6 s) is reported raw, not judged: one shell
builder fills **one** core, and it lands on a different one each boot
(`hot=cpu1` here, `cpu11` in `ws15d`, `cpu4` in `ws15b`) - always inside `0-23`,
never 24/25, and cpu24/25 stay near-idle across it too.

```text
RESULT cpu_summary nixbuild window_s_per_cpu=91.78 ncpus=26 hot=1 near_idle=0,2,…,25 near_idle_count=25 hot_count=1
RESULT cpu_verdict nixbuild shape=single-core verdict=not-applicable note=one builder fills one core; the reserve is judged in the saturated window
```

## 4. The two failed boots, and the three instrument traps they mark

1. **`ws15`**: the payload file was not in the launch cwd. `cd <arm dir>` is what
   the guest sees as `/workspace`, and the driver created only `host/` and `out/`
   there, so the session died `sh: /workspace/arm15.sh: No such file or directory`
   in 3 s. Fixed by copying the payload and probe into `$OUT_ROOT/$ARM` (arm 14's
   `ws14/` had them because they had been copied by hand).
2. **`ws15c`**: the probe's first revision started its sampler thread above the
   `def sampler`, so the payload died `NameError: name 'sampler' is not defined`
   after 19 RESULT lines - and those 19 lines are still honest readbacks
   (`0-23`, `24`). The thread start was moved below the definition.
3. **The "one guest at a time" gate arm 14 used is vacuous.** `pgrep -x
   libkrun-vm-work` matched **0** processes while a guest was live in `ws15b`
   (`ws15b-423910`, `cang ps` running): the VM worker is
   `cang internal libkrun-vm-worker-enter`, i.e. its process name is `cang`, and
   no process is named `libkrun-vm-work` at all (`grep -rn libkrun-vm-work
   crates/` finds only the argument string). The gate this arm used, and which
   reads 3 with a live guest and 0 after teardown, is
   `ps -eo comm,args | awk '$1=="cang" && /internal libkrun-vm-worker-enter/'`
   (comm-qualified so the shell running it cannot match itself), together with
   `cang ps`. Arm 14's `foreign_workers=0` lines were therefore not evidence of
   an idle host; this arm's gate lines are (`gate …=0 vm_workers=0` before, and
   `foreign_workers_after … vm_workers=0` after).

## 5. What this arm does not establish

1. **No `--root`, `as-dev` or `--workspace-disk` arm.** The default launch is the
   shape measured. Note 14 §3.2 expected `--root` to read `0-21` too; with the
   fix in place both of its halves are idempotent, but that is source reasoning,
   not a boot.
2. **Three boots of the same shape, not an A/B.** There is no OFF control here
   and none is needed for this ticket: the question is what one default launch
   reads. Note 14's OFF arm remains the control for the confinement itself.
3. **The saturation workload is 30 python spinners, not rustc.** Claim 6 is about
   which cores the *mask* leaves idle under a fully-loaded workload, not about a
   compiler's throughput; no timing comparison is made anywhere in this note.
4. **The host was not idle** (`load 0.19/0.40` at boot, 0.21-1.62 during), and
   the guest's own jiffies are the only CPU evidence used - per-CPU busy
   *fractions*, not wall times.
5. **`max-jobs = 1` in this image** (note 14 §5.3), so "many concurrent builders"
   is still unexercised; what is proven is the one builder and everything under
   the daemon.
6. **The check phase is a build claim, not a live one.** The fresh build run for
   this note says the tree's 441 tests pass in the nix sandbox; it says nothing
   about the guest, and §7 records the two tests that did not pass there until
   another agent rewrote them.
7. **Guest kernel console is 0 bytes** in every boot and `helper.stderr.log`
   holds only libkrun's recurring `vsock::muxer unexpected dgram pkt: 3` line -
   the same harmless noise notes 09/13/14 record.

## 7. The check phase this arm watched break, and its repair

While building the guest-init for the first boot, `nix build .#cang-musl` **failed
its check phase** (exit 101, `439 passed; 2 failed`) on the two tests that
`cpu_bound.rs` carried at that revision - the `nix build` sandbox has **no
`/sys`**, and both tests assumed it did:

```text
---- guest_init::runtime::cpu_bound::tests::the_online_cpu_list_is_counted_not_guessed stdout ----
panicked at crates/cang-guest-init/src/guest_init/runtime/cpu_bound.rs:291:64:
this host has sysfs: Os { code: 2, kind: NotFound, message: "No such file or directory" }

---- guest_init::runtime::cpu_bound::tests::a_confined_process_still_reports_the_guest_s_cpus stdout ----
panicked at crates/cang-guest-init/src/guest_init/runtime/cpu_bound.rs:312:9:
assertion `left == right` failed: the guest's CPU count does not follow this process's affinity
  left: 1
 right: 28
```

The second failure is the interesting one: the test defended the right property
(a confined process must not report its affinity count) but *asked the wrong
function* for it. With `/sys` unreadable, `guest_cpu_count()` falls back to
`sysconf`, and glibc's `get_nprocs` falls back to `sched_getaffinity` for the
same reason, so the process the test had just confined to one CPU answered `1`
- i.e. the test reproduced the musl behaviour it was written to exclude, on a
host where musl's path was not taken. Neither failure is a defect of the shipped
code; both are tests that cannot pass in the sandbox CI builds in, which is still
a landing blocker (this arm reported it rather than fixing it - `crates/` is out
of scope here).

That revision was booted with `doCheck = false` (boots `ws15b`/`ws15d`, binary
`1nxgjf4bq…`) so the live question could still be answered. **Its production code
is the production code that shipped**: `diff` of that revision against the
current one is confined to the `mod tests` block of `cpu_bound.rs` (the two tests
above were replaced by a tempfile fixture, `the_cpu_count_comes_from_the_kernel_list_not_this_process_affinity`),
and `runtime/session.rs`, `process.rs`, `runtime/exec.rs`,
`components/nix/root.rs` are byte-identical. Boot `ws15e` was then run with the
binary from a build **whose check phase passes** (441/0) and which is
byte-identical to that one, so no claim here rests on a test-revision difference.

## 6. Raw artifacts

`.scratch/arm15/` (scratch, not repo content; one directory per boot):

| path | holds |
|---|---|
| `ws15b/`, `ws15d/`, `ws15e/out/results.txt` | every `RESULT` line of that boot (98 lines in `ws15e`: 26 per-CPU rows per window) |
| `ws15/OUT` (`ws15` aborted in 3 s) | the failed boot: driver log + session log with `sh: /workspace/arm15.sh: No such file…` |
| `ws15c/` | the `NameError` boot: 19 RESULT lines |
| `.../host/driver.log` | gate (`pgrep_x_libkrun_vm_work=`, `vm_workers=`, host load), launch line, task id, `cang exec` rc + output, done sentinel, teardown, `foreign_workers_after` |
| `.../host/cang-session.log` | the guest console for that arm (the payload's own `tee` of every RESULT line) |
| `.../host/cang-exec-1.log` | `EXECMASK=0-23 EXECNPROC=24 EXECNPROC_ALL=26 EXECADJ=0` |
| `.../host/vmm/*.helper.stderr.log`, `*.guest-kernel-console.log` | the live VMM console copies |
| `check-phase.log`, `check-phase-failed.log` | `nix log` of the fresh checked build (`running 441 tests` / `441 passed; 0 failed`) and of the build that failed (`439 passed; 2 failed`, both panic messages) |
| `probe15.py`, `arm15.sh`, `run.sh` | the guest probe (per-CPU windows, daemon/builder sampler, whole-boot mask sampler), the payload, and the host driver |
