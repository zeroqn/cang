---
label: wayfinder:research
title: Does the guest's FUSE read path scale with concurrency, or is the single virtio-fs `fs worker` serialising it?
status: open
blocked_by: []
claimed_by: pi research child (2026-10-26)
---


> **Floor correction (2026-10-26).** Any "no-FUSE floor" quoted in this note as
> 232 s was a **lever-off** measurement, not a no-FUSE one: `/` is itself the
> virtio-fs share (`/proc/mounts`: `/dev/root / virtiofs`), so the guest-local arm
> launched 2401 `rustc`/`ld` binaries off the same connection and paid
> OPEN+RELEASE for each (OPEN 87,871-89,374). With zero-message open the same
> tmpfs arm runs 111.0-164.9 s, and the true no-FUSE floor is ~131 s. See
> `MAP.md` ("The governor fact that reorders everything") and note 23's
> independent re-measurement for the numbers and the method.
# 20 - Concurrency-scaling probe: does the FUSE read path scale with guest threads?

Notes 15 and 19 left one question open: is the single virtio-fs server thread
(`fs worker`) a **serialisation** bottleneck, or is per-request latency paid in
the guest no matter how many server threads exist? A pool or io_uring only buys
something in the first case. This note answers it with one guest boot, two
filesystems inside that VM, five workload shapes and a 4 Hz sample of the
server thread and the vCPU threads.

Tool: `tools/rust-compile-bench/concurrency-scaling.sh` (new, committed with this
note; its README section describes the arms and the traps). Artifacts:
`/home/dev/cang-bench/runs/concurrency-20261026-103911` (run A),
`.../concurrency-20261026-105018` (run B, primary - it carries the guest CPU
counters), `.../concurrency-20261026-110040` (run C, the committed script's own
end-to-end check). Host 28 CPUs (17 GiB RAM), guest 26 vCPUs / 12.7 GiB, guest
kernel `7.2.10-hardened1`, cang 0.14.0, image `ghcr.io/zeroqn/cang:latest`,
4096 files x 4 KiB in one flat directory, 3 rounds x 2 s per cell, arms
alternating between rounds.

## Verdict

1. **Server-side parallelism is NOT justified for the open/read/stat mix.** At 64
   guest threads the worker's CPU is **26-27% of one core** (two independent
   counters agree), the guest's own CPU accounting reaches **1.9 of 26 vCPUs**,
   and throughput plateaus at 16 threads (**3.2-4.7x** over one thread). The
   worker's serialised service time is **96-162 us of CPU per op** against a
   **1.2-23 ms** per-op round trip, so even a perfectly parallel server could
   remove at most **~14% of the latency at one thread and ~0.4% at 64**.
2. **It IS justified for READDIRPLUS, the directory-scan class - and only for
   that class.** There the single worker is saturated already at **one** guest
   thread (**70% of a core**, 95% at 64), throughput is flat (**1.29-1.52x**),
   and p50 latency grows **48x** (21.8 ms -> 1048 ms per 4096-entry scan). A pool
   over the same request queue could plausibly take that class to ~4-8x, because
   the guest spending only 9-22% of a core is *not* the limit there.
3. **Neither rubric row of note 19 fired.** The worker was never idle-and-empty
   (row 1) and never CPU-saturated on the small-file ops (row 3); the guest never
   saturated. The plateau at 16 threads is a round trip whose time is on neither
   side's CPU counter - see section 5 for what that leaves open.

## 1. The probe

One guest boot; inside it the same 4096-file tree on two filesystems:

| arm | path | what it is |
|---|---|---|
| `virtiofs` | `/workspace/tree` | the single root share: 1 request queue, 1 `fs worker` thread |
| `shm` | `/dev/shm/tree` | the same tree on guest tmpfs - the no-FUSE control |

A ~150-line C program (`gcc -O2 -pthread`, compiled *in the guest*, executed from
`/tmp` because `/dev/shm` is `noexec`) runs one measured cell per invocation:
T threads (1/4/16/32/64) walk the file list with a stride of T, each doing the
mode's op until its own deadline, and report ops/s, p50/p99 (histogram, 1 us
resolution below 1 ms), the sum of the worker threads' CPU, and the guest's own
CPU/interrupt counters from `/proc/stat`.

Modes, and why all five are needed:

