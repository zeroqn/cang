---
label: wayfinder:research
title: One request queue in front of one worker - would io_uring (or a pool) help, and how do we tell whether the worker is "idle" or "blocked"?
status: open
blocked_by: []
claimed_by: pi research child (2026-10-26)
---

# 19 - One queue, one worker: idle or blocked, and would io_uring help?

Ticket 04 closed the worker-pool question on a single number - `fs_worker` at **12% of
one core** - and note 15 section 4 showed that at that occupancy queueing can recover at
most ~5 s of the 67-79 s virtiofs penalty. Note 15 also flagged, as correction 2, that a
thread at 12% *CPU* can still be saturated if its service time is spent **blocked in host
syscalls**, and named a probe that would settle it. This note settles the shape of that
probe, prices io_uring against a thread pool, answers "how much concurrency does the
transport already allow", and reports one thing that is wrong with the number the
decision was taken on.

Read-only: no build, no VM arm of my own; nothing edited in the tree except this note and
its one-line entry in the map's decisions list (the sampler below ran from a scratch file). Fork sources read at `deps/libkrun` `7160f48f`
(`v2.0.0-cang.10-2-g7160f48f`); the guest kernel is `v7.2` (pinned guest
`linux-7.2.10`), fetched from `git.kernel.org` and quoted by `grep -n` line numbers.
The one live measurement in section 3.4 is a **read-only `/proc` sample of a co-tenant
session's VM**, labelled as a method dry run, not as an arm.

## Verdict

1. **The transport is not the limit; one function call in one thread is.** The request
   virtqueue is 1024 descriptors deep with at least 4 descriptors per request, so up to
   ~256 requests can be in flight, and the worker drains the whole ring on every kick.
   Guest-side, foreground FUSE requests are not throttled at all (only the 12
   *background* slots are), so in-flight depth is bounded by how many guest tasks are
   blocked in a FUSE call, not by the ring. Against all that, `process_queue` calls
   `server.handle_message` **inline**: one request in service, in FIFO order.
2. **io_uring is the wrong first move even if the worker turns out to be blocked.** For
   exactly the opcodes that dominate this workload the ring has no asynchronous path:
   `statx` is `REQ_F_FORCE_ASYNC`, `openat` is inline only for a fully dcache-resolved
   lookup and punts to io-wq otherwise, and rename/unlink/mkdir/symlink/link are all
   FORCE_ASYNC. In those cases io_uring *is* a thread pool (io-wq) with a nicer API and
   batched submission. The parallelism it buys is the parallelism a pool over the
   existing ring buys - which upstream virtiofsd ships, and for which this fork already
   has the template (`block/worker.rs::ReadPool`).
3. **The 12% number cannot be used as measured.** `tools/rust-compile-bench/mon-threads.sh:24`
   computes `$14 + $15` from `/proc/<pid>/task/<tid>/stat`, and the server thread is named
   `fs worker` - a comm containing a space shifts every field by one, so the sampler sums
   `cmajflt + utime` and **drops `stime` entirely**. On a live `fs worker` the dropped
   share was 59% of that thread's CPU (utime 1227 vs stime 1728 ticks over the same
   line). For a thread whose work *is* host syscalls, that is most of the signal.
4. **So: no hardware-level change is justified today, but "the pool is invisible" is not
   justified either** - it rests on row 1 of the table in section 2.4, computed from a
   CPU share that omits stime. Order of work: fix the sampler (one line), re-run one
   thread capture (the old captures are destroyed by the bug - they store the broken
   sum, not the raw ticks), and only if the corrected share is >>12% run the ~10-minute
   probe of section 3 in one boot with alternating arms. io_uring should not be
   prototyped before a pool over the existing ring is measured: it is strictly more work
   for the same benefit on this op mix.

## 1. How much concurrency the virtqueue already allows

