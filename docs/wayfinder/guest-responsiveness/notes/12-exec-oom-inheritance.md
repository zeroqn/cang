---
label: wayfinder:note
ticket: 12-exec-inherits-oom-protection
---

# An exec'd command is now the guest's OOM victim: ticket 01's arm 2, re-run on the score reset, kills the presser and the session answers an exec 10 s later

`cang exec`'s children used to inherit the session supervisor's
`oom_score_adj = -1000`, and [note 01](01-memory-failure-ladder.md) §6 measured
what that costs: on a 4 GiB guest the kernel skipped the 3.6 GB presser hanging
off `cang exec`, killed the session's idle payload, and 25 ms later panicked with
"System is deadlocked on memory". This note is the same arm, one boot, against
the guest-init that resets the score on the way in
(`runtime/exec.rs`'s `prepare_exec_child`, `process::drop_to_identity_and_exec`).
The result is a defect closed rather than a hypothesis settled: the victim is
now the presser, the console has no panic, and the session answers an `cang exec`
10 s after the kill, and `cang ps` still lists it 114 s after the kill.

## 0. What was run, and with what

| | |
|---|---|
| host CLI | `/nix/store/x9dflild0m1ppfwl4l4pdhzhzdsa0zk3-cang-0.23.6/bin/cang` (`result/bin/cang`) - **the same binary ticket 01's arm 2 used**, so it is not a variable |
| image | `localhost/cang:nixone` (the same tag) |
| guest init | `/nix/store/xbw6v2sf5a05bd6isghrkn9xp0iyzwyw-cang-static-x86_64-unknown-linux-musl-0.23.6/bin/cang-guest-init`, sha256 `ba49ce28a60538458fce7c709994dff8590f42eefe1c38e19c5b7dbdc1de82a2`, 2 829 672 bytes |
| guest init, pre-fix control | ticket 01 arm 2's `/nix/store/az5idng9kg2caiwxh0rmcd41y0awl5a6-…/bin/cang-guest-init`, sha256 `27958d5b27748c05f3028b961312509e732c092df2b6d5ab3e57be21c4127a01`, 2 828 808 bytes |
| guest kernel | `7.3.5-hardened1`, 26 vCPUs, workload uid 1000 |
| launch shape | managed session, payload `sleep 2400`, launched under `script -q -e` so stdin is a terminal (the shipped managed-session path) |
| memory pressure | `cang exec <task> -- sh /workspace/exstorm.sh`: presser v2 (distinct xorshift32 bytes per page, `CHUNK_MB=32 MAX_MB=8192 SLEEP_MS=50 HOLD_S=0`), 0.5 s in-guest sampler |
| `--mem` | 4 (`MemTotal` 4 027 340 kB, `SwapTotal` 4 027 336 kB, zram `disksize` 4 123 996 160 = 100 % of MemTotal, active compressor zstd) |
| gate | one boot, host window checked immediately before it: `gate foreign_workers=0 pgrep_x_cang=0` (another arm boots guests on this host; its VM was absent for this arm's whole life) |

**That the fix is in the bytes that booted** is a three-step chain, checked rather
than assumed:

1. the binary's derivation is
   `/nix/store/8qmsmxgdki3vnff84vd666fvya11m7b4-cang-static-x86_64-unknown-linux-musl-0.23.6.drv`;
2. that derivation's `src` binding is
   `/nix/store/3zr9ybhc6gbzdk8yqk12cx43g2a1fcp4-cang-workspace-source`, whose
   `crates/cang-guest-init/src/guest_init/runtime/exec.rs` and
   `…/process.rs` are **byte-identical** (`diff -q`) to this worktree's files
   (HEAD `4e39708f`, 10 modified files, the fix uncommitted as expected);
3. `grep -ac prepare_exec_child` finds the new function's name in that binary
   (1) and not in ticket 01's (0), and that binary is 864 bytes larger than ticket 01's
   (2 829 672 vs 2 828 808).

One deviation from the brief, disclosed: the plain `nix build .#cang-musl` failed
in its **check** phase, on `cang-guest-relay`'s
`server::tests::a_create_from_a_foreign_uid_lands_owned_by_the_tree_owner`
("the create of 'from-host.txt' must have succeeded: No such file or directory"),
which belongs to another agent's in-flight edit in that crate and has nothing to
do with guest-init. The binary quoted above is the same package built with
`doCheck = false` (`--impure` `--expr`, `.scratch/arm12/build3.log`); **no test
claim comes from that build**, and the live arm is this note's claim.

Capture: `.scratch/arm12/ws12/{exstorm.sh,sampler.sh,presser.rs,out/}`,
`.scratch/arm12/host/{driver.log,cang-exec-storm.log,cang-post-exec.log,vmm/}`.
The console is libkrun's passthrough in the task dir's `helper.stderr.log`
(copied every 1 s while the VM lived); `guest-kernel-console.log` was **0 bytes**
again, as in every arm of note 01.

## 1. The arm, and the one thing that changed

Ticket 01's arm 2 shape, unchanged: a managed session whose payload is an idle
`sleep`, and *all* the memory pressure arriving through `cang exec`. The guest's
own `dmesg` is closed to the workload (`kernel.dmesg_restrict = 1`;
`dmesg: read kernel buffer failed: Operation not permitted` at the end of this
arm), so the victim is read from the VMM console, as there.

Timeline (`t` = seconds after the first sample, t0 = 06:00:38.26Z):

| t (s) | event |
|---|---|
| +00 | (t0 = the first sample) the exec'd script is already up; its own readback `oom_score_adj=0`, `nice=0`, `pgrp=862` |
| +02 | presser starts (`pid=1008`, own readback `oom_score_adj=0`) |
| +25.4 | `MemAvailable` 433 MB, `AnonPages` 3 260 MB, swap still unused, `Cached` 35 MB |
| +29.7 | `MemAvailable` **0 MB**, `AnonPages` 3759 MB, first swap (`pswpout` 4 534), `zram mem_used_total` 218 MB |
| +31.7 | `zram mem_used_total` **1 031 000 064 = `mem_limit` exactly** (`mem_used_max` 1 031 032 832, 32 KiB over the cap), `Cached` 11 944 kB, `SwapCached` 1 967 892 kB, swaps 3 595 140 kB of 4 027 336 |
| +33.3 | **OOM kill** (kernel time 37.162511 s) |
| +34.1 | `oom_kill 1`, swap down to 29 860 kB, `MemAvailable` 3 731 MB, presser gone from the census |
| +43 | exec'd script finishes (its own `rc` after the presser's SIGKILL: `presser exited rc=137`), writes `done.txt` |
| +44 | `cang exec` returns **rc=0** |

The wall is the same wall note 01 §3-§4 described, reached by the same presser:
the memory budget pins at `mem_limit` while the device's *address space* still
advertises about 430 MB of free swap (3 595 140 kB of 4 027 336 in use), and the
device refuses stores from there on. From
`/sys/block/zram0`:

```
t+31.7  mm_stat 1259982848 1029805997 1031000064 1031000064 1031032832 53679 0 251021 251021
                  orig       compr      used       limit      used_max  same  comp  huge …
t+31.7  io_stat failed_reads failed_writes notify_free =  0    1112 0     21
t+34.1  io_stat failed_reads failed_writes notify_free =  0  158350 0 1004300
```

and `/proc/pressure/memory` at the same three points (`some` and `full` read
almost identical here because nearly every task was stalled, and their `total`
counters differ):

```
t+28.8  some avg10=0.00  avg60=0.00 avg300=0.00 total=36437      full avg10=0.00  … total=36437
t+29.7  some avg10=0.90  avg60=0.16 avg300=0.03 total=236287     full avg10=0.90  … total=236287
t+31.7  some avg10=10.16 avg60=1.85 avg300=0.38 total=1387493    full avg10=10.16 … total=1387483
t+34.1  some avg10=13.75 avg60=2.78 avg300=0.59 total=1776506    full avg10=13.75 … total=1775841
```

So this is not a null result: the arm reached the same ceiling (1 202 MB of pages
stored in 982 MB of compressed memory, `c` = 1.22, with `mem_used_total` pinned
at its 983 MiB cap, and 158 350 refused page stores) and then OOMed, exactly the
condition under which the pre-fix arm lost the session.

## 2. What the exec'd subtree's scores were, read three ways

Nothing here is inferred from the console; the score is measured from inside the
process, from the census, and from the sampler.

**From inside the exec'd work.** The exec'd script's first act:

```
== exstorm start 2026-11-25T06:00:37Z pid=862
== identity uid=1000 kernel=7.3.5-hardened1 cpus=26
== exec-child-own-oom_score_adj=0
== exec-child-nice=0 pgrp=862 session-pid=1
== sampler pid=909 own-oom_score_adj=0
```

and the presser's own progress file, 116 rows from `PRESS START` to the last
surviving readback, every one of them the same:

```
PRESS START pid=1008 oom_score_adj=0 chunk_mb=32 max_mb=8192 sleep_ms=50 pages=distinct
PRESS alloc_mb=3648 rss_kb=381956 vmswap_kb=3656884 oom_score_adj=0
PRESS alloc_mb=3680 rss_kb=75700 vmswap_kb=3996636 oom_score_adj=0   ← last row before the kill
```

**From the census at exec start** (`out/exstorm-status-at-start.txt`, every
process, `oom_score_adj` and `statm`):

```
proc 729 sleep          adj=0     …   # the session payload: score 0, as before
proc 862 sh             adj=0     …   # the exec'd script  ← was -1000 in arm 2
proc 898 sh             adj=0     …   # the sampler it started ← was -1000 in arm 2
proc 710 cang-guest-init adj=-1000 …  # the session supervisor and its workers
proc 724 cang-guest-init adj=-1000 …
proc 833 nix-daemon     adj=-1000 …
proc 834 timeout        adj=-1000 …
proc 835 nix            adj=-1000 …
proc 806 podman         adj=pause …
proc 837 cang-guest-init adj=-1000 …
```

**From the sampler, at the wall** (t+31.7, the last sample before the kill: `proc
<pid> <comm> <oom_score_adj> <statm size> <statm resident>`):

```
proc 1008 presser 0 2623275 103841     # ~406 MB resident, 10 GB of mapping
proc 729  sleep   0 264701  534
proc 862  sh      0 264094  636        # the exec'd script
proc 909  sh      0 264093  847        # the sampler
proc 710  cang-guest-init -1000 …      # nothing in the guest's protected set is a candidate
```

Every process the guest cannot afford to lose is still at -1000, so the guard's
purpose is intact; what changed is that the exec'd work joined the killable set
instead of joining the protected one.

## 3. The victim, and the panic that is not there

The VMM console, verbatim, from
`.scratch/arm12/host/vmm/ws12-126947-1795586432909081798.helper.stderr.log`
(copied while the VM lived):

```
[2026-11-25T06:01:09.948703Z ERROR init_or_kernel] [   35.557513] Write-error on swap-device (253:0:2244608)
… (ten such lines in the copy; the console line is rate-limited, pr_alert_ratelimited, page_io.c:538)
[2026-11-25T06:01:11.554070Z ERROR init_or_kernel] [   37.162511] Out of memory: Killed process 1008 (presser) total-vm:10493100kB, anon-rss:77244kB, file-rss:36kB, shmem-rss:0kB, UID:1000 pgtables:8072kB oom_score_adj:0
```

Nothing follows it: `grep -c 'deadlocked\|Kernel panic'` over the console copy is
**0**, where ticket 01's arm 2 has both lines 25 ms after its kill. The next
console entries are the vsock teardown at 06:01:22Z (the exec connection closing)
and 06:01:38Z — no kernel message at all.

The victim is also the *right* one by the kernel's own arithmetic, not just the
only one: `oom_badness` scores `rss + swapents + pgtables` (`oom_kill.c:228-229`),
so a presser with 77 MB resident and 3.9 GB of swapents outranks the session's
`sleep` (2 MB resident, and it was at 534 pages in that sample) that the pre-fix
arm died as. Both were killable this time; the kernel chose the presser, which is
exactly what "keep a victim available" was supposed to buy.

And the session is not merely un-killed but *working*: a second after the exec'd work
finished, the same session answered a second `cang exec`:

```
$ cang exec ws12-126947-… -- sh -lc 'echo alive-$(date -u +%FT%TZ) MemAvailable=$(awk …) own_oom_score_adj=$(cat /proc/self/oom_score_adj)'
alive-2026-11-25T06:01:21Z MemAvailable=3811824 own_oom_score_adj=0     rc=0
```

and `cang ps` reported the task `running` at that moment and again at 06:03:05Z
(114 s after the kill), until this session's own teardown ended it. `cang exec` also
reported **rc=0** rather than ticket 01's
`cang: cang exec connection closed before exit status` - it returned 0 because the
exec'd *command* was not killed, only the child it had started, and the script
went on to log and finish.

## 4. Before and after, on the same arm

| | ticket 01 arm 2 (pre-fix guest-init) | this arm (worktree guest-init) |
|---|---|---|
| exec'd script's score | `proc 745 sh -1000` | `proc 862 sh 0` (and its own readback `0`) |
| the 3.6 GB presser's score | `proc 980 presser -1000` | `proc 1008 presser 0` (116 own readbacks, all `0`) |
| victim | `Killed process 730 (sleep)` - the idle session payload | `Killed process 1008 (presser) oom_score_adj:0` |
| 25 ms after | `Kernel panic - not syncing: System is deadlocked on memory` | nothing |
| `cang exec` | rc=1, `cang exec connection closed before exit status` | rc=0, the script's own log returned |
| the session after | gone; a post-storm `cang exec` answered `no active cang task …` | `running`; post-storm `cang exec` rc=0 with a live in-guest reply |
| guest wall reached | yes (swap write errors at 15.3 s) | yes (`mem_used_total` = `mem_limit`, 158 350 refused stores) |

**One line.** The difference is the score the forked exec child starts at -
`prepare_exec_child`'s `process::allow_workload_kill()` as its first act, and the
same call in `drop_to_identity_and_exec` for the `as-dev` path; with it, the
kernel's candidate list is what §2 shows and the death it picks is the workload's.

## 5. What this arm does not claim

* **The console copy's window ends at 06:01:38Z** (t+60). It was a 1 s copier, and
  the second copy the brief asks for - from the `--preserve-debug` task dir -
  does not exist: by 06:03:05Z `/home/dev/disp/cang/ws12/tasks/` was empty, because
  this session's own teardown sent SIGTERM to the launcher's process group and
  cang cleaned up its task record while the detached VM workers stayed alive. The
  guest's liveness after the kill is therefore established by the exec reply at
  06:01:21Z and by `cang ps`, not by console lines after 06:01:38Z. **A panic
  between 06:01:38Z and 06:03:05Z is not excluded by any capture in this arm**;
  what argues against it is the state measured up to the sampler's last write at
  t+42.9 (`AnonPages` 19 MB, `MemAvailable` 3 721 MB, swap 28 MB) - no
  measurement exists after that point.
* **The teardown defect is mine, not cang's**: the driver SIGTERMed the launcher's
  process group, which it shared, and killed itself; the VM was then ended with
  `cang kill ws12-126947`. That is why there is no orderly `cang` exit line in the
  record, and why the session is *proven* to have outlived the kill by 60 s rather
  than by its own lifetime.
* **One boot, no repeat**: no within-arm spread for any counter here.
* **The census's third figure is not swap.** `/proc/<pid>/statm`'s third field is
  `shared`, and the guest scripts label it `swap`; only `size` and `resident` are
  quoted as such (§2, §3). Per-process swap was not measured; the presser's own
  `VmSwap` readback was (3 996 636 kB at 3 680 MB of allocation).
* **No build storm, no CPU claim**: this arm is the presser shape only, and
  another session's VM was booting guests on this host before and after it (the
  gate line at §0 is the whole window). Nothing here is a timing measurement.