| mode | op | FUSE traffic |
|---|---|---|
| `open` | `openat(O_RDONLY)`+`close` | `FUSE_OPEN`+`FUSE_RELEASE` per op |
| `read` | open + `fadvise(DONTNEED)` + `read` + close | adds a real `FUSE_READ` |
| `stat` | `statx(AT_STATX_FORCE_SYNC)` | forces a `FUSE_GETATTR` |
| `statwarm` | `statx(flags=0)` | guest attribute cache only - **the control** |
| `readdirplus` | `opendir`/full `readdir`/`closedir` | `FUSE_READDIRPLUS` batches |

The cache question decides whether a scaling table means anything: the share is
created with `attr_timeout = entry_timeout = 5 s` (`device.rs:80-89`,
`LinuxComplete` semantics), so a repeated `statx`/read of 4096 small files is
guest-cache-served and never reaches the server. `statwarm` measures that arm on
purpose (millions of ops/s, worker <4%); `stat` forces `FUSE_GETATTR` and `read`
drops the page cache with `POSIX_FADV_DONTNEED`, so both do hit the server.
Without that distinction a "concurrency" table measures the guest VFS.

While the payload runs, the host samples the VMM's `fs worker` at 4 Hz: state,
`wchan`, `utime+stime`, `schedstat` runtime and runqueue wait, voluntary context
switches, and every `fc_vcpu N` thread's ticks (so the busiest single vCPU can be
identified). Guest timestamps are mapped onto the host clock with a sync ping;
the offset was 0.32-0.87 s and stable to <10 ms across each run's start/end pings.

## 2. The table (run B, primary)

ops/s per cell type, mean of 3 rounds; `worker%` is the `fs worker` thread's
share of one core (schedstat runtime; `utime+stime` agrees with it in every cell).

| arm | mode | 1 | 4 | 16 | 32 | 64 | 16/1 | 64/1 | worker% 1->64 |
|---|---|---|---|---|---|---|---|---|---|
| virtiofs | open | 1071 | 2712 | 4127 | 3734 | 4083 | 3.85x | 3.81x | 13.7 -> 26.1 |
| virtiofs | read | 841 | 1837 | 2656 | 2800 | 2773 | 3.16x | 3.30x | 13.6 -> 26.8 |
| virtiofs | stat | 2802 | 8142 | 12962 | 13198 | 13118 | 4.63x | 4.68x | 12.1 -> 22.9 |
| virtiofs | statwarm | 228009 | 946751 | 3378215 | 4803365 | 4863541 | 14.82x | 21.33x | 3.4 -> 2.7 |
| virtiofs | readdirplus | 67 | 89 | 87 | 91 | 103 | 1.29x | 1.52x | 70.1 -> 95.2 |
| shm | open | 228595 | 835832 | 1488230 | 1544160 | 1384008 | 6.51x | 6.05x | ~0 |
| shm | read | 159764 | 587022 | 1290701 | 1364133 | 1280380 | 8.08x | 8.01x | ~0 |
| shm | stat | 346467 | 1385566 | 4560800 | 6276828 | 6096028 | 13.16x | 17.59x | ~0 |
| shm | statwarm | 338923 | 1313104 | 4397387 | 6030718 | 5934976 | 12.97x | 17.51x | ~0 |
| shm | readdirplus | 1544 | 846 | 1384 | 1335 | 1285 | 0.90x | 0.83x | ~0 |

Two `shm` rows need a caveat rather than a conclusion: `readdirplus` *falls* with
threads (0.83-0.90x) because 64 guest threads scanning one tmpfs directory are
guest-CPU-bound on the same inode (the arm burns 25 cores at 64 threads), and
`open`/`read`/`stat` saturate at ~6-18x when the guest's own 26 vCPUs fill up.
They are still the right control: they show what the *same syscalls* cost with no
FUSE in the path (2-6 us per op instead of 350-1200 us).

Server-side evidence per mode and thread count (run B, virtiofs only):