| # | fact | where |
|---|---|---|
| 1 | request-queue ring depth is **1024** | `deps/libkrun` `src/devices/src/virtio/fs/mod.rs:46` `QUEUE_SIZE = 1024`, installed as `QUEUE_CONFIG` at `:47` |
| 2 | the guest builds its vring at exactly that depth: it reads `VIRTIO_MMIO_QUEUE_NUM_MAX` and passes it to `vring_create_virtqueue`, then writes the size back | `linux v7.2` `drivers/virtio/virtio_mmio.c:374-391` |
| 3 | a request needs **at least 4 descriptors** ("requests need at least 4 elements": in-header, args, reply header, reply args; heap sgs above 6) | `fs/fuse/virtio_fs.c:1398-1408`, `virtqueue_add_sgs` at `:1461` |
| 4 | no `VIRTIO_RING_F_INDIRECT_DESC` is offered (only `VERSION_1 | EVENT_IDX`), so those 4 descriptors are not coalesced into one indirect table | `fs/device.rs:73` |
| 5 | => at most **~256 requests in flight**, and the ring never splits a request because of depth: `max_pages_limit = min(max_pages_limit, virtqueue_size - FUSE_HEADER_OVERHEAD)` = 1020 pages, above the server's 1 MiB write | `fs/fuse/virtio_fs.c:1730-1732`; `fs/server.rs:35` (`MAX_BUFFER_SIZE = 1 << 20`) |
| 6 | guest-side, **foreground requests are never throttled**: `fuse_block_alloc` blocks on `initialized`/`blocked`/io_uring only, and `for_background` is the flag that makes `blocked` matter; only background (writeback/readahead) waits on `max_background` | `fs/fuse/dev.c:86-90`, `:108-121`; `fs/fuse/dev.h:9` (`FUSE_DEFAULT_MAX_BACKGROUND 12`), `dev.c:347`, `:582-595`; `inode.c:1245-1258` |
| 7 | every CPU's request lands on the **same** queue with `num_request_queues = 1`: `queue_id = fs->mq_map[raw_smp_processor_id()]`, and with one request queue the fallback maps all CPUs to `VQ_REQUEST` | `fs/fuse/virtio_fs.c:1511`; `:902-916`; fork `fs/device.rs:78` |
| 8 | the worker **drains the ring**: `handle_event` loops disable_notification -> `process_queue` -> enable_notification until the guest stopped adding, `process_queue` pops until `pop` returns `None` | `fs/worker.rs:212-232`, `:234-236`; `virtio/queue.rs:491-496` (`pop` returns `None` on empty/zero-size/bus-master-off) |
| 9 | per request it parses the chain into `Reader`/`Writer`, calls **`server.handle_message` inline**, then `add_used` and maybe `signal_used_queue` | `fs/worker.rs:237-266`; `queue.rs:544-570` (`add_used`), `:713-747` (`needs_notification`, `EVENT_IDX` batching) |

Two of the question's assertions resolve as follows.

- *"Does the worker read one descriptor per notification or drain the available ring?"*
  It **drains** (`worker.rs:236`), and the outer loop (`:218-231`) re-runs the whole drain
  if the guest kicked while it was inside `process_queue`, so the device never leaves work
  parked in the ring. Depth is not lost to the endpoint; it is lost to the inline server
  call.
- *"How many requests can the guest have in flight?"* One per task blocked in a FUSE op
  (unbounded, up to memory), plus at most 12 async/background ones. At `CARGO_BUILD_JOBS=26`
  with rustc processes each holding a few open/read/stat in flight, the offered depth is
  routinely in the tens - i.e. 10-25x the queue's per-request serialisation.

**So the transport offers ~256-wide submission and the server accepts width 1.** What
follows is about whether using that width would pay, not whether it exists.

## 2. io_uring on the host side

### 2.1 What one FUSE request costs today, in host syscalls

