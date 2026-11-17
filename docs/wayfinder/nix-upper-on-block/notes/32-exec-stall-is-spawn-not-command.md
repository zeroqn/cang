---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The exec stall is in the spawn/handshake, not in the command - and only memory-allocating
# loads produce it

Split the exec round trip into its parts by having the guest command print its own clock:

```
host t0 --[send]--> guest S --[run]--> guest E --[reply]--> host t1
total = t1 - t0          run = E - S          other = total - run
```

`other` is clock-offset-free (the guest's clock runs ~350 ms ahead of the host's, measured, so
absolute S-t0 is useless but the difference is not).

| window | n | total p50 | total p95 | total max | command's own run p50 |
|---|---|---|---|---|---|
| idle | 52 | **58 ms** | 67 ms | 74 ms | 8 ms |
| 26 CPU burners (spin) | 183 | 62 ms | 100 ms | 147 ms | - |
| 26 fork+exec loops of `/bin/true` | 907 | 82 ms | 109 ms | 156 ms | - |
| 13 `dd oflag=direct` + 13 tmpfs churners | 9 | **1 979 ms** | 14.2 s | 14.2 s | 278 ms |
| **26 concurrent `rustc -O`** | 32 | **4 015 ms** | **54.3 s** | **97.7 s** | 88 ms |

Three readings:

1. **CPU saturation and a fork/exec storm are cheap**: 62 and 82 ms p50, both under 160 ms max.
   So "vsock goes to seconds because the CPUs are busy" is wrong, and so is "because of process
   churn".
2. **Loads that allocate memory and dirty page cache are what break it**: 2-4 s p50, tens of
   seconds at p95. The tmpfs churner arm (`dd` 200 MB into /dev/shm repeatedly) and 26 rustc
   processes share exactly that property and nothing else with the compile.
3. **The executed command is not slow**: it runs in 8-278 ms inside the guest while the round trip
   takes seconds, so the time is before the command starts or after it ends - process creation,
   the protocol handshake, or the transport copy.

## What this does and does not say about PTY and waypipe

They ride the same virtio-vsock, so a transport-level stall would hit them too - but they do **not
fork a process per message**, which is the half of `other` this experiment cannot separate out.
The measurement therefore does not establish that attach PTY or waypipe degrade like `cang exec`;
it establishes that `cang exec` does, under memory-allocating load.

Attempting to measure the PTY directly failed: driving `cang attach` from a scripted pty produced
2 091 samples of 100-160 us marked `timeout` - the client exits immediately (the pty read raises
EIO), so the attach path refuses a non-interactive client and needs a real terminal. That is a
harness limitation, not a result, and it is why this note reports no PTY number.

## Correction (same day, clean rerun): the p50s above were inflated ~10x by a parallel VM

The `run-vsock7` numbers in the table were taken while **two other `run-vsock7` instances and a
`run-vsock6` were still alive** - 25 and 19 minutes old, each with its own 8 GiB guest and its own
`dd`/tmpfs load loops. `ps` shows it plainly; the host was at 13 GB available instead of 14.7, and
the same boot type re-measured with a gate that refuses to start while any other
`libkrun-vm-worker-enter` exists gives:

| window (one VM, nothing else alive) | n | total p50 | p95 | max | command's own run p50 |
|---|---|---|---|---|---|
| idle | 50 | **62 ms** | 83 ms | 92 ms | 8 ms |
| 26 concurrent `rustc -O` | 43 | **397 ms** | 13.2 s | 66.2 s | 35 ms |
| 13 `dd oflag=direct` + 13 tmpfs churners | 83 | **602 ms** | 22.8 s | 65.0 s | 53 ms |

So the shape of the conclusion survives - memory-allocating loads degrade the exec round trip by
6-10x at the median and by two orders of magnitude at p95, while the executed command still runs
in 8-53 ms - but the medians I first reported (2.0-4.0 s) were host-pressure artefacts, the exact
confound note 13 documents. The load taxonomy (CPU 62 ms, fork/exec churn 82 ms, both under
160 ms max) is from the `run-vsock4`/`5` boots, which overlapped each other too; those two
windows are cheap enough that the ordering is unlikely to flip, but they have not been re-run
clean and should be treated as indicative.

Also fixed in this rerun: each run now gets its own workspace and evidence directory. The shared
`vsockws/evidence/phase` file meant a leftover host sampler could label its samples with a *later*
run's phase - which is why `run-vsock5`'s file contained "rustc" samples from a probe that has no
rustc window.

Retracted: the p50 column of the first table in this note.

## Severity under pure memory churn: 68 ms idle -> 11 s p50, with calls dying at the timeout

A dedicated window (13 tmpfs churners writing/freeing 200 MB + 13 `dd oflag=direct` writers on
the build disk, no rustc) makes the exec round trip far worse than the rustc storm, and produces
outright timeouts:

| window | n | outcome | total p50 | max |
|---|---|---|---|---|
| idle (10 s) | 29 | all rc=0 | **68 ms** | 171 ms |
| memory churn (60 s) | 9 | rc=0 | **11 292 ms** | 67 463 ms |
| memory churn | 2 | **rc=124** (killed by the 120 s client timeout) | 123.7 s / 168.4 s | - |

