# 48 - The guest kernel's completion path and request concurrency: what a libkrunfw patch could buy

**Written 2026-11-04**, against `main` `d1b8fbf`-era tree, fork `deps/libkrun`
tag `v2.0.0-cang.17`, firmware `deps/libkrunfw` (kernel `linux-7.2.10` +
`v7.2.10-hardened1`). No VM was booted and no build was run for this note: it
is a source read plus arithmetic on the numbers already measured by notes
[44](44-queue-depth-not-thread-count.md), [45](45-compile-iteration-lookup-diagnosis.md),
[46](46-request-cost-split-and-recommendation.md) and
[22](22-guest-side-round-trip-reduction.md).

**The question.** Note 46 §3 ranks a libkrunfw patch - "run
`virtio_fs_requests_done` inline instead of via the workqueue" - as candidate
(b), and lists "raising `max_background`" as (c)
(`docs/wayfinder/workspace-virtiofs-performance/notes/46-request-cost-split-and-recommendation.md:61-66`).
Is either worth a kernel build, and what does the completion path actually cost?

**Answer in one line.** (b) is worth **~0.1 % of the guest's 48 % share at the
measured batch (~40 replies per interrupt) and at most 1.3-2.6 % of it in the
pessimistic one-reply-per-interrupt shape** - 0.6-1.3 % of the 8.2 s iteration -
because the kernel already coalesces the hop behind the interrupt's reply batch
and already completes non-blocking requests inline *in the drain loop*; (c) is
**dead** - libkrun's FUSE INIT reply already
sets `max_background` to `u16::MAX` and the kernel adopts it, and in any case
that gate never touches a synchronous request. Do not ship a kernel patch.

## 0. Which source I read

The kernel is not vendored: `deps/libkrunfw/Makefile:1` names the base and the
Makefile downloads it at build time (`:2`, `:151-155`).

```
KERNEL_VERSION = linux-7.2.10
KERNEL_HARDENED_VERSION = v7.2.10-hardened1
...
for patch in $(KERNEL_PATCHES); do patch -p1 -d $(KERNEL_SOURCES) < "$$patch"; done
patch -p1 -d $(KERNEL_SOURCES) < $(KERNEL_HARDENED_PATCH)
cp config-libkrunfw$(VARIANT)_$(GUESTARCH) $(KERNEL_SOURCES)/.config
```

So everything below was read from the **stable tree at `v7.2.10`**, one file per
URL of the form
`https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/plain/<path>?h=v7.2.10`
(no `7.2.10` tarball was fetched): `fs/fuse/virtio_fs.c`, `fs/fuse/dev.c`,
`fs/fuse/inode.c`, `fs/fuse/file.c`, `fs/fuse/fuse_i.h`, `fs/fuse/dev.h`,
`fs/fuse/args.h`, `include/uapi/linux/fuse.h`, `include/linux/workqueue.h`,
`drivers/virtio/{virtio_mmio.c,virtio_ring.c,virtio_pci_common.c}`. The version
also matters because it has *moved on* from what the notes assume - see §1.1.

Two provenance points. The pinned base is `7.2.10`, while notes 22, 47 and 49
quote the torvalds tag `v7.2`; the `fs/fuse/virtio_fs.c` line numbers for the
functions this note turns on (`:920-927` vq callback, `:828-879` drain,
`:841-853` the disable/get_buf/enable loop, `:861-868` the `may_block` work
item) are identical in both trees (checked: both files are 1856 lines and the same
line numbers carry the same code), so their citations and these interlock.
And the transport/exit side of the round trip is notes 47 and 49's subject - note
47 finds the fork's virtio-pci is INTx-only with no MSI-X and no notify
ioeventfd, so the "delete the ISR exits" lever is not sitting there either. This
note only claims the completion-path and concurrency side.

## 1. What happens to a completed request

### 1.1 Two of the names in the ticket do not exist in this kernel

`virtio_fs_requests_done` and `virtio_fs_request_ready` are **not** symbols in
`fs/fuse/virtio_fs.c` at v7.2.10 (grep: zero hits). The request-queue path is:

| role | symbol | line |
|---|---|---|
| vq callback (IRQ context) | `virtio_fs_vq_done` | `fs/fuse/virtio_fs.c:920-927` |
| drain + complete | `virtio_fs_requests_done_work` | `:828-879` |
| per-reply completion | `virtio_fs_request_complete` | `:784-817` |
| per-reply work wrapper (`may_block` only) | `virtio_fs_complete_req_work` | `:819-826` |