| FUSE op | host work (passthrough backend) | where |
|---|---|---|
| LOOKUP | `openat(parent_fd, name, O_PATH|O_NOFOLLOW|O_CLOEXEC)` + `statx(fd, AT_EMPTY_PATH)` = **2 syscalls** + 2-3 `RwLock`/BTreeMap | `fs/linux/passthrough.rs:939-963` (openat `:950`, `statx` at `:963`), `:198-213` |
| GETATTR | one `statx(fd, AT_EMPTY_PATH|...)` | `:1290` + `:198-213` |
| OPEN | one `openat(proc_self_fd, "<fd number>")` - a `/proc/self/fd/N` magic-link open, not a resolved path | `:487-530`, openat at `:523` |
| READ | one `preadv` with iovecs pointing **into guest memory** | `:1226-1250`; `virtio/descriptor_utils.rs:259-268` (`write_vectored_at_volatile`) |
| WRITE | one `pwritev` from guest memory | `:1253-1287` |
| READDIRPLUS | `lseek64` + `getdents64` **+ one full LOOKUP per entry** = `2 + 2*D` syscalls | `:560-620`; `readdirplus` closure `:1104-1127` (`let entry = self.lookup(...)` at `:1124`); negotiated at `:899` (`DO_READDIRPLUS|READDIRPLUS_AUTO`) |
| CREATE/MKDIR/UNLINK/RENAME | one mutation syscall (+ a lookup for the reply on create) | `:1187`, `:1201` |
| RELEASE | `close` via `File` drop | `:722` |

Two consequences matter for the io_uring question:

1. the **data** path is already zero-copy into guest RAM (`preadv`/`pwritev` over the
   descriptor's volatile slices), so a ring cannot save copies that are not happening;
2. the cost is **metadata dominated** - `statx`, `openat`, `getdents64` and a `statx` per
   readdir entry - which is exactly the class of syscall the ring does *not* make
   asynchronous (2.2).

### 2.2 The opcode reality: for this op mix io_uring *is* a thread pool

| opcode | ring behaviour | citation (`linux v7.2`) |
|---|---|---|
| `IORING_OP_STATX` | prep sets `REQ_F_FORCE_ASYNC`; the issue function even asserts it never runs non-blocking: `WARN_ON_ONCE(issue_flags & IO_URING_F_NONBLOCK)` -> **always io-wq** | `io_uring/statx.c:46`, `:56` |
| `IORING_OP_OPENAT` / `OPENAT2` | inline only when the whole lookup resolves from dcache (`LOOKUP_CACHED` + `O_NONBLOCK` under `IO_URING_F_NONBLOCK`); on a miss returns `-EAGAIN` so the request is punted | `io_uring/openclose.c:138-142`, `:162-167` |
| `RENAMEAT`/`UNLINKAT`/`MKDIRAT`/`SYMLINKAT`/`LINKAT` | `REQ_F_FORCE_ASYNC` in every prep -> **always io-wq** | `io_uring/fs.c:78`, `:131`, `:180`, `:232`, `:280` |
| `IORING_OP_READ`/`WRITE` | tries the non-blocking issue (`IOCB_NOWAIT`), falls back to io-wq on `-EAGAIN`; `iopoll`/`async_size` are what make polling and true async possible | `io_uring/rw.c:941-949`, `:967`; `io_uring/opdef.c:242-266` |
| everything blocking | io-wq: bounded + unbounded worker sets with `max_workers` (unbounded defaults to `RLIMIT_NPROC`), tunable at run time | `io_uring/io-wq.c:1271-1273`, `:1477-1502` |

So the phrase in the map ("passthrough/io_uring are unreachable from a VMM server") is
wrong in the letter - a ring is just syscalls plus an fd the fork could add to the same
epoll set it already runs (`fs/worker.rs:160-176`) - and it is the wrong reason. The real
finding is: **the ring's advantage over a pool is batched submission and a shared worker
set, and for metadata ops it delegates to worker threads exactly as a pool would.**

### 2.3 What io_uring cannot do here

- **Build the reply.** The reply is written into the request's `Writer` (guest memory) and
  the chain must then go back with `add_used` on the same `Queue` and a possible
  `signal_used_queue` (`fs/worker.rs:244-266`). Completion handling therefore stays on
  the thread that owns the queues: the ring removes *waiting*, not *assembling*. The
  per-op CPU the worker burns - descriptor parse, inode/handle lookups under `RwLock`,
  attr encode, `add_used`, eventfd - is untouched, and that is precisely the quantity the
  12% was supposed to measure.
- **Keep the current borrow structure.** `handle_message(&self, Reader, Writer)` is
  synchronous and `process_queue` holds `&mut self`; the Reader/Writer borrow guest
  memory for the life of the call. Async completion requires owning the descriptor chain,
  the queue index and the reply length per in-flight request - a real refactor of
  `worker.rs:234-268`, of the same order as a pool's.
- **Change per-op semantics.** `FUSE_ATOMIC_O_TRUNC` is one host `openat(O_TRUNC)` either
  way; create/unlink/rename are serialised by the guest VFS's directory locks, not by the
  server's FIFO, so out-of-order dispatch is safe. What the serial server gives away when
  it goes parallel is **head-of-line blocking**: today one 1 MiB write or one big
  readdirplus delays every independent op behind it. That is a p99 effect, invisible in a
  mean CPU figure - a second reason the state sampler (3.2) has to run.
- **Handle short reads for free.** `read_exact_to` loops on preadv
  (`fs/filesystem.rs:149-183`); on a ring that becomes a link chain or a reissue, otherwise
  a short READ is reported as a short read (FUSE_READ must be exact-or-error).
- Error mapping is actually *easier*: `cqe.res` is already `-errno`.

### 2.4 What one submitting thread can reach, with the measured numbers

Model: an op costs `c` of CPU on the submitting thread (submit + complete + reply
assembly) and `d` of waiting. With `K` ops outstanding, one thread sustains
`R(K) = K / max(d, K*c)`, so `R -> 1/c`: **the ring's ceiling is set by `c`, not by the
blocking.**

Inputs (guest-virtiofs arm, synthetic 2401-crate tree, `CARGO_BUILD_JOBS=26`, note 15
section 4):

- `N ~ 5-13 M` FUSE requests per build;
- observed throughput `= 305 s / N ~ 23-61 k ops/s` (about 34 us/op mean at 9 M);
- server CPU as measured: `39 s` (12.3% of 315 s) -> `c_utime ~ 3-8 us/op` - **utime
  only**; if `stime:utime ~ 1.4` as observed on a live worker (3.5), `c ~ 7-19 us/op`
  and the arm's server CPU is ~94 s (31%);
- `1/c` ceiling with `c' = 6-10 us` (submit + CQE + the unchanged reply assembly):
  **~100-170 k ops/s, i.e. ~4-6x today's rate** - *if* the missing 20-50 us per op is
  blocked in host syscalls, and ~0x if it is the guest round trip with the queue often
  empty.

| reading | occupancy `rho` | recoverable queueing `rho/(1-rho) * S` | as % of the 67-79 s penalty |
|---|---|---|---|
| as measured (utime only, `S = 39 s`) | 0.12 | ~5 s | 6-8% |
| corrected CPU (`S ~ 94 s`, stime included) | 0.31 | ~42 s | 53-63% |
| blocked back-to-back (occupancy ~1, mostly sleeping) | -> 1 | the whole penalty | 100% |

The three rows differ by an order of magnitude and the decision so far used row 1. That
is the entire case for running the probe.

## 3. The decisive measurement

### 3.1 Guest-side concurrency scaling (one boot, alternating arms)

- Tree: 4096 files x 4 KiB in one directory on the share, and the same tree on `/dev/shm`
  inside the same VM. Pre-create, then read every file once to warm the host page cache.
- Op: `open(O_RDONLY) + read(4 KiB) + close` (the "small file" shape), and separately a
  `readdirplus` scan of the tree (the `2 + 2*D`-syscall shape of 2.1, which is what rustc
  does to `target/` and the registry).
- `T = 1/4/16/32` threads, 3-5 s per point, 2 rounds per `(filesystem, T)`, **alternating
  filesystem between rounds** so host-load drift hits both arms; log
  `RESULT fs=virtiofs threads=16 ops=... ops_s=... p50_us=... p99_us=...` lines to a guest
  file and copy them to `/workspace` (tiny - those writes are themselves FUSE traffic).
- What it proves: whether the server serialises (flat virtiofs curve while `/dev/shm`
  scales). What it cannot prove: whether the serial time is CPU or sleep - a flat curve
  looks identical in both worlds. That is 3.2, which must be sampled **in the same run**.

### 3.2 Host-side state sampler (4 Hz, per thread)

For the worker tid, per sample: `/proc/<pid>/task/<tid>/stat` (state, utime, stime),
`/proc/<pid>/task/<tid>/wchan`; at start and end also `/proc/<pid>/status`
(`voluntary_ctxt_switches`) and `/proc/<pid>/task/<tid>/schedstat` (run ns / runqueue-wait
ns). Validated script (it is what produced 3.4):

```sh
#!/bin/bash
# state/wchan/CPU sampler for one *thread* of the VMM. $1=pid $2=tid $3=samples $4=interval
pid=${1:?pid}; tid=${2:?tid}; n=${3:-120}; iv=${4:-0.25}
base=/proc/$pid/task/$tid
prev=""
while [ "$n" -gt 0 ]; do
    [ -r "$base/stat" ] || { echo gone; break; }
    line=$(cat "$base/stat")
    rest=${line#*)}            # strip "pid (comm)" - comm may contain SPACES
    rest=${rest# }
    state=$(echo "$rest" | awk '{print $1}')
    ut=$(echo "$rest" | awk '{print $12}')
    st=$(echo "$rest" | awk '{print $13}')
    wch=$(cat "$base/wchan" 2>/dev/null); [ -z "$wch" ] && wch=0
    vcs=$(awk '/voluntary_ctxt_switches/ {print $2}' "$base/status" 2>/dev/null)
    if [ -n "$prev" ]; then dticks=$((ut + st - prev)); else dticks=0; fi
    prev=$((ut + st))
    printf '%s|%s|%s|%s|%s|%s\n' "$(date +%s.%N)" "$state" "$wch" "$dticks" "$vcs" "$ut+$st"
    n=$((n - 1)); sleep "$iv"
done
```

Cross-check every CPU number against `ps -L -o tid=,time=,pcpu= -p <pid>` (traps in 3.5).

### 3.3 Interpretation table

| observation over the arm | means | action |
|---|---|---|
| >=70% of samples `S`, wchan `do_epoll_wait` | the ring was empty - the guest did not offer work | **stop**: neither a pool nor a ring changes anything |
| >=40% samples `D`, or `S` with wchan `io_schedule` / `wait_on_page_bit*` | service time is spent blocked in real host I/O | a **pool** is justified; `balance_dirty_pages` in the wchan instead means a dirty-ratio throttle, which no amount of parallelism fixes (that is a writeback/dirty-limit lever) |
| >=80% `R` and CPU% near 1 core | CPU-bound server | a pool buys cores (28 mostly idle guest vCPUs are available), **io_uring buys nothing** |
| `vcsw / N ~ 1` (N from the 3.1 probe) | one sleep per op = the epoll wake only | idle-waiting |
| `vcsw / N ~ 2` | an extra sleep per op = a blocking syscall | blocked |

The `vcsw/N` ratio is the cheapest corroborator: the guest probe gives `N` exactly, and
one sleep per op cannot hide a blocking syscall that also slept.

### 3.4 Method dry run on a live VM (opportunistic, not an arm)

While reading the fork, a co-tenant session's VM was up (`cang 0.14.0`, task
`guest-virtiofs-892830-1792980040582899626`, pid 892976, 45 threads), so the sampler got a
15 s / 60-sample run at 0.25 s on `fs worker` (tid 893425) as a dry run, 2026-10-26
~10:02:

- states: **`R` 23/60 (38%), `S` 37/60 (62%)**; of the `S` samples 22 had wchan
  `do_epoll_wait` and 15 had wchan 0 (the read raced with the wake - unclassified);
- **zero `D` samples**, no `wait_on_page_bit`, no `balance_dirty_pages`;
- ticks in the window: 600/1500 = **40% of a core**, so `R` essentially equals CPU for this
  thread; `ps -L` at the time: `fc_vcpu 0` 86%, `fs worker` 24-29% cumulative.

Reading: row 1 of 3.3 - an epoll-idle worker - for *that* workload. It is a different
arm, unattributed, and it ended before it could be re-sampled, so it validates the
instrument and the parse, not our conclusion. It is exactly why the probe must run inside
a controlled, alternating arm.

### 3.5 Two sampler traps - one of which already corrupted the map's decision number

**(A) A comm with a space shifts every field, and the harness does this today.**
`/proc/<pid>/task/<tid>/stat` puts comm in parentheses *without escaping spaces*, so
`1234 (fs worker) S ...` splits under awk as `$1=1234 $2=(fs $3=worker) $4=S`. Every
interesting field is therefore one higher than documented, `$3` is not the state, and
`$14+$15` is **`cmajflt + utime`, not `utime + stime`**. Live proof from that VM's own stat line:
`awk '{print $3}'` printed `worker)`, and `$14+$15` printed 153 where the same line's
`utime+stime` was 400 (`cmajflt` 1 + `utime` 152 versus `utime` 152 + `stime` 248).
`tools/rust-compile-bench/mon-threads.sh:24` does exactly `$14 + $15`, and *both*
space-bearing thread names in the map's tables are affected: `fs worker` **and**
`fc_vcpu N` (smaller there - `stime` was 24% of `fc_vcpu 0`'s ticks in the same read - but
the same bug). Fix: strip through the last `)`, then `$1`=state, `$12`=utime, `$13`=stime.
Impact: the arm's `fs_worker 12.3%` is utime-plus-a-fault-count; for a syscall-heavy
thread the omitted `stime` is the majority (59% in the live snapshot: 1227 utime vs 1728
stime ticks), which is why row 1 of 2.4 can move to row 2 - the difference between "the
pool is invisible" and "the pool could recover half the penalty". The old captures
(`logs/threads-*.txt`) store the broken sum, so they cannot be recomputed: one re-run is
needed.