| mode | thr | worker% | tick% | R/S/D % | of S: epoll | guest gcpu% | busy vcpu% |
|---|---|---|---|---|---|---|---|
| open | 1 | 13.7 | 13.7 | 30/70/0 | 60 | 44 | 89 |
| open | 16 | 26.2 | 26.5 | 57/43/0 | 33 | 168 | 90 |
| open | 64 | 26.1 | 26.2 | 52/48/0 | 29 | 202 | 86 |
| read | 1 | 13.6 | 13.0 | 14/86/0 | 67 | 43 | 88 |
| read | 16 | 25.1 | 25.5 | 36/64/0 | 36 | 162 | 86 |
| read | 64 | 26.8 | 27.3 | 67/33/0 | 19 | 191 | 87 |
| stat | 1 | 12.1 | 12.3 | 20/80/0 | 65 | 45 | 85 |
| stat | 64 | 22.9 | 23.0 | 48/52/0 | 29 | 191 | 90 |
| statwarm | 64 | 2.7 | 2.8 | 16/84/0 | 63 | 2363 | 81 |
| readdirplus | 1 | 70.1 | 70.0 | 71/29/0 | 0 | 15 | 16 |
| readdirplus | 16 | 89.9 | 90.0 | 91/ 9/0 | 9 | 19 | 13 |
| readdirplus | 64 | 95.2 | 95.2 | 96/ 4/0 | 4 | 15 | 13 |

`R/S/D` is the share of the 4 Hz state samples. For the small-file modes the
`R` share disagrees with both cumulative counters (read@64: R 67% but 27% of a
core) - a bursty server with a ~0.3 s cycle aliases the 4 Hz sampler, and `R`
includes runnable-not-running. The two counters agree with each other in every
cell, so **worker% is trustworthy and the state split is only a shape indicator**;
for `readdirplus` all three agree, which is what makes its saturation claim solid.
`D` was essentially zero everywhere (1 of 1105 samples in run B, on
`write_all_supers`; 2 in run A), so nothing in this path waits on host I/O.

Latency budget, run B, virtiofs, 1 thread (per op unless stated):

| mode | wall | guest CPU | worker CPU | vcsw/op | same op on shm |
|---|---|---|---|---|---|
| open | 934 us | 411 us | 128 us | 3.72 | 4.4 us |
| read | 1189 us | 511 us | 162 us | 4.63 | 6.3 us |
| stat | 357 us | 161 us | 43 us | 1.23 | 2.9 us |
| readdirplus | 14.9 ms/scan | 2.2 ms | 10.5 ms | 10.2 | 0.65 ms (shm, 1 thr) |

At 64 threads the same `read` cell is 23.1 ms/op (64 in flight / 2773 ops/s),
guest CPU 689 us/op, worker CPU 96 us/op, 2.17 vcsw/op.

### The same matrix, run A (a repeat 11 minutes earlier)

| mode | 1 | 4 | 16 | 32 | 64 | 16/1 | worker% 1->64 |
|---|---|---|---|---|---|---|---|
| open | 1998 | 5232 | 6649 | 6726 | 7362 | 3.33x | 12.6 -> 25.5 |
| read | 1528 | 3641 | 4775 | 5043 | 5188 | 3.12x | 12.9 -> 26.2 |
| stat | 6728 | 15274 | 19412 | 20574 | 21169 | 2.89x | 11.2 -> 21.7 |
| statwarm | 401030 | 1807739 | 5527833 | 7459836 | 7312170 | 13.78x | 2.4 -> 1.6 |
| readdirplus | 131 | 158 | 142 | 149 | 143 | 1.08x | 76.3 -> 97.6 |

Run A is ~1.8x faster in absolute terms on the virtiofs arm and ~1.35x on `shm`
(another session was running a `mon-threads.sh` monitor during run B; load
average 4.1). The *shapes* agree: the small-file modes land at 3.0-3.9x (run A)
vs 3.2-4.7x (run B), `readdirplus` at 1.08x vs 1.29x, and the worker's ceiling at
22-27% in both.

### Run C - the committed script end to end (1024 files, 1 round)

| mode | 1 | 16 | 64 | worker% 1->64 | gcpu% 1->64 |
|---|---|---|---|---|---|
| read | 445 | 1998 | 2602 | 10.1 -> 23.7 | 46 -> 198 |
| readdirplus | 122 | 188 | 191 | 62.9 -> 92.9 | 19 -> 22 |

Smaller tree, so `readdirplus` costs less per scan (8.2 ms) and the worker's
ceiling is 93% rather than 95%; the conclusion is unchanged.

## 3. What the numbers say

