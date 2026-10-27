---
label: wayfinder:research
title: Can a FUSE_LOOKUP be made cheaper rather than fewer - where the per-request latency goes, and which lever is left
status: done
blocked_by: []
claimed_by: pi lookup-latency child (2026-10-27)
---

# 34 - Per-request latency of a FUSE_LOOKUP: what is in the fork, what is in the guest, and what can be removed

Read-only. No build, no VM arm, no tracked file touched. Fork read at
`/home/dev/cang/cang/deps/libkrun` (`git describe --tags` =
**v2.0.0-cang.12**, clean worktree, HEAD `d1c58a9`); guest kernel citations are
from **linux-7.2.10** - the version the firmware actually pins
(`deps/libkrunfw/Makefile:1`), fetched as `fs/fuse/{dir,dev,virtio_fs}.c` from
`https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/plain/<path>?h=v7.2.10`
(the *torvalds* tree has no `v7.2.10` tag; note 22 quoted `?h=v7.2` mainline,
so a few of its line numbers differ from these by a handful of lines). Probe
numbers are re-used from notes 20, 21, 23 and 23-§8 (zmo-verify), never
re-measured here.

**Two trees, disclosed.** The read-only copy of the fork that this session can
reach is `/home/dev/cang/cang/deps/libkrun` = **v2.0.0-cang.12** (`1b3be5a`,
clean). *This* worktree's submodule `deps/libkrun` is **v2.0.0-cang.11**
(`9e423e7`) **plus uncommitted edits to `fs/server.rs` and `fs/device.rs`**
(a sibling session's positive/negative lookup split and its
`CANG_FS_{ENTRY,ATTR}_TIMEOUT` knobs - see §5). `diff -u` between the two copies
is **empty** for `fs/worker.rs`, `fs/linux/passthrough.rs`, `fs/augment_fs.rs`
and `virtio/queue.rs`, so every citation of those holds in both; for
`fs/server.rs` and `fs/device.rs` the line numbers below are the .12 ones, i.e.
~30 lines *lower* after line 100 than the .11+edits file.

## Verdict

1. **A LOOKUP cannot be made much cheaper inside the server, and the arithmetic
   says so.** One request costs **~130-144 us of wall at one guest thread**
   (p50, `stat` = exactly one `FUSE_GETATTR`), of which the fork's own CPU is
   **8-19 us** (worker share / ops-s). Everything the code audit finds in that
   8-19 us - one redundant host `statx` on the negative path, five small
   allocations, one of two lock pairs, one signal per reply - adds up to
   **~3-8 us per request**, i.e. **5-6% of the round trip** and, at this map's
   own conversion (1 us off every request ~ 1.6 s of build), **5-13 s of a
   176 s build in the best case and ~1-3 s realistically**. It is worth doing
   because it is nearly free, not because it closes the gap.
2. **What is left (~110 us of 130) is not request *handling*, it is waiting:
   the request crosses six wakeups** (guest kick -> dispatcher wake -> [pool
   thread wake -> dispatcher wake] -> vCPU wake -> guest kworker -> blocked
   caller task). No per-request timing exists anywhere in the stack to split
   them: `grep -rn Instant src/devices/src/virtio/fs/` is **empty**, and the
   notes' only latency evidence is per-op p50 from a guest probe.
3. **The cheapest instrument is a file read, not a patch**: field 2 of
   `/proc/<pid>/task/<tid>/schedstat` (time runnable but waiting for a CPU) for
   the `fs worker` thread decides lever (a) - the spin-before-sleep - before any
   code is written. That single number says whether dozens of us per request
   are host scheduler latency or not.
4. **The one lever big enough to matter that this investigation found is not on
   the cheapen axis at all: the server's `-ENOENT` reply throws away the
   kernel's negative cache, and the kernel will cache a negative if the reply is
   `nodeid = 0` + a valid `entry_valid` instead** (`fs/fuse/dir.c:587-589`,
   `:636-639`, `:654-657`, `:405-417`). That is ~15 lines in
   `linux/passthrough.rs::lookup`, zero new semantics for files the guest
   creates (a create sends `LOOKUP_EXCL`, which defeats the cache,
   `dir.c:409`), and it deletes whole requests rather than shortening them.
   **It is also the reason the existing `entry_timeout` arm measured nothing:
   raising `entry_timeout` could never touch a `-ENOENT` reply, so the arm was
   structurally incapable of moving the 91%-negative class.** Its ceiling is
   `0.87 M negative LOOKUPs x R x 28.6 us`, where R is the unknown repeat share
   within the 5 s timeout: R=10% -> 2.5 s, R=25% -> 6.2 s, R=50% -> 12.4 s.
   R is measured by the same 40-line instrument (§5), not guessed.