* **The pre-fix half is ticket 01's measurement, not a re-run**: this session did
  not boot the pre-fix guest-init, so the "before" column is that arm's record
  (its own console copy, census and exec log, quoted from
  `.scratch/ladder01/arm2/`).

## 6. What other tickets take from this

* **[08](../tickets/08-guard-guest-init-processes.md)'s resolution is right and now proven.**
  Its corrected passage says an exec launch "keeps the workers protected and
  everything the listener starts killable"; §2 is that, read from the guest: the
  five protected processes are at -1000 and the listener's child is at 0.
* **[11](../tickets/11-earlyoom.md)'s case gets stronger, and is still not measured.** The
  warning window is visible here: `full avg10` 0.00 at t+28.8 with 169 MB
  available, 0.90 one second later at `MemAvailable` 0, and 10.16 at t+31.7 with 1 112 stores
  already refused - 1.6 s before the kill. An early responder
  that skips `oom_score_adj -1000` can now act on the exec'd subtree at all;
  before this fix it could not, because the only killable process was the
  session payload. Whether it acts *in time* is ticket 11's arm.
* **[06](../tickets/06-zram-and-swap-tunables.md)** gets a second arm at the same wall with
  the same numbers: `mem_used_max` 32 KiB over the cap, `c = 1.22` for the
  presser's pages, 158 350 refused stores, `SwapCached` 1 967 892 kB (the refused
  page's slot held in swap cache) while the device still advertised 432 MB free.