**(a) The small-file path is latency-bound, and the worker is idle for most of
it.** At one thread the worker is parked in `epoll_wait` in 60-67% of samples and
sleeps ~1.5 times per FUSE request (vcsw/op / 3 requests): the classic
empty-ring shape of note 19 row 1. More guest threads do put more requests in the
ring - the worker's on-CPU share climbs and its epoll share drops from 60-67% to
19-29% - yet throughput stops rising at 16 threads. That plateau cannot be
server CPU (26%) and cannot be guest CPU (1.9 of 26 vCPUs).

**(b) The worker's serialised service time is too small to explain the plateau.**
Its CPU per op is 128-162 us at one thread and 96 us at 64 (it gets *cheaper* per
op as the ring batches), against 1189 us and 23.1 ms of per-op latency. Bounding
the pool's best case by `worker CPU per op / latency per op`: **14% at one thread,
0.4% at 64**. Equivalently: at 64 threads the worker still has 73% of a core
unused, so the queue that produces the 23 ms is not waiting for its CPU - a pool
would give that thread more cores to not-be-the-bottleneck with.

**(c) The guest's own CPU per FUSE op is the largest single line item.**
`open`+`read`+`close` costs the guest **411-511 us of CPU** against **4-6 us** for
the identical syscalls on tmpfs - an 80-100x inflation, and it is charged mostly
to the calling thread (`benchcpu%` is 18-19% when only 841-1071 ops/s run, i.e.
210-220 us of it, the rest to guest kernel/interrupt work). That cost does not
fall with concurrency (511 us/op at 1 thread, 689 us/op at 64), which is the
opposite of what a server-side bottleneck would look like. Whether all of it is
genuine guest execution or partly 100 Hz tick attribution of wakeup/interrupt
time is not resolvable with the counters used here.

**(d) The one class where the single worker really is the limit: READDIRPLUS.**
One guest thread already puts the worker at 70% of a core; 64 threads take it to
95% and `R`/runtime/tick all agree. Throughput stays at 1.3-1.5x while p50 grows
from 21.8 ms to 1048 ms - a 48-deep queue in front of a serial server. Per scan
the worker burns 10.5 ms of CPU (4096 entries x ~2.5 us, which is the
`openat(O_PATH)+statx` per entry of `passthrough.rs`'s readdirplus closure, note
19 section 2.1) while the guest burns 2.2 ms and, crucially, stays at 9-22% of a
core at every thread count: the guest can feed more. This is the shape a pool
fixes, and 1.29-1.52x is what *not* having one costs today.

**(e) The `statwarm` control is what stops the whole experiment from being
meaningless.** It scales 14.8-21.3x on the share (guest-cache bound, 26 vCPUs of
work, worker at 2.7%) while `stat` on the same tree saturates at 4.6x. Virtio-fs
is not intrinsically a 4x wall; the 4x wall belongs to ops that reach the server.

## 4. What note 19's decision gate says now

| note 19 section 3.6 condition | this probe |
|---|---|
| curve plateaus >=2.5x below `/dev/shm` | true by 350-490x for the small-file modes (the shm arm is RAM, so this condition alone is not discriminating) |
| corrected `fs_worker` share >=25% | 22.9-26.8% for open/read/stat (borderline), **95.2% for readdirplus** |
| `D`/syscall-blocked `S` >=40%, or `R` >=80% with >=10 us CPU/op | `D` ~0 everywhere; `S`-blocked not measurable (`/proc/<tid>/syscall` is EPERM for the non-dumpable VMM); `R` >=80% only for readdirplus |
| penalty still >=10% of the build | unchanged (note 15: 67-79 s of 299-314 s) |

So the gate fires **only** for the READDIRPLUS class. For the open/read/stat mix
it does not, and the reason is stronger than "the worker is idle": the worker is
*busy-but-not-CPU-bound* (27% of a core while the queue is 64 deep), which none
of note 19's three rows describes.

## 5. What this probe does not settle

- **Where 23 ms of per-op latency goes at 64 threads.** Neither side's CPU
  explains it: worker 96 us, guest 689 us, both far from saturation, and the
  small-file modes' vCPU threads are only 2.4 cores total. The candidate
  mechanisms - the guest's per-request submission/wakeup path, KVM exit + eventfd
  + irqfd latency per kick, or a host-side queue lock shared by kicking vCPUs and
  the worker - need a different instrument: sample the `fc_vcpu N` threads'
  *states* (not just their ticks) under load, and count virtqueue-notify MMIO
  exits per request in the VMM. A pool is not the instrument: it cannot shorten a
  round trip.