5. **Do not retry** the worker pool, DAX, entry/attr-timeout extension,
   `cache_policy = always`, multiqueue, io_uring/passthrough or MSI-X: §4(e)
   lists the measurement that already closed each one.

**Recommendation, in order:** (i) take the free server-side wins of §4(c) and
(b) - ~3-8 us/request, no semantics, fork-only, one commit; (ii) add the §5
instrument (per-opcode latency histogram + negative-repeat counter + loop
counters) and the zero-code `schedstat` read, and let them decide between the
negative-cache reply and the worker spin; (iii) only then touch the worker
loop.

## 1. The per-request latencies that already exist

Every cell the probes printed carries both a latency and a throughput; the
notes usually quoted one of them. This is the full set, per *op* (a probe op is
1-3 requests, see the request-count column) - anything labelled p50/p99 is
**latency**, anything labelled ops/s is **throughput**, `worker%`/`worker CPU`
is **server utilisation**, `vcsw/op` is **switches per op**.

Requests per op (from the kernel, note 22 §1.1): `open` 2 (`OPEN`+`RELEASE`),
`read` 3, `stat` 1 (`GETATTR`), `statwarm` 0, `readdirplus` 1 opendir + n
readdirplus. With zero-message open on, `open` is 0-1 requests; that is why its
p50 moves.

### 1.1 The clean session (note 23 §8.5, zmo-verify - the numbers to quote)

| fs | mode | thr | ops/s | p50 us | p99 us | requests/op |
|---|---|---|---|---|---|---|
| virtiofs | open (lever off) | 1 | 1892 | 518.7 | - | 2 |
| virtiofs | open (lever on) | 1 | 5454 | **135.3** | - | 0-1 |
| virtiofs | open (lever on) | 16 | 19203 | 799.3 | - | 0-1 |
| virtiofs | open (lever on) | 64 | 19115 | 4096 (bucket edge) | - | 0-1 |
| virtiofs | stat | 1 | 6277 -> 7132 | **129.3 -> 129.3** | - | 1 |
| virtiofs | statwarm | 1 | 451481 | 2.0 | - | 0 |
| virtiofs | readdirplus | 1 | 131 -> 4927 | 8192 -> 195.3 | - | n |
| virtiofs | readdirplus | 64 | 327 -> 23361 | 262144 -> 4096 | - | n |
| shm (control) | open/stat/statwarm | 1 | ~340-500 k | 2.0-3.0 | - | 0 |

### 1.2 Note 23 §3 (same binary, one boot per arm, earlier session)

`open@1` p50 **541.7 -> 165.0 us**, `open@16` 4096 -> 848, `open@64` 16384 ->
4096; `read@1` 632.3 -> 322.7; `stat@1` **133.0 -> 144.0**; `statwarm@1` 2.0 ->
2.0; `readdirplus@1` 8192 -> 215.3. Server CPU per cell: `open@1` 13.4 -> 8.2%
of a core, `open@64` 27.5 -> 24.7%, `readdirplus@1` 73.0 -> 0.0% (the guest's
directory cache, not the server).

### 1.3 Note 20 run B - the only per-op CPU decomposition, and the only
per-thread counts (co-tenant session, absolute values inflated)

| mode | wall/op | guest CPU/op | worker CPU/op | vcsw/op | same op on tmpfs |
|---|---|---|---|---|---|
| open | 934 us | 411 us | 128 us | 3.72 | 4.4 us |
| read | 1189 us | 511 us | 162 us | 4.63 | 6.3 us |
| stat | 357 us | 161 us | 43 us | 1.23 | 2.9 us |
| readdirplus | 14.9 ms/scan | 2.2 ms | 10.5 ms | 10.2 | 0.65 ms |

Divided by requests/op: **per request** 467/205/64 us (open), 396/170/54 (read),
357/161/43 (stat) - note 22 §1.1. The 43 us of worker CPU per single `GETATTR`
is the one figure that does not survive the clean session: `open@1` lever-on at
5454 ops/s with the worker at 8.2% of a core is **15 us/op**, and `stat@1` at
6268-7132 ops/s with 12-13% is **~19 us/op**. Treat **8-19 us/request** as the
server's cost and note 20's 43-64 us as a co-tenant-session artifact.