Two things follow immediately. First, **non-blocking replies are already
completed inline** - inside the drain loop, in one pass, with no per-reply work
item: `:856-872` ends each request from the loop unless `req->args->may_block`.
Second, the "run it inline instead of via the workqueue" patch is therefore
*not* about the per-reply hop for cargo's sync traffic; it is about the single
`schedule_work` the callback takes per **interrupt**, §1.3.

### 1.2 The trace

```c
/* Virtqueue interrupt handler */                       /* :919-927 */
static void virtio_fs_vq_done(struct virtqueue *vq)
{
	struct virtio_fs_vq *fsvq = vq_to_fsvq(vq);
	dev_dbg(&vq->vdev->dev, "%s %s\n", __func__, fsvq->name);
	schedule_work(&fsvq->done_work);
}
```

`schedule_work` is `queue_work(system_percpu_wq, work)` - a **per-CPU bound**
workqueue, so the kworker that drains runs on the CPU that took the interrupt,
and `queue_work` returns `false` (no wakeup) when the item is already queued
(`include/linux/workqueue.h:744-761`) - the second coalescing point.

The callback itself runs in **hard IRQ** context: the transport is virtio-mmio
(note 22 §1.3), whose handler is registered non-threaded
(`request_irq(irq, vm_interrupt, IRQF_SHARED, ...)`, `drivers/virtio/virtio_mmio.c:467`)
and, per device, reads and acks the ISR and then walks **every** vq:

```c
	status = readl(vm_dev->base + VIRTIO_MMIO_INTERRUPT_STATUS);   /* :296 */
	writel(status, vm_dev->base + VIRTIO_MMIO_INTERRUPT_ACK);      /* :297 */
	...
	if (likely(status & VIRTIO_MMIO_INT_VRING)) {
		virtio_device_for_each_vq(&vm_dev->vdev, vq)
			ret |= vring_interrupt(irq, vq);               /* :305-306 */
	}
```

`vring_interrupt` then calls `vq->vq.callback(&vq->vq)`
(`drivers/virtio/virtio_ring.c:3325-3327`), i.e. `virtio_fs_vq_done`. Both fs
vqs (hiprio + requests) live on that one IRQ line, so every completion interrupt
touches the hiprio vq too (harmless: `more_used()` fails, `:3306-3309`).

Then the drain, per interrupt:

```c
	spin_lock(&fsvq->lock);
	do {
		virtqueue_disable_cb(vq);                       /* :842 */
		while ((req = virtqueue_get_buf(vq, &len)) != NULL) {
			if (!virtio_fs_verify_response(req, len)) { ... }
			spin_lock(&fpq->lock);
			list_move_tail(&req->list, &reqs);      /* :849-851 */
			spin_unlock(&fpq->lock);
		}
	} while (!virtqueue_enable_cb(vq));                      /* :853 */
	spin_unlock(&fsvq->lock);

	/* End requests */
	list_for_each_entry_safe(req, next, &reqs, list) {
		list_del_init(&req->list);
		if (req->args->may_block) {                      /* :861 */
			w = kzalloc_obj(*w, GFP_NOFS | __GFP_NOFAIL);
			INIT_WORK(&w->done_work, virtio_fs_complete_req_work);
			schedule_work(&w->done_work);            /* :868 */
		} else {
			virtio_fs_request_complete(req, fsvq);   /* :870 inline */
		}
	}
```

`virtio_fs_request_complete` (`:784-817`) copies the reply out of the bounce
buffer, zeroes short-read tails, clears `FR_SENT`, calls `fuse_request_end()`
and drops `fsvq->in_flight`. `fuse_request_end`
(`fs/fuse/dev.c:627-663`) is the wakeup:

```c
	if (test_bit(FR_BACKGROUND, &req->flags)) {
		spin_lock(&fch->bg_lock);
		fuse_request_bg_finish(fch, req);
		flush_bg_queue(fch);
		spin_unlock(&fch->bg_lock);
	} else {
		/* Wake up waiter sleeping in request_wait_answer() */
		wake_up(&req->waitq);                            /* dev.c:653-656 */
	}
```

`virtqueue_disable_cb`/`enable_cb` cost **no exit**: with EVENT_IDX they are
guest-memory writes of `used_event` (`drivers/virtio/virtio_ring.c:1087-1101`,
`:1010`), not MMIO - so the retry loop at `:841-853` is free of host
transitions.