- **How much of a rustc build is READDIRPLUS.** The probe prices the class
  (2.5 us of server CPU per directory entry, saturating one core at 67-103 scans/s
  of a 4096-entry directory) but does not measure its share of the 5-13M FUSE
  requests of a build. That share is the only thing that decides whether the one
  justified pool is worth building. Counting `FUSE_READDIRPLUS` requests in a
  `guest-virtiofs` build arm (`run.sh`) is the cheap next step, and it is a
  measurement, not a prototype.
- **The alternative lever for that class.** The 2.5 us/entry is a full
  `openat(O_PATH)+statx` per entry; a cheaper readdirplus path would compete with
  the pool on the same win, and the pool's ceiling (guest can feed ~4-8x) is a
  guess from the guest's 9-22% CPU, not a measurement.
- **Host configuration.** `kvm.halt_poll_ns` is **200000** on this host, so the
  vCPU threads' ticks (1.5-2.4 cores) exceed the guest's own `/proc/stat` busy
  time (0.4-1.9 cores) by up to 1.2 cores; when the guest halts waiting for a
  FUSE reply, the host spins for up to 200 us. Treat the vCPU-tick column as a
  host-cost figure, not a guest one; the guest's own counter is the guest number.
  It also means absolute latencies here may be kinder than on a host with
  `halt_poll_ns=0`, and harsher on host CPU.
- **One host, co-tenant load, one tree shape.** 4096 x 4 KiB in one directory is
  rustc's small-source shape, not its whole mix; the two full runs differ by 1.8x
  in absolute latency because another session was busy, while every ratio above
  reproduced.

## 6. Raw numbers

Round 2 of run B, virtiofs arm, verbatim (the other rounds and the `shm` arm are
in `results/cells.txt`; the sampler capture is `samples.txt` in the same
directory):