Thread scaling (note 20 §2, ops/s): `stat` 2802 -> 12962 at 16 threads -> 13118
at 64; `open` 1071 -> 4127 -> 4083. Little's law on the 16-thread `stat` cell
(18645 ops/s, p50 858 us in the clean session) gives a **serial capacity of
~54 us per request** - the plateau is a serial resource, not worker CPU
(§6).

## 2. The fork's request path, cost by cost

Fork = `deps/libkrun`, v2.0.0-cang.12. The pool is on by default
(`DEFAULT_WORKER_THREADS = 4`, `worker.rs:40`; cang sets it explicitly,
`crates/cang-libkrun/src/lib.rs:39`, `CANG_FS_WORKER_THREADS`).

### 2.1 The loop - already deep-draining

| what | where |
|---|---|
| one dispatcher thread, epoll over {hpq evt, req evt, stop, pool completion} | `worker.rs:436-441`, `:474-497` |
| blocking point: `epoll.wait(32, -1, ...)` | `worker.rs:499-501` |
| wake: eventfd read, then `loop { disable_notification; process_queue; enable_notification }`, break only when the avail ring is empty | `worker.rs:557-583` (`:559` read, `:566-582` loop) |
| `process_queue` pops **all** available chains before returning to epoll | `worker.rs:610-629` |
| one chain: `lone = queue.len() <= 1`, pop, then inline or pool | `worker.rs:644-659`, `:620-627` |
| inline serve (the pre-pool path, used for a lone request) | `worker.rs:663-683` |
| pool dispatch (bounded `size*4`), pool thread serves, completion pushed back | `worker.rs:148-160`, `:183-204` |
| completions published by the dispatcher: `add_used` + `needs_notification` + `signal_used_queue` **per reply** | `worker.rs:586-608` (`:596-600`, `:601-604`, `:605`) |