**(B) `/proc/<tid>/stat` is not per-thread.** A non-leader tid looked up as
`/proc/<tid>` is served by `proc_tgid_stat`, i.e. **thread-group totals printed with the
thread's own comm**: `fs/proc/base.c:3308` (`tgid_base_stuff`) -> `fs/proc/array.c:677-681`
(`whole = 1`) -> `thread_group_cputime_adjusted` (`array.c:570-577`). Only
`/proc/<pid>/task/<tid>/stat` (`base.c:3661`, `tid_base_stuff`) -> `whole = 0`
(`array.c:671-675`) is that thread's own time. Observed here on the same thread at the
same instant: 82726+18727 ticks via `/proc/<tid>/stat` versus 1227+1728 via the task path,
with `ps -L` agreeing with the task path (827 s of CPU for a 2-minute-old 45-thread
process is the tell). So: always read the **task** path, and validate against `ps -L`.

### 3.6 What result means "worth it", and what means "stop"

- **Build the pool** when all of: the virtiofs curve plateaus >=2.5x below `/dev/shm`;
  corrected `fs_worker` share >=25%; the sampler shows `D`/syscall-`S` >=40% **or** `R`
  >=80% with >=10 us CPU per op; and the penalty is still >=10% of the arm. Expected gain
  is then ~40 s of the 67-79 s (row 2/3), well above the harness's 5% floor.