```
CELL round=2 fs=virtiofs mode=open threads=1 ops=1536 secs=2.002 ops_s=767 p50_us=2048.0 p99_us=4096.0 cpu_pct=19 entries=0 errs=0 gcpu_pct=48 intr=14047
CELL round=2 fs=virtiofs mode=open threads=4 ops=4964 secs=2.001 ops_s=2481 p50_us=2048.0 p99_us=4096.0 cpu_pct=42 entries=0 errs=0 gcpu_pct=124 intr=26652
CELL round=2 fs=virtiofs mode=open threads=16 ops=7161 secs=2.010 ops_s=3562 p50_us=8192.0 p99_us=16384.0 cpu_pct=63 entries=0 errs=0 gcpu_pct=176 intr=33390
CELL round=2 fs=virtiofs mode=open threads=32 ops=7080 secs=2.011 ops_s=3521 p50_us=16384.0 p99_us=16384.0 cpu_pct=72 entries=0 errs=0 gcpu_pct=197 intr=37737
CELL round=2 fs=virtiofs mode=open threads=64 ops=7679 secs=2.028 ops_s=3786 p50_us=16384.0 p99_us=32768.0 cpu_pct=70 entries=0 errs=0 gcpu_pct=213 intr=38942
CELL round=2 fs=virtiofs mode=read threads=1 ops=1238 secs=2.001 ops_s=619 p50_us=2048.0 p99_us=4096.0 cpu_pct=19 entries=0 errs=0 gcpu_pct=45 intr=13648
CELL round=2 fs=virtiofs mode=read threads=4 ops=3452 secs=2.002 ops_s=1724 p50_us=4096.0 p99_us=8192.0 cpu_pct=40 entries=0 errs=0 gcpu_pct=118 intr=25776
CELL round=2 fs=virtiofs mode=read threads=16 ops=5089 secs=2.008 ops_s=2535 p50_us=8192.0 p99_us=16384.0 cpu_pct=60 entries=0 errs=0 gcpu_pct=158 intr=33403
CELL round=2 fs=virtiofs mode=read threads=32 ops=5231 secs=2.012 ops_s=2600 p50_us=16384.0 p99_us=32768.0 cpu_pct=67 entries=0 errs=0 gcpu_pct=170 intr=35695
CELL round=2 fs=virtiofs mode=read threads=64 ops=5112 secs=2.026 ops_s=2523 p50_us=32768.0 p99_us=65536.0 cpu_pct=67 entries=0 errs=0 gcpu_pct=188 intr=38222
CELL round=2 fs=virtiofs mode=stat threads=1 ops=4330 secs=2.002 ops_s=2162 p50_us=406.0 p99_us=2048.0 cpu_pct=20 entries=0 errs=0 gcpu_pct=46 intr=13645
CELL round=2 fs=virtiofs mode=stat threads=4 ops=13631 secs=2.002 ops_s=6809 p50_us=431.0 p99_us=4096.0 cpu_pct=45 entries=0 errs=0 gcpu_pct=131 intr=24338
CELL round=2 fs=virtiofs mode=stat threads=16 ops=20372 secs=2.005 ops_s=10163 p50_us=2048.0 p99_us=8192.0 cpu_pct=70 entries=0 errs=0 gcpu_pct=178 intr=32776
CELL round=2 fs=virtiofs mode=stat threads=32 ops=22943 secs=2.016 ops_s=11383 p50_us=4096.0 p99_us=8192.0 cpu_pct=76 entries=0 errs=0 gcpu_pct=201 intr=39850
CELL round=2 fs=virtiofs mode=stat threads=64 ops=21421 secs=2.014 ops_s=10638 p50_us=8192.0 p99_us=16384.0 cpu_pct=77 entries=0 errs=0 gcpu_pct=191 intr=37541
CELL round=2 fs=virtiofs mode=statwarm threads=1 ops=407295 secs=2.001 ops_s=203572 p50_us=3.0 p99_us=6.0 cpu_pct=81 entries=0 errs=0 gcpu_pct=88 intr=3318
CELL round=2 fs=virtiofs mode=statwarm threads=4 ops=1680077 secs=2.001 ops_s=839604 p50_us=3.0 p99_us=5.0 cpu_pct=328 entries=0 errs=0 gcpu_pct=347 intr=6403
CELL round=2 fs=virtiofs mode=statwarm threads=16 ops=6850818 secs=2.004 ops_s=3419236 p50_us=3.0 p99_us=5.0 cpu_pct=1454 entries=0 errs=0 gcpu_pct=1494 intr=9896
CELL round=2 fs=virtiofs mode=statwarm threads=32 ops=8438754 secs=2.020 ops_s=4178584 p50_us=3.0 p99_us=5.0 cpu_pct=2065 entries=0 errs=0 gcpu_pct=2147 intr=18090
CELL round=2 fs=virtiofs mode=statwarm threads=64 ops=8936773 secs=2.027 ops_s=4409936 p50_us=3.0 p99_us=6.0 cpu_pct=2097 entries=0 errs=0 gcpu_pct=2346 intr=22615
CELL round=2 fs=virtiofs mode=readdirplus threads=1 ops=113 secs=2.016 ops_s=56 p50_us=32768.0 p99_us=32768.0 cpu_pct=9 entries=463187 errs=0 gcpu_pct=17 intr=3508
CELL round=2 fs=virtiofs mode=readdirplus threads=4 ops=163 secs=2.039 ops_s=80 p50_us=65536.0 p99_us=131072.0 cpu_pct=9 entries=668137 errs=0 gcpu_pct=9 intr=4376
CELL round=2 fs=virtiofs mode=readdirplus threads=16 ops=176 secs=2.187 ops_s=80 p50_us=262144.0 p99_us=262144.0 cpu_pct=9 entries=721424 errs=0 gcpu_pct=19 intr=5215
CELL round=2 fs=virtiofs mode=readdirplus threads=32 ops=192 secs=2.297 ops_s=84 p50_us=524288.0 p99_us=524288.0 cpu_pct=10 entries=787008 errs=0 gcpu_pct=19 intr=5550
CELL round=2 fs=virtiofs mode=readdirplus threads=64 ops=256 secs=2.539 ops_s=101 p50_us=1048576.0 p99_us=1048576.0 cpu_pct=9 entries=1049344 errs=0 gcpu_pct=10 intr=7475
```