**Fact: deep-drain is already implemented** (lever (b)'s first half is done).
What is *not* done is signalling once per drain: both the inline path
(`worker.rs:677-682`) and the completion loop (`:601-607`) signal per reply.

**Fact: with the pool active, a non-lone request costs two extra thread hops**
(dispatcher -> pool thread, pool thread -> dispatcher over an eventfd), which is
the price cang added in note 21. Note 21's own A/B bounds each hop at a few us
at most: a full build with the pool measured 308.8/303.7/298.6 s against
322.7/320.2/300.2 s without (+1.6 M requests x 2 hops x cost; a 20 us hop would
have cost 60 s and did not appear).

### 2.2 Per-request allocations (all of them)

| allocation | where |
|---|---|
| `Reader::new` collects readable descriptors into a `VecDeque<VolatileSlice>` | `descriptor_utils.rs:216`, called `worker.rs:313` |
| `Writer::new` the same for writable | `worker.rs:320` |
| `Reader/Writer::consume` allocates `Vec::with_capacity(buffers.len())` per call, so >= 2 more per reply | `desc.rs:91`, `server.rs:1609`, `:1613` |
| the LOOKUP name: `let mut buf = vec![0u8; namelen]` + `read_exact` | `server.rs:303-307` |
| the name again as a `CString` by the virtual-entry overlay, on **every** lookup including the miss path | `augment_fs.rs:161` (map `:55`, read lock `:162`) |
| one crossbeam node per job and per completion | `worker.rs:156`, `:194-199` |

Five small `malloc`s per negative lookup, ~0.3-0.5 us total. Not where the
latency is, but free to remove for the name (a stack buffer < 256 B covers
every rustc probe).

### 2.3 Locks and the "cheap-lookup" cache that already exists

| what | where |
|---|---|
| `inodes: RwLock<MultikeyBTreeMap<Inode, InodeAltKey, Arc<InodeData>>>`, `handles: RwLock<BTreeMap<..>>` | `passthrough.rs:489`, `:494`; `multikey.rs:21-22` |
| lookup: **read** lock for the parent, `statx_at(parent, name)`, **read** lock + `get_alt(ino,dev,mnt_id)`, hit -> `refcount.fetch_add(Acquire)` and return | `passthrough.rs:1138-1144`, `:1157`, `:1165`, `:1177-1190` |
| miss -> `openat(O_PATH|O_NOFOLLOW|O_CLOEXEC)` + `statx(fd)` + read lock probe + (first time only) `next()` + **write** lock insert | `passthrough.rs:1195-1204`, `:1209`, `:1223`, `:1233-1249` |
| the cache: **not** parent+name. `(ino, dev, mnt_id) -> inode`, positive only, populated on inode creation, dropped by `forget`/`batch_forget` | `multikey.rs:58-68`, `passthrough.rs:1266-1278` |
| a real name->inode map exists only for *virtual* entries (`AugmentFs`) | `augment_fs.rs:55`, `:162` |
| hit statistics: only the committed temporary instrumentation `DBG_HITS`/`DBG_PROBES` (two relaxed adds per lookup, logged every 200 k hits) | `passthrough.rs:192-193`, `:1158-1175` |

So a steady-state lookup takes **two read locks and no write lock** (only a
first-seen inode writes), and **it is already the "cheap" path**: the statx fast
path was added so a `readdirplus` scan is 4096 `statx` instead of 4096
`openat+statx+close` triples.

**The one real defect on this path is the negative case, which is 91% of
lookups.** `statx_at` is tried first (`:1157`); when it fails with `ENOENT` the
code falls through to `openat` (`:1195`), which fails with `ENOENT` too and is
replied (`:1202-1204`). **A negative lookup costs two host path walks**
(`statx_at` `passthrough.rs:262-283`, `openat` `:1195-1201`) where one would do.
`grep -n ENOENT linux/passthrough.rs` finds no ENOENT handling in `lookup`, and
the protocol hook that would let the guest cache the answer
(`Entry.inode == 0`, `filesystem.rs:30-33`) is never produced.

### 2.4 The reply: write, used ring, interrupt

| step | where |
|---|---|
| reply body copied into the descriptor's writable slices | `reply_ok` `server.rs:1587-1622` -> `Writer::write_all` -> `desc.rs:462-476` |
| used ring: one entry + idx, `SeqCst` fence | `Queue::add_used` `queue.rs:544-580`, called `worker.rs:596-600` / `:674-676` |
| `needs_notification`: fence + `used_event` guest-memory load, already the kernel's batched `num_added` form | `queue.rs:713-747` (`:734-743`) |
| signal: two mutexes (status, irqchip) + one irqfd eventfd write | `device.rs:64-76` -> `mmio.rs:120-129` (`:121-122` status mutex, `:124-126` irqchip mutex) |
| the irqfd itself | `kvm/mmio.rs:142`; the kick side is an ioeventfd, `kvm/mmio.rs:132-139` |

**Fact: one interrupt kick per reply, and it can be hoisted to one per drain**
without touching the protocol, because `needs_notification`'s `num_added`
arithmetic is exactly the kernel's batched form (`queue.rs:734-743`); moving
both call sites after their loops is protocol-legal. It saves 2 mutex
acquisitions + 1 eventfd write per co-drained reply, and lets the guest's
ISR-read/ISR-ack pair (2 exits, note 22 §1.3) cover several replies.

### 2.5 What the fork already does *not* cost

- `FUSE_PARALLEL_DIROPS` **is** negotiated (`server.rs:1015-1026`, `:1016`), so
  lookups in one directory do **not** serialize on the guest's `fi->mutex`
  (`dir.c:632-635`) - a candidate serialization that is already gone.
- No per-request timer, counter, or clock anywhere: adding one is not fighting
  existing machinery, it is the first of its kind.

## 3. The guest side: yes, every negative lookup blocks the caller

All citations: linux-7.2.10, stable tree, as fetched.

1. **A LOOKUP is fully synchronous and never backgrounded.** `fuse_lookup_name`
   -> `fuse_simple_request` (`dir.c:586`) -> `__fuse_simple_request` ->
   `fuse_chan_send` (`req.c:60-69`) -> `fuse_get_req(fch, false)` (so no
   `FR_BACKGROUND`, `dev.c:142-143`) -> `__fuse_request_send` (`dev.c:746-760`,
   `BUG_ON(FR_BACKGROUND)` at `:750`) -> `fuse_send_one` (`:755`) ->
   `request_wait_answer` (`:757`).
2. **The wait**: `wait_event_interruptible(req->waitq, FR_FINISHED)`
   (`dev.c:704-706`); virtiofs's `send_interrupt` is an empty TODO
   (`virtio_fs.c:1284-1293`), so a signal just falls through to
   `wait_event_killable` (`dev.c:720`) and `wait_event` (`:743`). Nothing but
   the reply or teardown ends it.
3. **One kick per request, never a batch.** The sg list is built for this
   request only (`virtio_fs.c:1431-1445`, 4 elements for a LOOKUP,
   `:1316-1333`), added once (`:1461`), then `virtqueue_kick_prepare` (`:1479`)
   and `virtqueue_notify` (`:1484`) in the same `fsvq->lock` critical section
   (`:1452-1481`). `virtqueue_add_split` increments `vq->num_added` per call and
   `kick_prepare` zeroes it (`virtio_ring.c:809`, `:853-855`), so a stream of
   LOOKUPs can never coalesce into one kick from the guest side. Only the host
   can suppress a notify (`VRING_USED_F_NO_NOTIFY`, `virtio_ring.c:864-866`).
4. **The reply takes an extra workqueue hop.** The vq callback only schedules
   work: `virtio_fs_vq_done` -> `schedule_work(&fsvq->done_work)`
   (`virtio_fs.c:920-927`); a kworker runs `virtio_fs_requests_done_work`
   (`:828-879`), drains with `disable_cb`/`get_buf`/`enable_cb` (`:841-853`),
   then completes each request (`:870`) -> `fuse_request_end` -> `wake_up`
   (`dev.c:654-656`). A LOOKUP does **not** take the second per-request work
   item, because that is gated on `args->may_block` (`virtio_fs.c:861-868`).
5. **So a lookup costs three context switches** (caller out, kworker in, caller
   in - possibly on another vCPU, i.e. an IPI) plus 3-4 spinlock acquisitions
   (`fiq->lock` for the unique id, `fsvq->lock`, `fpq->lock` on submit and again
   on completion) and 2-3 `kmalloc`s (`fuse_req_cachep` `dev.c:33`, the forget
   link `dir.c:577`, the argbuf bounce `virtio_fs.c:710` with the memcpy at
   `:714-718`).
6. **The guest is blocked on the host reply for every LOOKUP, negative
   included**, so per-request latency is the whole story for 54% of the
   workload's requests. What the build sees of it is a different number: see
   §6, ~22% of it is exposed.
7. **Negative caching: the reply form decides everything.**
   - `-ENOENT` reply: `fuse_lookup` sets `outarg_valid = false`
     (`dir.c:636-639`) and calls `fuse_invalidate_entry_cache` (`:657`) ->
     `fuse_dentry_settime(entry, 0)` (`:349-352`) -> the dentry is never valid;
     `fuse_dentry_revalidate` re-sends on the next probe (`:408-417`: the
     "For negative dentries, always do a fresh lookup" branch is inside the
     timeout-expired branch).
   - `nodeid == 0` reply: `fuse_lookup_name` returns success with
     `*inode = NULL` while keeping the reply's timeout - the comment at
     `dir.c:587` is literally *"Zero nodeid is same as -ENOENT, but with valid
     timeout"* - so `outarg_valid` stays true and
     `fuse_change_entry_timeout` (`:654-655`, `:305-309`) stamps the negative
     dentry. A repeat inside the timeout then takes `ret = 1` at `:470` and
     sends **no request**.
   - `LOOKUP_EXCL|LOOKUP_REVAL|LOOKUP_RENAME_TARGET` always defeat it
     (`dir.c:409`), which is what makes the trick safe for the guest's own
     creates; there is no `FUSE_NEGATIVE_ADVISE` and no separate negative
     timeout knob in 7.2.10 (grep of the fetched sources).
   - **cang's server returns `-ENOENT`** (§2.3), so it is in the first form and
     the guest re-probes every miss every time.

## 4. Levers, ranked, with the site to change

### (a) spin before sleeping in the worker - fork only

Site: `worker.rs:499-501` plus the loop at `:500-538`. Today the worker
`epoll.wait(..., -1, ...)`s and is woken by the queue eventfd (which the vCPU
thread writes straight through the ioeventfd, `kvm/mmio.rs:132-139`). A bounded
spin - after `enable_notification` returns `false`, poll `avail->idx` in guest
memory (a plain volatile load through `self.mem`) for `CANG_FS_POLL_US`, and
only then `epoll_wait(-1)` - removes the sleep+wake from every request. It is
legal because the avail ring, not the eventfd, is the source of truth; the
eventfd still needs reading once the spin gives up. Cost: one host core per
share while a workload is running (a knob, default 0 = today). **Expected
effect = the worker's runqueue wait, which is unmeasured; the measurement is
one file read (§5 tier 0).** If that read says 2 us, this lever is dead; if it
says 20 us, it is worth 1.6 M x 20 us and dwarfs everything else here.

### (b) signal once per drain, not once per reply - fork only

Site: `worker.rs:601-607` (hoist out of the completion loop) and `:677-682`
(inline path). ~10 lines. Saves 2 mutexes + 1 eventfd write per co-drained
reply and coalesces the guest's ISR pair. Requires knowing the drain depth; the
instrument counts it. Expected: **1-2 us per reply beyond the first in a
drain**, and at depth 1 nothing. Cheap; do it with (c).

### (c) the negative path's redundant syscall and allocations - fork only

Site: `passthrough.rs:1157` + `:1195-1204`. When `statx_at` fails with
`ENOENT`, return `Err(ENOENT)` directly instead of falling through to an
`openat` that is guaranteed to fail - **one of two host path walks removed from
91% of lookups** (keep the fall-through for other errors and for the
"statx ok, inode unknown" case). Same commit: `server.rs:303-307` name buffer ->
stack/heap-reused buffer, `augment_fs.rs:161` -> compare without the `CString`
clone, and drop the `DBG_*` counters. Expected **3-8 us per negative request**
(10-25% of the server's share), near-zero risk, no protocol or semantic change.

### (d) remove a thread hop - fork + kernel

Two candidates, both bounded small by note 21's pool A/B: publish the used-ring
entry from the pool thread that served it (needs the used ring shared/atomic,
`worker.rs:586-608` + `queue.rs:544`), or complete the request inline in the vq
callback instead of `schedule_work` (`virtio_fs.c:926` -> call
`virtio_fs_request_complete` there; kernel/firmware change, hardirq context).
Each removes one wake of ~1-3 us into a path that already has six. Bad ratio -
skip unless the instrument says a single hop is unexpectedly expensive.

### (e) already measured flat or impossible - do not retry

| lever | the measurement that closes it |
|---|---|
| worker pool (more threads behind the one queue) | note 21: neutral on open/read/stat (1.00-1.03x), build 308.8/303.7/298.6 vs 322.7/320.2/300.2 s, i.e. no win; it only moves readdirplus, which zero-message open made moot (note 23: 73.0% -> 0.0% server CPU) |
| DAX window (per-connection or per-inode) | notes 05, 14, 18: dropped; no fallback when the window fills |
| longer `entry_timeout`/`attr_timeout` | the 5 s -> 60 s arm: positives -28.5%, total LOOKUP -2.3%, GETATTR +39.8%, no wall change - and per §3.7 it *cannot* touch a `-ENOENT` reply, so it can never address the 91% class |
| `cache_policy = always` | note 30: adds nothing over the kernel's no-open default (`fs/fuse/file.c:154-156`), reached by one site only (the `CREATE` reply) |
| multiple request queues | no such feature for virtio-fs (`num_request_queues = 1`), and it is the pool's prerequisite, not a latency lever (note 22 §2 rank 8) |
| io_uring / `FUSE_PASSTHROUGH` | both need a `/dev/fuse` file bound to the connection; a virtqueue server has none (notes 22 §3, 24) |
| virtio-pci + MSI-X to delete the ISR exits | ~3-5 us/request, needs a second transport in the fork; bad ratio (note 22 §2 rank 9) |

### (f) the one that is off the topic but wins: a cacheable negative reply - fork only

Site: `passthrough.rs::lookup`'s error branch (`:1202-1204`) plus
`filesystem.rs:30-33`. Reply `Entry { inode: 0, entry_timeout: cfg.entry_timeout,
.. }` instead of `Err(ENOENT)`; the kernel then caches the negative for 5 s
(`dir.c:587-589`, `:654-655`, `:470`). Guards: do not touch `refcount` or
`nlookup` for a zero inode; `create` still sends `LOOKUP_EXCL` (`dir.c:409`), so
a create is never blocked by a stale negative; the staleness window equals the
one positive entries already have (note 13). Ceiling
`0.87 M x R x 28.6 us` (§6). **This is the highest expected payoff per unit of
risk in the whole list, and R - the share of negatives that are repeats inside
the timeout - is the entire question.** Nobody has measured R, and the existing
`entry_timeout` arm proves it *could not*: with the current `-ENOENT` reply form
the kernel has nothing to expire, so that arm's negatives were constant by
construction while its positives fell 28.5% (§5).

## 5. The instrument that settles where the latency is

Two tiers, in this order, and the first one is free.

### Tier 0 - no code: `schedstat`

The probe harness already read field 1 (runtime) of `schedstat` for the
`fs worker` thread to get `worker%` (note 20 §2). Field 2 is *time spent
runnable, waiting for a CPU* and field 3 is the timeslice count; nobody ever
looked. Sample per tid (`/proc/<cang-pid>/task/*/schedstat`, matching comm
`fs worker` and `fs work N`) at 10 Hz through a build and through a probe cell,
and report `d_wait / d_requests` and `d_runtime / d_requests` using the
request delta from the existing opcode histogram (`CANG_FS_STATS=1`,
`worker.rs:541-555`).

- `d_wait/requests` >= 10 us -> lever (a) is the answer, build the spin knob.
- `d_wait/requests` <= 2 us -> the worker's wake is not the cost; the residual
  is guest-side (the three switches and the kworker hop of §3.5) and no
  fork-only lever can reach it.
- `voluntary_ctxt_switches` delta per request (`/proc/<tid>/status`) says
  whether the worker sleeps once per request or once per drain - i.e. whether
  (b) has anything to batch.

### Tier 1 - ~40 lines in the fork, reusing what is already there

- **Already there in this worktree, do not rebuild it**: the .11+edits
  `server.rs` carries `lookup_positive` / `lookup_negative` / `lookup_error`
  (`:106-107`, incremented at `:340`, `:347`) and prints
  `{miss}% of lookups found nothing` in `log_opcode_histogram` (`:190-198`).
  That is where the brief's **91% negative** comes from. Its *split* is the
  strongest existing evidence for lever (f): the `entry_timeout` 5 s -> 60 s arm
  recorded in the RSI skill store (not stored as a note in this map) moved
  **positives -28.5% and the total LOOKUP count only -2.3%**, i.e. the negative
  count did not move at all - exactly what `dir.c:636-639`/`:657` predicts for
  a `-ENOENT` reply.
- **Per-opcode latency histogram**: in `server.rs::handle_message`
  (`:191-212`), take `Instant::now()` after `count_opcode` (`:211`), wrap the
  dispatch `match` in a `let res = match ...;`, and record
  `t0.elapsed()` into `lat[opcode][bucket]` with 16 log2 buckets
  (1..32768 us) plus a max. `Instant` is `clock_gettime(CLOCK_MONOTONIC)` via
  vDSO (~20-25 ns, <0.05% of a request). Print it from
  `log_opcode_histogram` (`server.rs:162-189`, already wired to milestones and
  to teardown at `worker.rs:550-555`) as `p50/p90/p99` **per opcode** next to
  the counts.
- **Negative-repeat counter** (this is what decides lever (f)): in
  `passthrough.rs::lookup`'s failure branch, probe a fixed 8192-entry
  open-addressed table keyed by `hash(parent, name)` carrying the µs timestamp
  of the last miss; count `neg_repeat` when the entry is younger than
  `entry_timeout`. ~4 counters (`neg`, `neg_repeat`, `pos_fast`, `pos_slow`),
  printed with the histogram. Print *R = neg_repeat/neg*.
- **Loop counters** in `worker.rs`: epoll returns (`wakes`), chains served,
  signals, pool dispatches, completions, and a small histogram of *chains per
  wake*. `wakes` vs served answers "does every request pay a host sleep";
  the drain-depth histogram prices lever (b).
- **One knob**: `CANG_FS_POLL_US` (0 = today) wired like the other device
  settings (`device.rs:167-169`, `libkrun/src/api/device_builders.rs`,
  `crates/cang-libkrun/src/lib.rs:39`) so lever (a) is an A/B **in one binary**,
  the way `CANG_FS_ZERO_MESSAGE_OPEN` was (note 23 §8.1).

What it must be differenced against: the guest probe's per-op wall p50 (note 23
§8.5) over the same opcode. `guest_wall_per_op - server_service_per_op` is the
transport + scheduling residual, and it is the number every lever in §4 has to
attack. Today nobody knows it: note 22 §1.4 says outright that nothing in
`fs/fuse` or `drivers/virtio` fills the remaining 120-180 us per request.

## 6. The arithmetic: what any per-request lever can be worth

| quantity | value | source |
|---|---|---|
| virtio-fs build, lever on | 176.0 s (n=4, 162.6-185.3) | note 23 §8.2 |
| guest-local `/dev/shm`, same binary, same lever | 131.5 s | note 23 §8.2 |
| host native | 130.2 s | note 23 §8.2 |
| **exposed transport cost** | **44.5-45.8 s (25-26% of the arm)** | derived |
| requests, lever on | ~1.6 M (1.5 M served; 1 M at 3.26% READ) | note 23 §2, §8.1 |
| **exposed latency per request** | **~28.6 us** | 45.8 s / 1.6 M |
| avg unloaded latency per request | ~130-144 us (p50, 1 thread) | note 23 §8.5 |
| negative LOOKUPs | ~0.87 M (54.06% of requests, 91% of those negative) | note 23 §2 |
| server-attributable cost per request | 8-19 us (15-25% of it) | §1.3 |
| removable server-side cost, all of §4(b)+(c) | 3-8 us/request | §4 |

Consequences:

- **1 us off every request ~ 1.6 s of build time**, and the absolute ceiling
  (every request instantaneous) is the 45.8 s gap. Only ~22% of a request's
  130 us currently shows up as build time - the other 78% is overlapped by 26
  concurrent rustc processes - so a lever must land on the exposed part, which
  is exactly what the §5 histogram is for.
- **The whole §4(c)+(b) programme is worth ~5-13 s in the linear case and ~1-3 s
  once the 22% overlap is taken into account.** It does not close a 45.8 s gap.
- **Lever (f) is worth `0.87 M x R x 28.6 us`** = 2.5 s at R=10%, 6.2 s at
  R=25%, 12.4 s at R=50%. It wins because it removes requests rather than
  shortening them, which is the axis this map has already proved pays
  (zero-message open: 2.4-2.7x on the whole build).
- **Lever (a) is worth `1.6 M x (the worker's runqueue wait)`** after the
  overlap discount, i.e. 0-30 s; it is the only fork-side lever that can be in
  the tens of us, and the only unknown left is the one `schedstat` answers for
  free.
- **The ~54 us of serial capacity the 16-thread cell exhibits** (§1.3) is the
  other thing a per-request lever cannot fix: at that point latency is queueing,
  and the serial resource is one of {the single dispatcher's per-request work,
  the guest's `fsvq->lock`, the one `done_work` kworker}. The tier-1 counters
  (`wakes` vs served, chains per wake, dispatcher schedstat) name which, and if
  it is the guest's spinlock or the kworker, no fork change can touch it.

## 7. What I could not verify, and the risks in the above

1. **No per-request timing exists anywhere in this stack** (zero `Instant` in
   `src/devices/src/virtio/fs/`), so every "per request" figure here is derived:
   either a probe op divided by its request count (note 22 §1.1's mapping), or
   worker CPU share divided by ops/s. The 8-19 us server cost is an aggregate,
   not a distribution; the 28.6 us exposed latency is a build-level quotient.
2. **The 130-144 us is a one-thread p50 from a `stat` cell of a specific
   session.** Absolute seconds in this map are session-scoped - the host-native
   arm itself moved 104 -> 130 s between sessions (note 23 §8.3) - so the
   conversion "1 us ~ 1.6 s" inherits that 1.25x.
3. **Note 20's worker-CPU-per-op (43-64 us) and guest-CPU-per-op (161-205 us)
   are from a co-tenant session** with 26 vCPUs on 28 CPUs; note 22 §1.4 shows
   the guest column is tick-accounted at HZ=250 and contaminated by steal. I
   used the clean session's 8-19 us, which is 3-5x smaller, and both could still
   be wrong by 2x.
4. **The repeat share R is unknown and no existing note bounds it.** The
   `entry_timeout` 5 s -> 60 s arm cannot: per `dir.c:636-639` and `:657`, a
   `-ENOENT` reply never creates a cacheable dentry, so that arm only ever
   priced positives. Any claim that "negatives are unique names" is therefore
   unmeasured - it is what §5's counter is for.
5. **Lever (f) is a proposed code change, not a tested one.** The kernel side is
   verified from source (`dir.c:587-589`, `:654-657`, `:470`); the server side
   is not: whether cang's reply path accepts `inode = 0` without tripping an
   assertion in `AugmentFs`/`Server`, and whether the 5 s window is acceptable
   for a host-created file the guest then probes, are open. The staleness
   class is the same one positive entries already have (note 13), and
   `LOOKUP_EXCL` protects creates, but I did not build or run it.
6. **The fork I read is v2.0.0-cang.12, and this worktree's submodule is
   v2.0.0-cang.11 + uncommitted `fs/server.rs`/`fs/device.rs` edits** (the
   positive/negative lookup counters and the timeout knobs, §5); the four files
   I lean on for the request path are byte-identical between them. The .12 copy
   also carries committed temporary instrumentation in `lookup`
   (`DBG_HITS`/`DBG_PROBES`, `passthrough.rs:192-193`) that any latency work
   should delete first (two relaxed atomics per lookup, and a `% 200000` on a
   shared counter).
7. **Whether `FUSE_PARALLEL_DIROPS` is actually negotiated at run time** is a
   runtime fact (the fork offers it, `server.rs:1016`); I read the code, not a
   mount's INIT reply.
8. **Kernel line numbers are 7.2.10's.** Note 22's numbers came from mainline
   `?h=v7.2`; where the two differ, this note's are the 7.2.10 ones (verified
   against the fetched files: `Makefile` VERSION 7 / PATCHLEVEL 2 / SUBLEVEL 10).
9. No build, no boot, no A/B: this note is source reading plus other notes'
   measurements, by the brief's constraints. Everything in §4 is a prediction
   with a stated measurement that would confirm it.