- **Stop** when >=60% of samples sit on `do_epoll_wait` (as the dry run showed), or the
  guest curve is still rising from 16 to 32 threads (nothing is being serialised), or the
  corrected share stays ~12%.
- **Skip io_uring either way**, unless the `R`-dominated row wins *and* the pool prototype
  shows the remaining time inside the syscalls it issues. For metadata the ring hands
  those to io-wq (2.2), so its residual advantage over a pool - batched submission, shared
  worker set, no idle thread stacks - is a second-order optimisation on top of the pool,
  not a first move; and note that the fork already carries the pool template for
  virtio-blk (`src/devices/src/virtio/block/worker.rs`, `ReadPool`), while upstream
  virtiofsd ships `process_queue_pool` over one request queue.

## 4. Verdict for the map

- Concurrency is **available and unused**: ~256-wide submission in front of a width-1
  server (`mod.rs:46`; `virtio_fs.c:1398-1408`; `worker.rs:234-268`). That is a fact about
  the design, not yet a reason to change it.
- **io_uring is neither unreachable nor the cheap fix.** It is syscalls plus one more fd in
  the existing epoll; for statx/openat/mutations it is io-wq threads; for cached
  reads/writes it saves syscalls; and in all cases the reply assembly and the
  `add_used`/irqfd path stay on the one thread. A pool over the existing ring reaches the
  same parallelism for less code.
- **The decision cannot be taken on the existing number.** `mon-threads.sh:24` drops
  `stime` for exactly the thread the map is arguing about, and the recoverable share moves
  from ~5 s to ~42 s if the missing half is as large as the one live snapshot suggests.
  Re-measure before re-deciding: one thread capture (cheap, no VM code change), then the
  ~10-minute one-boot probe of section 3 if the corrected share is >=25%.
- Nothing here justifies a fork change today. It reopens ticket 04's "closed on a number"
  as "closed on a number that is read incorrectly", and it prices the next rung (flags and
  DAX in note 15 section 7) above the pool exactly as before.

## Sources

| what | where |
|---|---|
| fork, read at `7160f48f` = `v2.0.0-cang.10-2-g7160f48f` | `deps/libkrun/src/devices/src/virtio/fs/{mod,device,worker,server,filesystem}.rs`, `fs/linux/passthrough.rs`, `virtio/{queue,descriptor_utils}.rs`, `virtio/block/worker.rs` |
| guest kernel, tag `v7.2` | `fs/fuse/virtio_fs.c`, `fs/fuse/dev.c`, `fs/fuse/dev.h`, `fs/fuse/inode.c`, `drivers/virtio/virtio_mmio.c`, `io_uring/{statx,openclose,fs,rw,opdef,io-wq}.c`, `fs/proc/{base,array}.c` - all via `https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/<path>?h=v7.2`, quoted from `grep -n` output |
| Rust ring crate | tokio-rs `io-uring`, `opcode` module (`OpenAt`, `OpenAt2`, `Statx`, `Readv`, `Writev`, `Fsync`, `MkDirAt`, `UnlinkAt`, `RenameAt`, `Close`) - docs.rs/io-uring/latest/io_uring/opcode |
| harness | `tools/rust-compile-bench/{run.sh,mon-threads.sh,summarise-threads.sh}`; the 12.3% row and the three-arm numbers: note 15 section 4, MAP "Numbers on the table" |
| the numbers this note re-uses | guest-virtiofs 298.9/306.5/311.4 s, guest-local `/dev/shm` 232.0-235.1 s, host 104.2/104.7 s; penalty 67-79 s |
