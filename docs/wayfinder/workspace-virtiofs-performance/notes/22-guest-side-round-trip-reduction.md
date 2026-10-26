---
label: wayfinder:research
title: How can the cost of a FUSE round trip be reduced on the *guest* side, given the measured 934 us open?
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
# 22 - Guest-side round-trip reduction

Note 20 measured one `openat`+`close` on the share at **934 us wall / 411 us of
guest CPU / 128 us of worker CPU** (tmpfs: 4.4 us), and read as **1189/511/162**,
`statx(FORCE_SYNC)` as **357/161/43**. It also left the mechanism of that guest
cost explicitly unresolved ("whether all of it is genuine guest execution or
partly 100 Hz tick attribution ... is not resolvable with the counters used
here"). This note answers the question with primary sources: it decomposes the
guest side of one round trip, prices every transition and every piece of guest
work, and ranks the levers the guest side actually has.

Read-only: no build, no VM arm. Kernel sources are `linux` tag **v7.2** (the
pinned guest is `linux-7.2.10`, `deps/libkrunfw/Makefile:1`), fetched from
`git.kernel.org` `plain/?h=v7.2` and quoted by `grep -n` line numbers; the fork
is `deps/libkrun` at `7160f48f`; the device model is OASIS **virtio 1.2**
(§2.7.13, §2.7.14, §5.11).

## Verdict

1. **The 411 us does not decompose into guest execution, and cannot be made to.**
   Normalised per FUSE *request* - an `openat`+`close` is **two** requests, a
   `read` is **three**, a forced `statx` is **one** (§1.1) - the three modes cost
   **161-205 us of accounted guest CPU each**, i.e. a fixed per-request cost
   independent of the opcode. The code the guest actually runs per request
   (§1.2) is a few kmallocs, a 4-6 entry scatterlist, one MMIO notify and 3-4
   task switches: 10-30 us. The gap is not missing code - it is the counter:
   `CONFIG_TICK_CPU_ACCOUNTING=y` at `HZ=250` (`config-libkrunfw_x86_64-kvm-lto:140,434`,
   no `VIRT_CPU_ACCOUNTING_GEN` at `:141`) and note 20's own `busy` definition,
   which sums `user..steal` and subtracts only idle+iowait
   (`tools/rust-compile-bench/concurrency-scaling.sh:307-330`). Steal is real in
   a 26-vCPU guest on a 28-CPU host with a co-tenant session. **Steer by request
   count, not by the CPU number.**
2. **The transitions are cheap - three exits per request, not two, and only one
   of them is in-kernel.** The fork registers the queue-notify address as an
   **ioeventfd**, so a kick never reaches libkrun
   (`deps/libkrun/src/libkrun/src/vmm/device_manager/kvm/mmio.rs:133-140`); but a
   completion also makes the guest **read `INTERRUPT_STATUS` and write
   `INTERRUPT_ACK`** (`drivers/virtio/virtio_mmio.c:293-294`), and neither address
   is ioeventfd-registered, so **both come out to the VMM** as `VcpuExit::MmioRead/
   MmioWrite` (`.../vmm/linux/vstate.rs:1542-1553`). With the irqfd injection that
   is ~8-18 us per request: **2-5% of a round trip.**
3. **Therefore the only lever with a large numerator is removing requests.**
   Ranked in §2: **zero-message open** (`FUSE_NO_OPEN_SUPPORT`, 2 of 2 requests
   per file open+close) first, then **the INIT-reply flags the fork never acks**
   (`FUSE_MAX_PAGES`, `FUSE_ASYNC_READ`, `FUSE_CACHE_SYMLINKS`), then nothing -
   `FUSE_PASSTHROUGH` and fuse-uring both require a `/dev/fuse` file the guest
   does not have (§3), DAX is dropped by note 05, and the caches, the batch
   sizes and the kick coalescing are already at their ceiling for a one-deep
   synchronous caller.
4. **One thing to do next:** price the class before building it. The pinned
   kernel cannot trace it (`# CONFIG_FTRACE is not set`, `config-...:3630`;
   `# CONFIG_DEBUG_FS is not set`, `:3474`), so the opcode mix has to be counted
   server-side (§4), and then `FUSE_NO_OPEN_SUPPORT` is ~30 lines in the fork's
   server and one re-run of the existing `open` probe mode.

## 1 Decomposing the 411 us

### 1.1 First: how many requests one "op" is

Note 20's numbers are per *syscall pair*, and a syscall pair is not a round
trip. From the guest kernel:

| probe mode | requests per op | why |
|---|---|---|
| `open` | **2**: `FUSE_OPEN` + `FUSE_RELEASE` | `fuse_file_open()` sends `FUSE_OPEN` (`fs/fuse/file.c:138`, `:162`); `close` prepares `FUSE_RELEASE` (`:358`) and sends it as a *background* request (`fuse_file_put()` with `sync = fc->auto_submounts`, `:388` -> `fuse_simple_background()`, `:125`) |
| `read` | **3**: `+ FUSE_READ` | after the passthrough/direct-io switch a normal read takes `fuse_cache_read_iter()` (`fs/fuse/file.c:1850-1855`, `:1113`) |
| `stat` | **1**: `FUSE_GETATTR` | `fuse_do_getattr()` builds `args.opcode = FUSE_GETATTR` (`fs/fuse/dir.c:1476-1497`) |
| `statwarm` | **0** | served from the attribute cache, entry/attr timeout 5 s (`deps/libkrun/src/devices/src/virtio/fs/device.rs:84-89`) |
| `readdirplus` | `1` `FUSE_OPENDIR` + *n* `FUSE_READDIRPLUS` + `1` `FUSE_RELEASEDIR` | `n = ceil(bytes / clamp(ctx->count, PAGE_SIZE, max_pages << PAGE_SHIFT))` (`fs/fuse/readdir.c:383`, `:395`) |

Dividing note 20's run-B table (1 thread) by that count gives the number the
rest of this note works with - **the cost of one request**:

| mode | wall / request | guest CPU / request | worker CPU / request | same op on tmpfs |
|---|---|---|---|---|
| `open` | 467 us | 205 us | 64 us | 4.4 us (2 syscalls) |
| `read` | 396 us | 170 us | 54 us | 6.3 us (3 syscalls) |
| `stat` | 357 us | 161 us | 43 us | 2.9 us (1 syscall) |

Three different opcodes, three different payload sizes, one answer: a request
costs **~360-470 us of wall and 160-205 us of accounted guest CPU**. The guest
cost is per *request*, not per byte and not per opcode - which is what makes it
a transport cost rather than a protocol cost, and what makes the request count
the only decision variable.

### 1.2 What the guest actually executes per request

Two functions named in the question are **not** on this path, and knowing that
matters for where to look:

- **`fuse_dev_do_read()` is not called at all.** It is the `/dev/fuse` read side
  (`fs/fuse/dev.c:1516`), i.e. how a userspace daemon pulls a request out of a
  character device. A virtio-fs client instead hands the request to a virtqueue -
  `fuse_chan_send()` (`fs/fuse/dev.c:809-842`) -> `__fuse_request_send()`
  (`:746-760`) -> `fuse_send_one()` -> the connection's `send_req` op ->
  `virtio_fs_send_req()` / `virtio_fs_enqueue_req()`
  (`fs/fuse/virtio_fs.c:1499`, `:1394-1497`) - and completes it from the used ring
  (`virtio_fs.c:828-879`). The copy work `fuse_dev_do_read` would do
  (`fuse_copy_*`) never happens; the guest instead pays the scatterlist build and
  the argbuf bounce in the table below.
- **`fuse_simple_request()` is now a 10-line wrapper**: `__fuse_simple_request()`
  → `fuse_req_prep()` → `fuse_chan_send()` (`fs/fuse/req.c:52-70`). The request
  machinery was split out of `dev.c` in this kernel (`fs/fuse/{req,dev}.c`,
  `struct fuse_chan`), so citations older than that refactor point at the wrong
  file.

What is left is the real per-request cost - the synchronous path, in order:

| step | code | cost |
|---|---|---|
| VFS -> `fuse_do_open`/`fuse_cache_read_iter`/`fuse_do_getattr` | `fs/fuse/file.c:189-198`, `:1113`, `fs/fuse/dir.c:1476` | the path walk that the `statwarm` control also pays: 3.7 us/op *including* the syscall, so < 2 us/request of it is FUSE |
| allocate the request and its argument buffer | `fuse_get_req()` `fs/fuse/dev.c:108-147` (`atomic_inc`, `kmalloc`), `fuse_chan_send()` `:809-842` | 2 `kmalloc`/`kfree` pairs per request - the argbuf is **always** bounced because stack args cannot be mapped: `copy_args_to_argbuf()` `fs/fuse/virtio_fs.c:696-716`, called at `:1426`; copied back and freed at `:725-759` |
| build the scatter-gather list | `sg_count_fuse_req()` `fs/fuse/virtio_fs.c:1312-1342` ("requests need at least 4 elements", `:1398`), `sg_init_*` `:1345-1390` | 4-6 descriptors written to stack (no `VIRTIO_RING_F_INDIRECT_DESC`, `.../virtio/fs/device.rs:77`) |
| enqueue + kick | `virtqueue_add_sgs()` `fs/fuse/virtio_fs.c:1461`, `virtqueue_kick_prepare()`/`virtqueue_notify()` `:1479-1484` | one MMIO write to `QUEUE_NOTIFY` -> **ioeventfd**, see §1.3 |
| wait | `request_wait_answer()` `fs/fuse/dev.c:697-744` -> `wait_event_interruptible()` `:705` | 1 schedule-out; the vCPU then halts (or spins in `kvm.ko` under `halt_poll_ns=200000`, note 20 §5) |
| reply: IRQ -> workqueue | `virtio_fs_vq_done()` `fs/fuse/virtio_fs.c:920-927` -> `schedule_work()` -> `virtio_fs_requests_done_work()` `:828-879` | **an extra wakeup**: the vq callback does not complete the request, it schedules a kworker; the kworker drains with `virtqueue_disable_cb`/`get_buf`/`enable_cb` (`:841-853`, exactly the loop virtio 1.2 §2.7.14 prescribes) |
| complete + wake the caller | `fuse_request_end()` `fs/fuse/virtio_fs.c:813` -> `fs/fuse/dev.c:628-664` -> `wake_up(&req->waitq)` | 1-2 more switches: kworker out, caller in (possibly on another vCPU: an IPI) |
| for `may_block` requests only (readahead, `fs/fuse/file.c:786`) | a *second* work item: `kzalloc` + `schedule_work(&w->done_work)` `fs/fuse/virtio_fs.c:861-868` | one more workqueue hop per readahead reply |

Counted up: **1 MMIO notify, 1 IRQ entry with two MMIO ISR accesses, 1
workqueue schedule, 3-4 task switches, 2 `kmalloc`/`kfree` pairs, ~4 descriptor
writes** (the transitions themselves are §1.3). That is a 10-30 us
request on any reading of it, and note 20's own corroboration points the same
way: `vcsw/op` was 3.72 for a 2-request op (`~1.9` switches per request, note 19
§3.3's "one sleep per op = the epoll wake" scale), *not* tens.

Two things here are **guest-side and avoidable in principle**: the bounce-buffer
`kmalloc`/`memcpy` for every request (upstream's choice because stack args cannot
be mapped - a kernel patch, not cang's code), and the workqueue hop the vq
callback takes instead of completing in the interrupt (`fs/fuse/virtio_fs.c:926`
could call `virtio_fs_request_complete()` inline). Both are ~1-3 us each: not
worth a kernel patch for a 400 us round trip.

### 1.3 What the transitions cost, and how many there are

Per request->reply the guest makes three MMIO accesses that leave the guest, and
libkrun is entered for two of them:

| # | transition | handled where | cost |
|---|---|---|---|
| 1 | kick: `writel(vq->index, base + VIRTIO_MMIO_QUEUE_NOTIFY)` (`drivers/virtio/virtio_mmio.c:270`) | **in `kvm.ko`**: the notify address is an ioeventfd - `vm.register_ioevent(queue_evt, &IoEventAddress::Mmio(mmiobase + NOTIFY_REG_OFFSET), i)` (`deps/libkrun/src/libkrun/src/vmm/device_manager/kvm/mmio.rs:133-140`) - KVM matches the datamatch, signals the queue eventfd, re-enters the guest; the VMM's userspace fallback (`0x50 => eventfd.write`, `deps/libkrun/src/devices/src/virtio/mmio.rs:361-364`) only runs on a datamatch miss | 1 exit, ~1-2 us |
| 2 | reply ISR: `readl(INTERRUPT_STATUS)` then `writel(status, INTERRUPT_ACK)` (`drivers/virtio/virtio_mmio.c:293-294`) | **userspace**: those addresses are *not* registered, so both arrive as `VcpuExit::MmioRead`/`MmioWrite` and the VMM thread dispatches them on the bus (`deps/libkrun/src/libkrun/src/vmm/linux/vstate.rs:1542-1553`) | **2 exits**, ~2-4 us |
| 3 | injection: `interrupt_evt.write(1)` -> irqfd | `vm.register_irqfd(mmio_device.interrupt_evt(), self.irq)` (`kvm/mmio.rs:142`), one-shot pulse (`deps/libkrun/src/devices/src/virtio/mmio.rs:142-168`) | ~2-6 us |

Plus the device side: the `fs worker`'s epoll wake and `read()` of the queue
eventfd (`deps/libkrun/src/devices/src/virtio/fs/worker.rs:418-452`, `:489-491`), and
the used-ring stores written straight into guest memory
(`deps/libkrun/src/devices/src/virtio/queue.rs:544-580`).

So the question's "one MMIO kick + one used-ring IRQ" is right in spirit and
**low by one**: a completion costs **three** exits, of which the two taken all
the way out to the VMM for the ISR read/ack are the largest term. Adding the wake
and the injection, the whole transition budget of a round trip is **~8-18 us**
(the `1.81 us` MMIO handler and `5.81 us` irqfd figures are third-party bare-metal
measurements - see the sources table - and *this* host is itself a KVM guest, so
treat them as a floor). **That is 2-5% of the ~400 us round trip.**

Three consequences worth naming:

- **The only transition item that is both large and removable is the ISR pair**,
  and it is removable without touching the guest: a transport that never reads an
  ISR register (virtio-pci + MSI-X, or an in-kernel ISR) deletes both exits. It is
  a fork-side transport change worth ~3-5 us per request - the last row of the §2 table,
  where the ratio is bad.
- **EVENT_IDX suppresses none of it at depth 1.** `virtqueue_kick_prepare_split()`
  gates on `vring_need_event(avail_event, new, old)`
  (`drivers/virtio/virtio_ring.c:843-870`,
  `include/uapi/linux/virtio_ring.h:219-227`), which for the one element a
  synchronous caller adds evaluates true; the device's `needs_notification()`
  agrees (`.../virtio/queue.rs:713-747`). Coalescing needs two descriptors inside
  one host disable/enable window, which a blocking caller never produces.
- **`kvm.halt_poll_ns=200000`** (note 20 §5) makes the wakeup cheap in latency and
  expensive in host CPU: a vCPU that halts waiting for a reply spins in
  `kvm_vcpu_halt` for up to 200 us, which is why the host's vCPU ticks exceed the
  guest's own busy time.

### 1.4 Why the 411 us cannot be closed - and what to use instead

Three independent reasons the guest-CPU column cannot be turned into a budget:

1. **Resolution.** `CONFIG_HZ=250` (`config-libkrunfw_x86_64-kvm-lto:434`),
   `CONFIG_TICK_CPU_ACCOUNTING=y` (`:140`), and `VIRT_CPU_ACCOUNTING_GEN` is
   **off** (`:141`). Accounting is therefore tick-sampled in 4 ms quanta. The
   probe reads whole-guest `/proc/stat` (`concurrency-scaling.sh:307-330`) over
   a 26-vCPU guest, so one jiffy of mis-attribution on each vCPU is **104 ms of a
   2 s cell** - +/-68 us per op at 1536 ops. The 411 us number carries a noise
   floor of that order.
2. **Steal is inside `busy`.** The probe computes
   `busy = total - (idle + iowait)` with `total = sum(v[0..7])`
   (`concurrency-scaling.sh:318-323`), and field 8 of `/proc/stat` is `steal`.
   `CONFIG_PARAVIRT_TIME_ACCOUNTING=y` (`config-...:351`), so the guest does
   report it - and this measurement ran 26 vCPUs on a 28-CPU host *with a
   co-tenant session* (note 20 §2: "another session was running a
   `mon-threads.sh` monitor during run B; load average 4.1"). Some of "guest CPU"
   is the host not scheduling the vCPU.
3. **The residual has no addends left.** §1.2 accounts for 10-30 us of execution,
   §1.3 for 10-20 us of transitions, and the worker's 43-64 us is on the other
   side of the machine. Nothing in `fs/fuse` or `drivers/virtio` fills the
   remaining ~120-180 us per request.

Conclusion for the map: **treat note 20's guest-CPU column as an upper bound with
a +/-70 us floor and an unknown steal component, and do the ranking on counts**
(requests, workqueue hops, switches) - which is what §2 does.

## 2 The levers the guest side actually has

Ranked by *requests removed per op* over *work required*. "Ceiling" is what the
lever can do for a rustc build, stated as the class it can touch, not as a
fabricated total - note 20 §5 records that the opcode mix of a build is
unmeasured, which is §4's first task.

| # | lever | requests removed | what it needs | ceiling for a rustc build | verdict |
|---|---|---|---|---|---|
| 1 | **zero-message open** `FUSE_NO_OPEN_SUPPORT` + `FUSE_NO_OPENDIR_SUPPORT` | **2 of 2** per file open+close; **2 of 2** per directory open | server replies `-ENOSYS` to the first `FUSE_OPEN`/`FUSE_OPENDIR`; server opens the host file lazily at the first `FUSE_READ`/`WRITE` and keys it by inode (fh arrives as 0) | the OPEN+RELEASE share of the build's requests; eliminates the entire `open` row (934 us -> the `statwarm` scale) | **do this** |
| 2 | **INIT-reply flags never acked**: `FUSE_MAX_PAGES` (32 -> 256 pages), `FUSE_ASYNC_READ`, `FUSE_CACHE_SYMLINKS` | 0 directly; up to **8x fewer** `FUSE_READ`s for sequential/readahead reads, and pipelined instead of blocking | 1 line each in `passthrough.rs:899-910`; max_pages must stay <= the server's 1 MiB buffer | the read/readahead class only; the probe's 4 KiB `read` mode would not move | **do this** |
| 3 | attribute/entry cache extension (5 s now), `FUSE_EXPLICIT_INVAL_DATA` | 0 (already cached) | nothing | none - `statwarm` at 3.7 us/op is what "cached" costs | skip |
| 4 | `FUSE_PASSTHROUGH` (inode passthrough) | would remove **all** data round trips | a `/dev/fuse` file **bound to the virtio-fs connection** - impossible (§3, note 24) | 0 here | **impossible** |
| 5 | `FUSE_IO_URING` / fuse-uring (`CONFIG_FUSE_IO_URING=y`, `config-...:2894`) | would remove the server's read/write on the fuse device | the fuse-device owner and `IORING_OP_URING_CMD`; a virtqueue server has neither (§3) | 0 here | **impossible** |
| 6 | per-inode DAX (`FUSE_HAS_INODE_DAX`) | removes `FUSE_READ`/`WRITE` for mapped ranges | a DAX window + `FUSE_SETUPMAPPING` per 2 MiB region | closed by note 05 (whole-root connection, no fallback when the window fills) | **already rejected** |
| 7 | kick coalescing / `VIRTIO_F_EVENT_IDX` | 0 | already negotiated (`fs/device.rs:77`), already drained with the spec's disable/re-enable loop (`fs/fuse/virtio_fs.c:841-853`, virtio 1.2 §2.7.14) | a one-deep synchronous caller cannot batch (virtio 1.2 §2.7.13 steps 1-2 "MAY be performed repeatedly if batching is possible") | exhausted |
| 8 | multiple request queues | 0 | `num_request_queues > 1` in the config space (`fs/device.rs:82`) + the guest's `mq_map` (`fs/fuse/virtio_fs.c:892-916`) | virtio 1.2 §5.11.4 says multiple queues are "intended to increase performance", but a width-1 server makes it a no-op - it is the pool's prerequisite, not a lever | deprioritise |
| 9 | a transport with no ISR register (virtio-pci + MSI-X, or an in-kernel ISR) | 0 requests; deletes **2 of the 3** exits per reply | a second transport in the fork (`MmioTransport` -> PCI/MSI-X) and its cmdline/bus plumbing | ~3-5 us per request, ~1% of the round trip | bad ratio - only if the transport is being changed anyway |

### 2.1 Rank 1 - zero-message open

The kernel offers the capability in `FUSE_INIT` (`fs/fuse/inode.c:1457,1460`:
`FUSE_NO_OPEN_SUPPORT | ... | FUSE_NO_OPENDIR_SUPPORT`) and the **server accepts
it by answering `-ENOSYS`**, not by a flag in its reply:

```c
139   bool open = isdir ? !fc->no_opendir : !fc->no_open;
...
166   } else if (err != -ENOSYS) { ... }
169   } else {
170       if (isdir) { kfree(ff->args); ff->args = NULL; fc->no_opendir = 1; }
175       else       { fc->no_open = 1; }
```
(`fs/fuse/file.c:139`, `:166-178`)

After that first failure the guest sends nothing: `fuse_file_open()` skips the
request entirely (`open == false`), and on close `fuse_file_put()` completes the
`FUSE_RELEASE` locally without a request:

```c
110   if (!args) {                       /* no opendir: nothing to do at all */
112   } else if (args->opcode == FUSE_RELEASE && ff->fm->fc->no_open) {
113       fuse_release_end(args, 0);
```
(`fs/fuse/file.c:110-113`)

So for a regular file the class goes **2 requests -> 0**, and for a directory
**2 -> 0** (the `!args` branch), with the only cost being the one `-ENOSYS` at
first open. Bookkeeping: `ff->fh` stays 0 (`fs/fuse/file.c:154-156`), so
`FUSE_READ`/`FUSE_WRITE` arrive with `fh == 0` (`fs/fuse/file.c:184-187`) and the
server must resolve the file from `nodeid` alone.

**Server work in the fork** is small because `LOOKUP` already keeps an `O_PATH`
fd per inode (`linux/passthrough.rs:939-963`) and `OPEN` today is just a re-open
of it through `/proc/self/fd` (`open_inode()`, `:487-535`). The change is:
return `-ENOSYS` from `open()`/`opendir()`, and in `read()`/`write()` do that same
`open_inode()` when the handle is absent, caching the result per inode; drop it
on `FORGET`/inode eviction. Two risks to write down before writing code: (a) the
fd lifetime now follows the *inode*, not the open - so the cache needs a bound
and an LRU, and cang already has an EMFILE sensitivity; (b) `writeback` mode
sends reads on write-only opens (`passthrough.rs:499-506`), so the lazy open must
re-derive flags the way `open_inode()` does.

**Correctness constraints, from the kernel's side of the same code:**

- **`O_TRUNC` is not free and must not be.** Truncate-on-open only rides the `OPEN`
  request when `fc->atomic_o_trunc` is negotiated (`fs/fuse/file.c:257-258`), and
  the fork does not ack `FUSE_ATOMIC_O_TRUNC` (`linux/passthrough.rs:899-910`); the
  truncation is carried out after the open either way
  (`fs/fuse/file.c:287-295`). With zero-message open an `O_TRUNC` open therefore
  costs **one** `FUSE_SETATTR` instead of the `OPEN`+`SETATTR` pair - a win, not a
  hole, but the server's lazy open must not try to reproduce `O_TRUNC` itself.
- **`FUSE_FLUSH` is still sent** on last close (`fs/fuse/file.c:491-506`), so
  `flush`/`fsync` semantics survive the missing `RELEASE`; the fork's `flush` and
  `fsync` (`linux/passthrough.rs:1631`, `:1664`) keep working.
- **Coherence is unaffected**: `mmap` and the page cache read through
  `FUSE_READ` (`fs/fuse/file.c:1850-1855`), which still goes to the server.
- **The fd lifetime moves to the inode**, which is the one real new risk (see §5).

**Ceiling**, honestly: it cannot touch `FUSE_READ`, `GETATTR`, `READDIRPLUS` or
the write path. Its whole value is the OPEN+RELEASE class. In the probe that
class is the `open` row, 934 us at one thread, of which the *server* is 128 us -
so the lever's ceiling there is ~86% of the row. For a build it is the share of
requests that are OPEN/RELEASE, which is unmeasured (§4).

This lever already has a ticket in the map (`tickets/19-zero-message-open.md`,
charted from the same reasoning); this note is its evidence base and corrects two
things in it. (i) Its transition list is short: a round trip is **three** exits,
not two, and the two it misses are the userspace `INTERRUPT_STATUS`/`ACK` pair
that no ioeventfd absorbs (§1.3) - which is also why removing a request removes
*three* exits, not one. (ii) Its correctness question about `O_TRUNC` is answered
below, and the answer is that truncation still costs exactly one `SETATTR`.

### 2.2 Rank 2 - the INIT flags the fork never acks

The fork's `init()` returns only `DO_READDIRPLUS | READDIRPLUS_AUTO`
(+`WRITEBACK_CACHE`/`SUBMOUNTS` when enabled) and ignores everything else it is
capable of (`linux/passthrough.rs:899-910`). Three of the ignored bits are pure
wins:

- **`FUSE_MAX_PAGES`** - the guest offers it (`fs/fuse/inode.c:1459`) and
  `fc->max_pages` is only raised when the *reply* carries it
  (`fs/fuse/inode.c:1341-1344`), otherwise it stays at
  `FUSE_DEFAULT_MAX_PAGES_PER_REQ 32` (`fs/fuse/fuse_i.h:35`) = **128 KiB**. That
  caps one `FUSE_READ`/`WRITE` at 32 pages and it also caps the directory reply
  import buffer: `bufsize = clamp(ctx->count, PAGE_SIZE, fc->max_pages << PAGE_SHIFT)`
  (`fs/fuse/readdir.c:383`). Acking 256 pages matches the server's 1 MiB
  `MAX_BUFFER_SIZE` (`deps/libkrun/.../fs/server.rs:35`) and cuts a sequential
  read's request count by up to 8x.
- **`FUSE_ASYNC_READ`** - `fc->async_read` is set only from `FUSE_INIT`
  (`fs/fuse/fuse_i.h:496-497`, "Only set in INIT"), and it is what makes
  readahead asynchronous: `fuse_send_readpages(..., fc->async_read)`
  (`fs/fuse/file.c:926-927`, `:1109-1110`), with the batch size
  `min(fc->max_pages, readahead_count(rac))` (`:941`). Without the bit, readahead
  replies block the filler instead of pipelining.
- **`FUSE_CACHE_SYMLINKS`** - `fc->cache_symlinks` is only set from the reply
  (`fs/fuse/inode.c:1337-1338`); costs one line, removes a `READLINK` round trip
  per repeated symlink path. rustc's world has few of these: near-zero, keep it
  for completeness only.

Work: three lines. Ceiling: the read/readahead class only, and *not* on the
probe as written - its `read` mode is a single 4 KiB read after
`fadvise(DONTNEED)` (note 20 §1), which no batch size helps. It shows up in a
build arm reading `.rlib`s and large sources sequentially.

### 2.3 What is left on the readdirplus class

Not a guest-side lever, and the sibling pool ticket owns it. The guest's numbers
here are already reasonable - guest CPU 2.2 ms of a 14.9 ms scan - and the 10.5 ms
is server CPU for `openat(O_PATH)+statx` per entry (`linux/passthrough.rs:1104-1127`,
note 19 §2.1). The only guest-side knob is the same `max_pages` bit above, worth
one round trip per 128 KiB of directory entries.

## 3 What is impossible here

### 3.1 FUSE_PASSTHROUGH: the mechanism needs a `/dev/fuse` file bound to the virtio-fs connection

> **Correction (2026-10-26, note 24).** This heading used to claim the guest has no
> `/dev/fuse`. It does: `CONFIG_FUSE_FS=y` + `CONFIG_DEVTMPFS_MOUNT=y` in the pinned
> kernel give the guest a `fuse` misc device node. What it has no instance of is a
> `/dev/fuse` **file whose connection is the virtio-fs one** - the driver allocates
> its `fuse_dev`s and installs them straight on the virtqueues
> (`fs/fuse/virtio_fs.c:1588-1612`), so no userspace file ever refers to that
> connection. Conclusion unchanged; premise corrected.

Passthrough is the only mechanism that makes data I/O cost **zero** FUSE round
trips, and it does work as advertised where it applies: the server registers a
backing file with

```c
/* dev.c:2298 */
static long fuse_dev_ioctl_backing_open(struct file *file, struct fuse_backing_map __user *argp)
{
	struct fuse_dev *fud = fuse_get_dev(file);
	...
	return fuse_backing_open(fud->chan->conn, &map);
```
(`fs/fuse/dev.c:2298-2314`, dispatched from `.unlocked_ioctl = fuse_dev_ioctl` at
`dev.c:2388`, `FUSE_DEV_IOC_BACKING_OPEN` at `:2353`), the kernel turns the fd into
an id (`fs/fuse/backing.c:83-130`: needs `fc->passthrough` and
`capable(CAP_SYS_ADMIN)` at `:94`, `fget_raw(map->fd)` at `:101`, regular files
only at `:107-109`, `s_stack_depth` at `:113`, id allocated at `:125`), and the
server returns that id in the open reply - `struct fuse_open_out { u64 fh; u32
open_flags; int32_t backing_id; }` (`include/uapi/linux/fuse.h:807-811`), consumed
at `fs/fuse/iomode.c:180`. From then on the file's `read`/`write`/`splice`/`mmap`
go straight to the backing file: `fuse_file_read_iter`'s dispatch
(`fs/fuse/file.c:1849-1855`) picks `fuse_passthrough_read_iter()`
(`fs/fuse/passthrough.c:28-51`), which is `backing_file_read_iter()` ->
`vfs_iter_read()` (`fs/backing-file.c:173`; writes at `:227`). Nothing is written
to the fuse device.

Three facts make it unusable for cang, and the first is fatal:

1. **The ioctl entry point is the fuse char device's `file`.** `fuse_get_dev(file)`
   (`fs/fuse/dev.c:2301`, `:1662-1678`) resolves the connection from the
   `file->private_data` that `/dev/fuse`'s `open` created (`dev.c:1651-1660`). The
   virtio-fs client does allocate `fuse_dev`s - but installs them on virtqueues,
   never on a user-visible file: `fsvq->fud = fuse_dev_alloc()` then
   `fuse_dev_install(fsvq->fud, fc->chan)` (`fs/fuse/virtio_fs.c:1588-1612`). No
   file, no `unlocked_ioctl`, no way for the *host* server to reach the guest's
   `fuse_conn`. Even a hypothetical fd-passing side channel would fail on
   `fget_raw(map->fd)` + `vfs_iter_read()` running in the **guest's** kernel while
   the file belongs to the host's.
2. **It is mutually exclusive with the rank-1 lever anyway.** `fuse_file_io_open()`
   returns early when there is no open reply to read the id from:
   `if (FUSE_IS_DAX(inode) || !ff->args) return 0;` (`fs/fuse/iomode.c:206`) - and
   `ff->args` is exactly what zero-message open frees (`fs/fuse/file.c:170-173`).
   Passthrough also cannot coexist with `FUSE_WRITEBACK_CACHE` at INIT
   (`fs/fuse/inode.c:1387-1395`), nor with DAX (`iomode.c:206`), and once an inode
   is passthrough every later open must answer `FOPEN_PASSTHROUGH` or `-EINVAL`
   (`iomode.c:209-215`).
3. **Nobody has built it for virtio-fs**, so "cang would have to build it" is not
   a work estimate but a research project: passthrough landed in **v6.9** (Amir
   Goldstein, `fuse: introduce FUSE_PASSTHROUGH capability`) and **virtiofsd still
   does not implement it** - its `OpenOut` carries `padding` where the kernel has
   `backing_id`, and its `InitOut` has no `max_stack_depth`, which the kernel
   requires to be `> 0` (`fs/fuse/inode.c:1389`).

**The virtio-fs analogue of passthrough is the DAX window**, not passthrough
itself, and that is already rejected on its own merits: note 05 (the connection
is the whole guest root, so `dax=always` is all-or-nothing and every 2 MiB region
of every mapped binary costs a `FUSE_SETUPMAPPING` on the one worker) and note 18
(which corrects the stated reason - the host filesystem is not the blocker; the
per-connection scope is).

### 3.2 fuse-uring: the same argument, one layer further in

`CONFIG_FUSE_IO_URING=y` is in the pinned kernel (`config-libkrunfw_x86_64-kvm-lto:2894`)
and the code is `fs/fuse/dev_uring.c`. It replaces the server's `read()`/`write()`
on the fuse device with `IORING_OP_URING_CMD` SQEs, submitted against the
`/dev/fuse` file the server owns: `fuse_uring_cmd()` resolves the connection from
`cmd->file` (`fs/fuse/dev_uring.c:1233`, `fuse_get_dev(cmd->file)`), and the hook
is on that char device's fops (`fs/fuse/dev.c:2391`, `.uring_cmd = fuse_uring_cmd`).
It is also runtime-gated (`fuse.enable_uring`, advertised as
`FUSE_OVER_IO_URING (1ULL << 41)`). A virtio-fs server has no such file (§3.1), so
**fuse-uring is unavailable to cang for exactly the same reason** - and it would
not help the data path anyway: with passthrough the reads are plain
`vfs_iter_read`s that need no uring at all, and without it the ring only changes
how *metadata* requests reach a server that, in cang, is not a fuse-device client.

### 3.3 What the guest side genuinely cannot get rid of

- **A reply must arrive as an interrupt** and the blocked task must be woken, so
  every round trip carries at least one guest IRQ entry and two task switches,
  plus (here) two userspace ISR exits (§1.3).
- **A synchronous caller cannot batch.** virtio 1.2 §2.7.13 says steps 1-2 "MAY be
  performed repeatedly if batching is possible"; a `read()` that blocks is not.
  Same for the device side's `needs_notification()` (`queue.rs:713-747`).
- **The 4-descriptor minimum per request** ("requests need at least 4 elements",
  `fs/fuse/virtio_fs.c:1398`) with no `VIRTIO_RING_F_INDIRECT_DESC`
  (`.../virtio/fs/device.rs:77`) is a ring-capacity limit, not a latency one.

## 4 The one thing to do next, and how to know it worked

Ranked by (guest-side latency removed) / (work):

**(0) Free, do it first: price the class.** Note 20 §5 records that the opcode mix
of a real build is unmeasured, and it cannot be measured from inside the guest:
`# CONFIG_FTRACE is not set` (`config-libkrunfw_x86_64-kvm-lto:3630`) and
`# CONFIG_DEBUG_FS is not set` (`:3474`), so the fuse tracepoints in
`fs/fuse/fuse_trace.h` are not available and there is no tracefs. Count
server-side instead: a `[u64; N]` histogram beside the opcode dispatch
(`deps/libkrun/src/devices/src/virtio/fs/server.rs:117`,
`x if x == Opcode::Open as u32 => self.open(...)`), printed on shutdown, and one
guest-virtiofs build arm (`tools/rust-compile-bench/run.sh`). That yields the five
shares that decide everything below (OPEN, RELEASE, READ/readahead, GETATTR,
READDIRPLUS) for the cost of one arm and no behaviour change.

**(1) Zero-message open** - `FUSE_NO_OPEN_SUPPORT` + `FUSE_NO_OPENDIR_SUPPORT`
(§2.1). ~30 lines: return `-ENOSYS` from `open()`/`opendir()` in
`PassthroughFs`, and in `read()`/`write()` call the existing `open_inode()`
(`linux/passthrough.rs:487-535`) when the handle is absent, caching per inode with
a bound, releasing on `FORGET`. **Confirmation, in one boot, with the tool that
already exists:** `tools/rust-compile-bench/concurrency-scaling.sh` `open` mode at
1/4/16/64 threads with alternating rounds against a server built with and without
it. Expect the `open` row to leave the 934 us band and land near the `statwarm`
band (single-digit us), the worker's share for that mode to fall to ~0, and
`readdirplus` scans to lose their `FUSE_OPENDIR`/`FUSE_RELEASEDIR` pair. If the
row does not move, the ENOSYS is not being seen (check that the guest's first
`FUSE_OPEN` really returns `-ENOSYS`, not `-ENOENT`-wrapped).

**(2) The three INIT flags** - `FUSE_MAX_PAGES` (256 pages),
`FUSE_ASYNC_READ`, `FUSE_CACHE_SYMLINKS` (§2.2). Three lines in
`linux/passthrough.rs:899-910`; needs `max_pages <= 256` to stay inside the
server's 1 MiB `MAX_BUFFER_SIZE` (`fs/server.rs:35`). **Confirmation:** a probe
mode that reads a multi-MiB file sequentially (the existing `read` mode's single
4 KiB read after `fadvise(DONTNEED)` cannot show this) - expect the request count
per file to drop by up to 8x and the mode's wall time to fall roughly in
proportion to its request count.

Both are strictly additive and neither touches the pool, the transport, or the
kernel. What *not* to do, in one line each: do not prototype passthrough or
fuse-uring (§3.1, §3.2 - no fuse device); do not re-open DAX (note 05); do not
patch the guest kernel for the per-request argbuf `kmalloc` or the workqueue hop
in `virtio_fs_vq_done()` (1-3 us each, `fs/fuse/virtio_fs.c:696-716`, `:920-927`);
do not expect the pool to move the open/read/stat classes (note 20's ceiling:
14% at one thread, 0.4% at 64).

## 5 What this note does not settle

- **The opcode mix** (§4 step 0). Every ceiling in §2 is expressed as a class, not
  as seconds, because the class shares are unmeasured.
- **Whether the server's fd-per-inode cache survives a real build.** Zero-message
  open moves the fd lifetime from `open`/`close` to inode eviction, and cang has a
  documented EMFILE sensitivity; a build arm with the lazy cache in place is the
  test, and an LRU bound is the mitigation.
- **Note 15 and note 20 disagree by an order of magnitude, and the disagreement
  is about `N`.** Note 15 infers `Delta ~ 5-16 us` per FUSE op by assuming
  `N ~ 5-13 M` requests per build (`notes/15-why-one-queue-and-one-thread.md:571-591`);
  note 20 *measures* one round trip at ~360-470 us (this note's §1.1). Both cannot
  hold: at note 20's 16-64-thread throughput (2656-4127 ops/s) a 305 s arm
  completes at most ~1.2 M requests, so either `N` is ~1 M (and `Delta` is
  ~56-66 us of *amortised* penalty, consistent with ~400 us at 26-64 in flight) or
  most requests are cache hits at 3.7 us. Either way the build-level ceilings in §2
  stay expressed as class shares, not seconds - and `N` is the first thing §4's
  counter would fix.
- **The ISR-exit term.** §1.3 prices the two userspace exits per reply at ~3-5 us
  and notes that a transport without an ISR register would delete them; nobody has
  prototyped a virtio-pci/MSI-X transport for the fork, so that number is
  arithmetic, not measurement.

## Sources

| what | where |
|---|---|
| the build-level `Delta ~ 5-16 us` estimate this note challenges | `notes/15-why-one-queue-and-one-thread.md:571-591` (an inference from the 67-79 s penalty and an assumed `N`, not a measurement) |
| note 20's numbers this note normalises | `notes/20-concurrency-scaling-probe.md` §2 (latency budget table, 1 thread) |
| guest kernel, tag `v7.2` | `fs/fuse/{file,dir,dev,req,readdir,inode,passthrough,iomode,backing,virtio_fs,fuse_i.h,fuse_trace.h}`, `fs/backing-file.c`, `drivers/virtio/{virtio_mmio,virtio_ring}.c`, `include/uapi/linux/{fuse,virtio_ring}.h` - all via `https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/<path>?h=v7.2`, quoted from `grep -n` line numbers |
| pinned guest kernel config | `deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto` (`HZ=250` `:434`, `TICK_CPU_ACCOUNTING` `:140`, no `VIRT_CPU_ACCOUNTING_GEN` `:141`, `PARAVIRT_TIME_ACCOUNTING` `:351`, `CONFIG_FUSE_PASSTHROUGH` `:2893`, `CONFIG_FUSE_IO_URING` `:2894`, no `FTRACE` `:3630`, no `DEBUG_FS` `:3474`) |
| fork device model, read at `7160f48f` | `src/devices/src/virtio/fs/{device,server,worker,fuse}.rs`, `fs/linux/passthrough.rs`, `src/devices/src/virtio/{queue,mmio}.rs`, `src/libkrun/src/vmm/device_manager/kvm/mmio.rs`, `src/libkrun/src/vmm/linux/vstate.rs` |
| virtio 1.2 | §2.7.13 "Supplying Buffers to The Device", §2.7.14 "Receiving Used Buffers From The Device", §5.11 "File System Device" (§5.11.2 virtqueues, §5.11.4 `num_request_queues` "Use of multiple queues is intended to increase performance", §5.11.6.1 "The device processes requests in any order") - `docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html` |
| KVM exit / irqfd cost provenance | third-party measurement, microkvm `docs/en/benchmark.md`: userspace MMIO exit handler **1.81 us** avg (0.80-11.58, n=4011), eventfd thread 3.45-3.47 us, **irqfd 5.81 us** avg (3.94-7.08, n=11); kernel-side cost class: `Documentation/virt/kvm/halt-polling.rst:14-21` ("normally on the order of a few micro-seconds"), `virt/kvm/kvm_main.c:3715-3720`; the "1-2 us exit" and "0.2-1 us used-ring stores" figures in §1.3's budget are cost-class assumptions, not measurements on this host |
| `halt_poll_ns` on this host | `/sys/module/kvm/parameters/halt_poll_ns` = `200000`; effect recorded in note 20 §5 |
| guest vCPU count on this host | `crates/cang/src/runtime/launch/config/components/resources.rs:8-18` (`available_parallelism - 2`), i.e. 26 vCPUs on 28 host CPUs per note 20 §1 |
| probe harness and its counters | `tools/rust-compile-bench/concurrency-scaling.sh:303-331` (`busy = sum(user..steal) - idle - iowait`, `intr`) |
| where passthrough/DAX were settled | notes 05 (DAX window), 14, 18 (per-inode DAX, and the correction to note 14's reason) |

## Correction (2026-10-26, parent, from the fork's own source)

**Two of the three "INIT-reply flags never acked" are in fact acked.** In
`deps/libkrun/src/devices/src/virtio/fs/server.rs:898-906` the server's `supported`
set already contains `FsOptions::ASYNC_READ` **and** `FsOptions::MAX_PAGES`, and
`:933-949` replies `max_pages = (MAX_BUFFER_SIZE - 1)/page_size + 1` = **256** with
`max_write = MAX_BUFFER_SIZE` (1 MiB). So the guest is already offered 256 pages /
1 MiB reads with readahead pipelined - there is no 32 -> 256 win to collect.

The only one of the three genuinely missing is **`FUSE_CACHE_SYMLINKS`** (absent
from the `supported` set; `fuse.rs:145` defines the bit and `:371` exposes it, it is
simply never OR'd in). That is a one-line change worth keeping in the ranking, but
on its own it is a symlink-heavy-build win only, not a read-path win.

The rest of the note stands, including the parts this correction does not touch:
the three-exits-per-round-trip accounting (which fixes ticket 19's count), the
`O_TRUNC` caveat, the passthrough/io_uring impossibility, and the ranking's item 1
(zero-message open), which remains the only lever with a measured numerator.