So under memory pressure `cang exec` does not merely slow down: two of eleven attempts ran past
two minutes and were killed. The stderr of those calls is empty, so this note cannot name the
failing stage beyond "before the command's output, after the request left the host" - the guest
command never printed anything in those attempts.

**And an unexplained cluster in the earlier clean boot**: of the 3 102 `mix` samples in
`run-vsock8`, 3 014 returned in 38-74 ms with *no guest output at all* while 88 succeeded with a
602 ms p50. Fast returns with no output are a different failure mode from the timeouts above, and
because that runner sent stderr to /dev/null the cause is unknown. It is the one hole in this
note; the fix is the same runner with stderr captured (as in `run-vsock9`), which now exists.

## Second correction: the memory-churn arms measured a dying guest, not a slow transport

`run-vsock9` (the stderr-capturing rerun) settled what the churn window actually was. Of its 1 826
exec attempts, **1 786 returned rc=1 in 45 ms p50** and every one of those carries the same
message on stderr:

```
cang: no active cang task with id, handle, or handle prefix, and no matching handle selector
```

The task no longer existed. The guest-init's own log in the same run shows why:

```
cang-guest-init: exec request failed: failed to write cang exec protocol frame tag: Broken pipe (os error 32)
[ERROR init_or_kernel] rcu: INFO: rcu_preempt detected stalls on CPUs/tasks:
[ERROR init_or_kernel] rcu:  18-...!: (1 GPs behind) ... (detected by 18, t=40290 jiffies, ncpus=26)
[ERROR init_or_kernel] rcu: INFO: Stall ended during stack backtracing.
[ERROR krun_devices::virtio::vsock::muxer] unexpected dgram pkt: 3
```

So under 13 tmpfs churners (200 MB each, repeatedly) plus 13 `dd oflag=direct` writers in an 8 GiB
guest, the **guest kernel wedged with RCU stalls of ~40 000 jiffies**, the guest agent could no
longer write its reply frames (EPIPE), and the VM died. The "11.3 s p50 / two calls past 120 s"
reported above are therefore samples of a *dying* guest, not of a slow transport - and the 3 014
fast "no guest output" samples in `run-vsock8` are the same phenomenon (the task was already gone).

Retracted: the churn row of both tables in this note, as a measurement of exec or transport
latency. What survives:

- **idle 62-68 ms** (many samples, two boots, both clean);
- **rustc storm 397 ms p50 / 13.2 s p95 / 66 s max** with the guest healthy throughout (every
  sample carried guest timestamps, so the guest was alive and answering);
- the observation that CPU saturation (62 ms) and fork/exec churn (82 ms) are cheap;
- a new, separate finding: **guest memory exhaustion wedges the guest kernel (RCU stalls) and
  breaks the exec channel** (EPIPE from the guest agent, then "no active task"), which is a
  robustness property of an over-committed guest rather than a latency property of vsock. An 8 GiB
  guest with 26 concurrent rustc processes plus tmpfs churn is beyond what that guest can hold;
  the earlier `-p sandsea-collector` build survived on 8 GiB, so the threshold sits between those
  two workloads.

## Separating the remaining two halves

`other` = (spawn: send + fork/exec + handshake) + (return: reply + host wakeup). To split it, the
guest agent should log its own accept/parse/fork/exec timestamps (it is `cang-guest-init`, pids
visible in `setup.txt`), or the host should timestamp a one-way ping that needs no process at all
(the attach protocol's frame, not exec). Both are next steps; neither is done.

## Also: is the IO pool generally slower than serial?

Two measurements answer it differently, by workload shape:

- **concurrent reads** (note 29, `/nix` through the composed overlay, 128 MiB slices): serial beat
  the pool at every depth - 2 087 vs 1 824 MB/s at 1 reader, 2 681 vs 2 261 at 2, 2 541 vs 2 005
  at 4, 1 975 vs 1 882 at 7 (a wash inside a 1 128-2 354 spread).
- **a real compile** (note 31, `-p sandsea-collector --jobs 26`): indistinguishable - the three
  warm arms were 246 / 236 / 266 s across serial, pool+direct and serial+direct, with the
  first-of-round artefact (553 / 681 s) dominating everything else.

So: serial is faster for read-heavy work, not measurably faster for a compile. The pool's cost is
a bounce-buffer copy per request, which is a proportion of a read phase that a compile barely has.

## Evidence

`run-vsock7-20261117T012455Z` (`logs/exec-split.txt`: per-sample host t0/t1 and guest S/E,
`workspace/evidence/setup.txt` with the rustc version and source size). Earlier windows:
`run-vsock4-*` (spin/spawn), `run-vsock5-*` (spawn/io, and the failed PTY attempt),
`run-vsock6-*` (block+memory mix). Probes `probe-vsock{4,5,6,7}.sh`, runners `run-vsock{4,5,6,7}.sh`,
`pty_rtt.py`.