### 1.3 Context switches and wakeups per batch

Per **interrupt** (one used-ring scan):

1. one hardirq entry on a vCPU (and, on the host, the irqfd injection of note 22
   §1.3 row 3);
2. `schedule_work(&done_work)` - one kworker wakeup, coalesced away entirely if
   the work is already queued (`workqueue.h:748-753`);
3. the interrupted task is switched out and the kworker in: **one scheduling
   round trip (2 task switches)**;
4. N replies drained and completed: **0 extra switches**, N `wake_up()`s of the
   N waiting callers (each may IPI another vCPU - unavoidable, and it is the
   caller's own wakeup, not overhead);
5. `if (!list_empty(&fsvq->queued_reqs)) schedule_work(&fsvq->dispatch_work)`
   (`:874-878`) - a second workqueue run only when the ring had been full.

So the workqueue is **1 wakeup + 2 switches per batch**, and per *request* its
cost is that divided by the batch size. Measured batch sizes: **~43 replies per
host->guest notification** in the 26-shard storm
(`docs/wayfinder/workspace-virtiofs-performance/notes/44-queue-depth-not-thread-count.md:96-97`)
and ~11 creates = ~40 requests per interrupt in the second window
(`46-request-cost-split-and-recommendation.md:40`).

**Can it be done inline in the virtio callback?** Mechanically yes - the drain
body only takes spinlocks (`fsvq->lock`, then `fpq->lock`, the same order the
enqueue path takes at `:1461-1472`), does `kmem_cache_free`, writes guest
memory, and `fuse_request_end`'s `FR_BACKGROUND` branch calls `flush_bg_queue`
→ `fuse_send_one` → `virtio_fs_send_req` → `virtqueue_add_sgs(GFP_ATOMIC)` +
kick, all atomic-safe (the code already relies on that: `:1531-1541` defers
only the *error* path to a worker, "Can't end request in submission context").
What is **not** safe is the `may_block` class - the reply finishes user pages,
which is exactly why upstream defers it: "blocking async request completes in a
worker context" (`:860`). Any patch must keep that branch, so the shippable
change is "run `virtio_fs_requests_done_work`'s body in the IRQ instead of
queueing it", i.e. delete one kworker hop per batch - nothing per request.

## 2. `max_background` / `congestion_threshold`: it gates background requests only, and libkrun already maxes it

### 2.1 The gating code

Sync requests never consult it. `fuse_chan_send()` allocates with
`fuse_get_req(fch, false)` and sends immediately
(`fs/fuse/dev.c:809-833`), and `fuse_block_alloc` - the only place the
background flag matters - is:

```c
static bool fuse_block_alloc(struct fuse_chan *fch, bool for_background)  /* dev.c:88 */
{
	if (!smp_load_acquire(&fch->initialized))
		return true;

	return (for_background && fch->blocked) ||                /* :94 */
	       (fch->io_uring && fch->connected && !fuse_uring_ready(fch));
}
```

`fch->blocked` is set only by the background path, and the queue that
`max_background` throttles is only walked from there:

```c
static void flush_bg_queue(struct fuse_chan *fch)                 /* dev.c:581 */
{
	while (fch->active_background < fch->max_background &&
	       !list_empty(&fch->bg_queue)) { ... }
}
```

```c
	spin_lock(&fch->bg_lock);                                  /* dev.c:878-887 */
	if (likely(fch->connected)) {
		fch->num_background++;
		if (fch->num_background == fch->max_background)
			fch->blocked = 1;
		list_add_tail(&req->list, &fch->bg_queue);
		flush_bg_queue(fch);
```

What it *does* gate, therefore, is narrow and enumerable: readahead-only pages
(`fs/fuse/file.c:932-939`, "Congested and only async pages left, so skip the
rest") and background writeback (`fs/fuse/file.c:2321-2323`). Cargo's
LOOKUP/GETATTR/GETXATTR stream (79 % + 5 % + 1 % of requests,
`45-...:30-33`) is `fuse_chan_send`, i.e. `for_background = false`: **raising
the limit cannot move it.**

### 2.2 It is already at its maximum, so there is nothing to raise

Note 45 §6 records "connection credit: unreachable from the guest ... kernel
default 12, `virtio_fs.c` never overrides it"
(`45-compile-iteration-lookup-diagnosis.md:112-113`), and note 46 builds
candidate (c) on the same reading. The first half is right - `virtio_fs.c`
never sets it and the *kernel* default is 12
(`fs/fuse/dev.h:9`, `FUSE_DEFAULT_MAX_BACKGROUND 12`; congestion 9,
`fs/fuse/inode.c:60`). But the value does not come from the kernel default: the
**server sets it in its FUSE INIT reply**, and the kernel adopts it:

```rust
let out = InitOut {                                   /* server.rs:1105 */
    ...
    max_background: u16::MAX,                          /* :1110 */
    congestion_threshold: (u16::MAX / 4) * 3,          /* :1111 */
```
`deps/libkrun/src/devices/src/virtio/fs/server.rs:1105-1118`, with
`KERNEL_MINOR_VERSION: u32 = 27` (`.../fs/fuse.rs:15`) - above the `minor >= 13`
floor:

```c
static void process_init_limits(struct fuse_conn *fc, struct fuse_init_out *arg)  /* inode.c:1238 */
{
	int cap_sys_admin = capable(CAP_SYS_ADMIN);
	if (arg->minor < 13)
		return;
	...
	if (arg->max_background) {
		unsigned int max_background = arg->max_background;
		if (!cap_sys_admin && max_background > max_user_bgreq)
			max_background = max_user_bgreq;
		fuse_chan_max_background_set(fc->chan, max_background);   /* :1254 */
	}
```

The root virtiofs mount is made by the init task
(`rootfstype=virtiofs rw`, `deps/libkrun/src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:5-6`)
and every other path - `/workspace` included - is a graft inside that one root
export (`44-queue-depth-not-thread-count.md:17-19`), so `cap_sys_admin` is 1 for
the only FUSE INIT the guest ever sends, and nothing is clamped: **`max_background` is 65535 and
`congestion_threshold` 49151 in the guest today.** Candidate (c) is not "unlikely
to show anything" - it is already at the ceiling, and it would still not gate a
single synchronous request. Notes 45 and 46 should carry this correction.

## 3. Does the client batch requests, or the driver drain more than one reply?

**Replies: yes, drained in one scan.** The loop at `fs/fuse/virtio_fs.c:844`
pulls every completed chain (`while ((req = virtqueue_get_buf(vq, &len)) != NULL)`)
before re-enabling the callback, and interrupts themselves are already
amortised ~40:1 (§1.3). Nothing to gain.

**Submissions: one `kick_prepare` per request, no client-side batching.** Every
request goes through `virtio_fs_send_req` → `virtio_fs_enqueue_req`, which ends
with:

```c
	notify = virtqueue_kick_prepare(vq);                    /* :1479 */
	spin_unlock(&fsvq->lock);
	if (notify)
		virtqueue_notify(vq);                            /* :1483-1484 */
```

and `virtqueue_notify` is `vm_notify`'s `writel(vq->index, base + VIRTIO_MMIO_QUEUE_NOTIFY)`
(`drivers/virtio/virtio_mmio.c:267-275`). The **transport** is what batches the
exits: `virtqueue_kick_prepare_split` consults the device's `avail_event`

```c
	if (vq->event) {
		needs_kick = vring_need_event(vring_read_split_avail_event(vq),
					      new, old);            /* virtio_ring.c:860-862 */
	}
```
(`vq->event` = EVENT_IDX negotiated, `virtio_ring.c:1376`; advertised and acked
here: `deps/libkrun/src/devices/src/virtio/fs/device.rs:78`), and the host only
re-arms by writing `avail_event = next_avail` when it stops reading
(`deps/libkrun/src/devices/src/virtio/queue.rs:646-666`, `:688-704`), while the
interrupt side is a separate contract: the guest says when to be interrupted by
writing `used_event` on every `get_buf` (`drivers/virtio/virtio_ring.c:1005-1011`,
`:1099-1101`), and the host's `needs_notification` reads exactly that
(`queue.rs:713-747`). Consequence, and it matters for the A/B: as note 22 §1.3
put it, at depth 1 the event-index test "evaluates true" - a lone synchronous
caller always kicks, so the iteration shape (26 concurrent rustc,
`CARGO_BUILD_JOBS=26`, `45-...:7`) batches only to the extent its requests
really arrive in bursts.

## 4. What a one-function patch would cost

**Where the patch goes.** `deps/libkrunfw/patches/0*.patch`, applied in sorted
order onto a pristine tree before the hardened patch
(`deps/libkrunfw/Makefile:8`, `:151-155`); the series is 23 patches today
(`deps/libkrunfw/patches/`), the newest `0023-drm-virtio-gate-...`.

**What it builds.** `nix/pkgs/libkrunfw.nix` has two modes. The release fetches
a prebuilt asset; `useLocalSource = true` compiles the kernel from the
`libkrunfw-src` flake input, and **it compiles the non-LTO config**:

```
cp config-libkrunfw_x86_64-kvm config-libkrunfw_x86_64        # nix/pkgs/libkrunfw.nix (localSource preBuild)
```
with `CONFIG_LTO_NONE=y` (`deps/libkrunfw/config-libkrunfw_x86_64-kvm:750`),
against `CONFIG_LTO_CLANG_THIN=y` in the shipped asset's config
(`config-libkrunfw_x86_64-kvm-lto:750`). That is the good news: **a local
experiment is a plain non-LTO kernel build**, thin-LTO is only the published
artifact (`MakefileLto`, `docs/maintenance.md:274-283`), and the fork's build
already wires `sccache` in front of `cc`/`cxx` for both host and kernel
(`nix/pkgs/libkrunfw.nix`, `localSource.preBuild`: `.nix-sccache-wrappers/cc`,
`makeFlagsArray+=("CC=...")`). No kernel-build wall time is recorded anywhere under
`docs/`, so treat "one kernel build" as the unit of cost rather than a number.

**How to build and boot it.** The documented recipe is
`docs/maintenance.md:293-297`:

```
nix build .#cang-dev --override-input libkrunfw-src path:$PWD/deps/libkrunfw
CANG_LIBKRUNFW_LIBRARY=<checkout>/libkrunfw.so.5
```

(`docs/build.md:92-97` describes `.#cang-dev` the same way.) The A/B path is
real and cheap: cang `dlopen`s the firmware by explicit path when the env var is
set, before libkrun's own soname lookup
(`crates/cang-libkrun/src/firmware.rs:29`, `:39-63`) - so **a kernel
experiment needs no cang rebuild, no image rebuild, no container load**, just a
boot per arm. Note 45 already ran its arms that way
(`45-...:12`).

**The trap.** Because the local build is non-LTO and the shipped asset is
ThinLTO, a local win is not a shipped win: these static functions get inlined
into `vring_interrupt` under LTO, so the exact thing being measured is the thing
LTO changes. A defensible A/B therefore has to be run **both** non-LTO
(local, cheap) **and** LTO (unwrapped clang, per `docs/maintenance.md:278-283`),
or the result does not transfer to a release.

**Shipping it** is the full fork cascade: commit on `deps/libkrunfw` branch
`cang`, permanent tag `v5.6.2-cang.<n>`, the fork's publish workflow builds the
LTO assets, then `scripts/update-libkrunfw.sh --tag ... --system x86_64-linux`
and `nix build .#libkrunfw`, plus the pin in `nix/pins.nix`
(`docs/maintenance.md:343-427`). That is a release-sized act for a few percent.

## 5. Verdict

**Do not ship a libkrunfw patch for the completion path.** The number that
settles it is not the guest's 55 us per request
(`44-...:99-100`), it is *how many requests share one interrupt*, because that
is the only thing the workqueue hop is amortised over:

| shape | requests/s | batch (replies per IRQ) | kworker hops/s | at 3-6 us per hop | as % of the guest's ~2 vCPU cores |
|---|---|---|---|---|---|
| 26-shard storm | 28 000 | ~43 (`44-:96-97`) | 650 | 2-4 ms/s | **0.1-0.2 %** |
| create window (26 000 req/s = 7 232 creates/s x ~3.6 requests) | 26 000 | ~40 (`46-:40`) | 650 | 2-4 ms/s | **0.1-0.2 %** |
| iteration, worst case (lone requests) | 8 500 | 1 | 8 500 | 26-51 ms/s | **1.3-2.6 %** |

Even the pessimistic row - which contradicts the 26-way parallelism of the
iteration (`45-...:7`) - is **1-3 % of the guest's 48 % share, i.e. under 1.5 %
of the 8.2 s iteration**. The two other completion-side terms are the same
order and are *per request*, so they are the ones a patch would actually have to
fix, and note 22 already priced them: the bounce buffer (`kmalloc` +
`memcpy` + `kfree` per request: `fs/fuse/virtio_fs.c:710`, `:758`, called
unconditionally at `:1426`) and the workqueue hop, "both ~1-3 us each: not worth
a kernel patch for a 400 us round trip"
(`22-guest-side-round-trip-reduction.md:144-149`).

So the ranking inside the kernel space is:

1. **(rejected) run the drain in the IRQ** - ceiling 0.1-2.6 % of the guest
   share; needs a fork release to ship; must keep the `may_block` worker
   branch (`virtio_fs.c:860-868`) so it does not even remove all hops.
2. **(rejected) `max_background`/`congestion_threshold`** - already at
   `u16::MAX`/49151 via libkrun's INIT reply (`server.rs:1110-1111` →
   `inode.c:1248-1255`), and structurally unable to gate a synchronous
   request (`dev.c:88-96`, `:809-833`). Zero to gain, by construction.
3. **(the only thing worth a kernel build) instrument it.** The guest's 55 us
   per request is the last unattributed 48 % of the cost, and the pinned kernel
   cannot be profiled at all: `# CONFIG_DEBUG_FS is not set`
   (`config-libkrunfw_x86_64-kvm:3472`) and `# CONFIG_FTRACE is not set`
   (`:3626`), which is why note 22 §1.4 has to bound the guest column instead of
   measuring it (tick accounting, `CONFIG_HZ=250`, ±70 us floor,
   `22-...:196-220`). `CONFIG_FTRACE` is a `menuconfig` that defaults on under
   `CONFIG_DEBUG_KERNEL` (`kernel/trace/Kconfig:204-209`, and DEBUG_KERNEL *is*
   set, `config-libkrunfw_x86_64-kvm:3443`) - it is off only because the config
   says so - so three or four lines (`CONFIG_FTRACE`, `CONFIG_FUNCTION_TRACER`,
   `CONFIG_DYNAMIC_FTRACE`; `FUNCTION_GRAPH_TRACER` defaults to `y`,
   `kernel/trace/Kconfig:254-259`; tracefs mounts at `/sys/kernel/tracing`, so
   `CONFIG_DEBUG_FS` is not even required) plus
   `nix build .#cang-dev --override-input libkrunfw-src ...` turn
   `function_graph` loose on `fuse_*`/`virtio_*` for exact per-symbol
   microseconds - an instrument-only arm, never shipped, and the prerequisite
   for pricing *any* of the guest-side candidates in note 46's list.

**The A/B that would prove (or kill) item 1**, if someone wants it anyway:

- arms: **control** = local non-LTO `-kvm` firmware, unpatched; **test** = same
  tree + one patch moving `virtio_fs_requests_done_work`'s body into the vq
  callback (keeping the `may_block` deferral). Both booted from the same `cang`
  via `CANG_LIBKRUNFW_LIBRARY`, so only the kernel differs.
- prove which kernel booted: the guest-evidence `uname`/`/proc/version` line
  plus a `pr_info` marker in the patch, and count the marker in `dmesg` so the
  inline path provably ran (a marker that never appears is the failure mode to
  rule out first).
- workload: the no-op iteration of `tools/rust-compile-bench`
  (`45-...:5-8`) and the 26-shard create storm
  (`44-queue-depth-not-thread-count.md` §2), 3 reps per arm,
  interleaved A/B/B/A so warm cache cannot be assigned to the patch.
- observables: iteration wall seconds; guest `/proc/interrupts` delta for the fs
  device around a marker-bracketed window (same method as `44-...:96-97` - this is
  the measurement that decides the batch ratio, and it should be taken **before**
  the kernel build, because it alone can close the candidate); host per-thread
  CPU via the `split-runner.sh` sampler
  (`46-request-cost-split-and-recommendation.md` §4).
- kill criterion: a move under ~3 % (the run-to-run spread of the iteration:
  10.9 / 10.6 / 10.7 s, `45-...:21`) is not a result. Expect it.

## 6. One-line summary for the map

The guest kernel's completion path is already batched (one workqueue hop per
~40 replies), already inline for every non-blocking reply, and already at the
`max_background` ceiling; a libkrunfw patch buys at most ~1.5 % of the
iteration and costs a fork release, so the kernel is the wrong place to spend
the next cycle - spend it on a `CONFIG_FTRACE` instrumentation kernel, which is cheap through `.#cang-dev` + `CANG_LIBKRUNFW_LIBRARY` and is
what any future guest-side claim needs.