A `readdirplus` cell stops only between scans, so its `secs` overruns 2 s as the
queue deepens (2.54 s at 64 threads) - that is why its measured ops/s is slightly
higher than a strict 2 s window would give, and it does not change the ratio.

Sampler lines (run B, one consecutive stretch covering the tail of a `read@64`
cell, the field order is on the script's last `echo`):

```
1792983108.434876594  R  0               698  6993069145   24924399    0        (vCPU tick CSV)
1792983108.758491880  S  0               705  7059946441   25126237    155668
1792983109.084357119  R  0               710  7114585685   25228216    156520
1792983109.415002739  S  do_epoll_wait   719  7195053730   25504360    158172
1792983109.738829896  R  0               727  7279691508   26735933    0
1792983110.064667524  R  0               736  7370587114   27204648    0
1792983110.384408298  S  do_epoll_wait   745  7461946976   27378625    0
1792983110.705791373  R  do_epoll_wait   754  7551103122   27614605    0
1792983111.024018838  S  do_epoll_wait   763  7638842471   27797476    0
1792983111.342777420  S  do_epoll_wait   767  7683770291   28130897    169405
```

(The `0` in the vcsw column of that run is the pre-fix read bug documented in
section 7; run C, made with the committed script, has 0 such samples. `runqueue
wait` stayed at 0.0% of the window in every cell, and the `D` state appeared in
2 samples of 1105.)

## 7. Instrumentation traps this probe hit (all silent, all fixed)

1. **`$12` is not field twelve.** In bash `$12` parses as `${1}2`, so
   `ut=$12` after `set -- $rest` produced the string `S2` and the arithmetic
   `vcpu + ut + st` read a shell variable named `S2` (`S2: unbound variable`
   under `set -u`). Use `${12}`/`${13}`. This is a *different* bug from note 19's
   comm-with-a-space one, with the same signature: nonsense CPU numbers.
2. **`/proc/<tid>/wchan` has no trailing newline.** `IFS= read -r w < file ||
   w=0` therefore discards every value (`read` returns 1 at EOF *after* setting
   the variable) and the sampler reported `wchan 0` for a sleeping thread 100% of
   the time while a direct `cat` printed `do_epoll_wait`.
3. **`/proc/<pid>/task/<tid>/syscall` is EPERM for the VMM** (it is
   non-dumpable), so the syscall the thread is blocked in cannot be recorded;
   `wchan` is the only wait-site available.
4. **`voluntary_ctxt_switches` read with a `while read` loop returned 0 for ~20%
   of samples** on a busy VM, and a cumulative counter that momentarily reads 0
   destroys the delta. Read it with one `awk` and keep the last good value.
5. **A running bash script is re-read from the file.** Writing a new version of
   `concurrency-scaling.sh` over the one a run was executing killed that run's
   analysis with a bogus `syntax error near unexpected token` at a line that
   `bash -n` accepts. Copy analysis out of a finished run instead of editing the
   script under it.
6. **State sampling and cumulative counters can disagree, and the counters
   win.** With ~7 samples per 2 s cell, a server whose busy bursts are ~0.3 s
   long aliases the 4 Hz sampler; `R` also counts runnable-not-running. `tick%`
   and `schedstat` runtime agreed cell-by-cell in all 150 cells; the `R` share
   did not, for the small-file modes.

## Sources

| what | where |
|---|---|
| the probe, committed with this note | `tools/rust-compile-bench/concurrency-scaling.sh` (+ README section) |
| run A / B / C artifacts | `/home/dev/cang-bench/runs/concurrency-20261026-{103911,105018,110040}/` - `results/cells.txt`, `samples.txt`, `cells.tsv`, `percell.tsv`, `table.txt`, `results/payload.log` |
| cache timeouts (`attr_timeout`/`entry_timeout` 5 s, `LinuxComplete`) | `deps/libkrun/src/devices/src/virtio/fs/device.rs:80-89`, `linux/passthrough.rs:368-369` |
| the per-entry lookup that makes READDIRPLUS expensive | `deps/libkrun/src/devices/src/virtio/fs/linux/passthrough.rs:1104-1127` (note 19 section 2.1) |
| the numbers this note re-uses | note 15 section 4 (guest-virtiofs 298.9/306.5/311.4 s, guest-local 232.0-235.1 s, `fs_worker` 12%) and note 19 sections 2.4 and 3.6 |
