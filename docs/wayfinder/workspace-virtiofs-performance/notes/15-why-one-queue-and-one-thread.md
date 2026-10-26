---
label: wayfinder:research
title: Why does this virtiofs device have exactly one request queue and one serving thread, and what would it take to change that?
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
# 15 - One request queue, one serving thread: origin, guest side, and what changing it would buy

Ticket: this note exists because [ticket 04](04-first-transport-change.md) closed the
worker-pool question on a single measurement (12% worker CPU) and the map's
"starting fact" states the queue limit as if a feature bit gated it. Both are worth
pinning to code and to history, because they decide whether **multiqueue** stays on
the table at all and what a future session would actually have to write.

Read-only: nothing in this tree was edited, no `cargo`, no build, no VM. The fork was
read at its current tip, `7160f48f` (`v2.0.0-cang.10-2-g7160f48f`), in
`/home/dev/cang/cang/deps/libkrun`; the guest kernel is `v7.2` (the pinned guest is
`linux-7.2.10`), fetched from `git.kernel.org`. Web pages (spec, crosvm, QEMU,
virtiofsd) were read by a delegated fetch; every quote below is from the page named
beside it.

| what | where |
|---|---|
| the fork's virtio-fs device (read here, line numbers exact) | `deps/libkrun` @ `7160f48f`, `src/devices/src/virtio/fs/{mod,device,worker,server,augment_fs}.rs`, `fs/linux/passthrough.rs` |
| fork transport plumbing | `src/devices/src/virtio/{device,mmio}.rs`, `src/libkrun/src/vmm/device_manager/kvm/mmio.rs`, `src/libkrun/src/api/device_builders.rs` |
| cang's call site | `crates/cang-libkrun/src/linked.rs` |
| guest kernel | `v7.2` `fs/fuse/virtio_fs.c` (`https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/fs/fuse/virtio_fs.c?h=v7.2`) |
| spec | OASIS virtio v1.2 csd01, `virtio-fs.tex` (`https://docs.oasis-open.org/virtio/virtio/v1.2/csd01/tex/virtio-fs.tex`) |
| crosvm | `github.com/google/crosvm`, `devices/device_virtio_fs/src/{lib.rs,vhost_user.rs}` (formerly `devices/src/virtio/fs`), commits `152117f3d3` / `bd4723b218` (2019-07-17) |
| QEMU | `hw/virtio/vhost-user-fs.c`, commit `98fc1ada4c` (2019-09-30) |
| upstream virtiofsd | `gitlab.com/virtio-fs/virtiofsd` `main`, `src/vhost_user.rs`; and the C virtiofsd in QEMU <= 7.2, `fuse_virtio.c` |
| upstream libkrun (is this a cang regression?) | `github.com/containers/libkrun` `main`, `src/devices/src/virtio/fs/mod.rs` |
| the measurements this note reasons about | map "Numbers on the table", [ticket 04](04-first-transport-change.md), `tools/rust-compile-bench/{run.sh,mon-threads.sh,README.md}` |

## Verdict

**The count is not negotiated and not feature-gated: it is a constant in the fork.**
virtio-fs exposes its request-queue count as one device-config word and has **no
`VIRTIO_F_MULTIQUEUE` bit at all** (`virtio-fs`'s only feature is
`VIRTIO_FS_F_NOTIFICATION`), so a device that writes `num_request_queues = 1`
(`fs/device.rs:78`) plus a two-entry `QUEUE_CONFIG` (`fs/mod.rs:45-47`) is the whole
limitation; the guest cannot ask for more, and the driver never looks for a feature
bit (zero hits for `MULTIQUEUE` in `virtio_fs.c` at `v7.2`). The one serving thread is
separate and newer: libkrun's own `eac94787` (2024-02-26) moved the FUSE work off the
VMM event loop onto **one** thread, `FsWorker`, which epolls both queues and calls
`server.handle_message` inline (`worker.rs:148-153,212-268`).

**Changing it is smaller than the map assumed and buys less.** Multiqueue needs no
feature bit, no virtio header change and no guest-side work - it is a `Vec<QueueConfig>`
(the console device in this same tree already does exactly that) plus one serving
thread per queue - and a pool over the *existing* single queue is smaller still
(the fork's own `block/worker.rs::ReadPool` is a working template, and upstream
virtiofsd has shipped this exact design in preference to multiqueue since 2019).
But **the measured 12% worker CPU bounds what any of it can recover at roughly 3-5 s
of a ~305 s arm (1-2%), under this harness's own 5% floor**: extra queues and extra
threads change *how many* requests are served at once, and this workload does not
queue requests. What the numbers are consistent with instead is a **5-16 us
round-trip cost per op paid 5-13 M times** - the lever that matches that diagnosis is
*fewer round trips*, not more servers.

Two corrections to the map/ticket text that follow from this:

1. The map's "`VIRTIO_F_MULTIQUEUE` is never offered, so the guest driver cannot ask for
   more queues however it is configured" has the right conclusion for the wrong
   reason, and note 01's "an honest change offers `VIRTIO_F_MULTIQUEUE` as well" is
   wrong outright - **that feature does not exist for virtio-fs**. There is nothing to
   offer and nothing for the driver to check.
2. Ticket 04's wording "at 12% it is idle 88% of the time, so the pool would add
   complexity ... to a thread that has nothing to do" reaches the right decision on a
   slightly wrong premise. A serving thread that is 12% *CPU*-busy can still be
   saturated if its service time is spent blocked (host I/O, page faults), and a
   thread can be idle because the *workload* is sparse rather than because the server
   is fast. The decision survives only because the service here is CPU-dominated
   (warm host page cache, `ioeventfd`/`irqfd` notifications - see section 4). The
   cheap probe that would settle it is named in section 5.

## 1. Where the limitation lives, line by line

### 1.1 The queue count is a static array and a config constant

```rust
// deps/libkrun/src/devices/src/virtio/fs/mod.rs:41-51
mod defs {
    use super::super::QueueConfig;

    pub const FS_DEV_ID: &str = "virtio_fs";
    pub const NUM_QUEUES: usize = 2;
    pub const QUEUE_SIZE: u16 = 1024;
    pub static QUEUE_CONFIG: [QueueConfig; NUM_QUEUES] = [QueueConfig::new(QUEUE_SIZE); NUM_QUEUES];
    // High priority queue.
    pub const HPQ_INDEX: usize = 0;
    // Request queue.
    pub const REQ_INDEX: usize = 1;
```

```rust
// fs/device.rs:73-78  (Fs::new)
let avail_features = (1u64 << VIRTIO_F_VERSION_1) | (1u64 << VIRTIO_RING_F_EVENT_IDX);

let tag = fs_id.into_bytes();
let mut config = VirtioFsConfig::default();
config.tag[..tag.len()].copy_from_slice(tag.as_slice());
config.num_request_queues = 1;
```

`VirtioFsConfig` is `{ tag: [u8; 36], num_request_queues: u32 }` (`device.rs:31-34`),
i.e. the two fields the spec defines plus the notification-buffer word the fork omits.
The transport takes the queue count from `queue_config()` and nowhere else
(`device.rs:173-175`):

```rust
fn queue_config(&self) -> &[QueueConfig] {
    &defs::QUEUE_CONFIG
}
```

and the trait contract says the transport builds queues from it, and re-creates them
from it after reset - so a device's queue count is fixed for its lifetime as written
(`device.rs:315-321`):

```rust
/// The lifecycle of a virtio device is to be moved to a virtio transport, which will then query the
/// device. The transport constructs queues based on queue_config() and passes them to the device
/// during activation, transferring ownership. After reset, the transport recreates queues
/// from queue_config() for the next negotiation cycle.
pub trait VirtioDevice: AsAny + Send {
```

Both transports read the length: MMIO (`devices/src/virtio/mmio.rs:481`) and PCI
(`devices/src/virtio/pci.rs:396`, and `NUMBER_OF_QUEUES` is written from
`queue_config.len()`, `pci.rs:687-689`).

### 1.2 There is no `set_num_queues`-like knob anywhere on this device

Grepping the whole device tree for `set_num|NUM_QUEUES|MULTIQUEUE` returns only the
static arrays per device (`block/mod.rs:20`, `net/mod.rs:11`, `fs/mod.rs:45`, ...).
The only virtio device in this tree whose queue count is *data* is vhost-user, where
it is an argument (`vhost_user/device.rs:290-300`, `350-371`), and the only one that
builds a `Vec<QueueConfig>` at runtime is console:

```rust
// devices/src/virtio/console/device.rs:82-85, 100
let num_queues = num_queues(ports.len());
let queue_config: Vec<QueueConfig> = (0..num_queues)
    .map(|_| QueueConfig::new(QUEUE_SIZE))
    .collect();
...
            queue_config,
```

`FsDevice` - the API cang actually uses - exposes exactly two setters, neither of them
a queue count (`src/libkrun/src/api/device_builders.rs:651-666`):

```rust
    pub fn set_overlay(&mut self, overlay: FsOverlay<'a>) { ... }

    /// Set the size of the DAX (direct access) shared memory window.
    ...
    pub fn set_dax_window_size(&mut self, bytes: u64) {
        self.shm_size = Some(bytes as usize);
    }
```

and its constructor fixes everything else (`device_builders.rs:671-690`, with
`shm_size: None` at `:686`). cang's only call site is three lines
(`crates/cang-libkrun/src/linked.rs:317-326`), so no cang-side option exists to ask
for more queues even if the fork had one.

### 1.3 One thread, both queues, everything inline

`activate()` builds the server and spawns **one** worker (`fs/device.rs:203-245`), and
`FsWorker` owns the server by value (`worker.rs:79-91`: `server: FsServer`), constructed
once in `FsWorker::new` (`:95-145`, `PassthroughFs::new` at `:120`). The thread:

```rust
// fs/worker.rs:148-153
pub fn run(self) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("fs worker".into())
        .spawn(|| self.work())
        .unwrap()
}
```

`work()` registers exactly two device eventfds (HPQ + REQ) and a stopfd with epoll
(`worker.rs:155-178`) - it indexes `queue_evts[HPQ_INDEX]` and `queue_evts[REQ_INDEX]`,
so a third queue would not even be watched - and `process_queue` is the serialiser
(`worker.rs:234-268`):

```rust
fn process_queue(&mut self, queue_index: usize) {
    let queue = &mut self.queues[queue_index];
    while let Some(head) = queue.pop(&self.mem) {
        ...
        let len = match self.server.handle_message(
            reader, writer, self.allow_idmap, &self.shm_region, &self.exit_code, ...
        ) { ... };

        if let Err(e) = queue.add_used(&self.mem, head.index, len as u32) { ... }

        if queue.needs_notification(&self.mem).unwrap() {
            self.interrupt.signal_used_queue();
        }
    }
}
```

`handle_message` is `&self` on `Server` (`server.rs:85-86`) and every op handler is
`&self` (`server.rs:190-1410`), so the *server* is already shareable; only the worker's
ownership (`server: FsServer`, not `Arc`) and the queue/notification handling are
single-threaded by construction. `activate()` panics on a second activation
(`device.rs:209-211`) and `reset()` joins the single handle (`device.rs:255-262`) -
those two are the join/teardown sites a pool would have to generalise.

### 1.4 This is not cang's fault, and it is recent

Upstream `containers/libkrun` `main`'s `fs/mod.rs` is **byte-identical** to the fork's
(2286 bytes fetched, same `NUM_QUEUES = 2` / `QUEUE_CONFIG` / `HPQ_INDEX` / `REQ_INDEX`
block). The single worker is newer than the device: `eac94787` (Sergio Lopez,
2024-02-26) is titled *"virtio/fs: move work to a separate thread"* and says

> Move the work to a separate thread. This reduces the operation latency, minimizes
> the noise between devices and allows us to implement more aggressive optimization
> strategies.

Before that commit the FUSE work ran in the VMM's own event loop (the commit deletes
`fs/event_handler.rs` altogether). So the fork's model is "one device, one thread" by
deliberate upstream design - and the commit's own justification ("allows us to
implement more aggressive optimization strategies") is exactly the door a pool would
walk through.

## 2. History: crosvm built the interface for N, shipped 1, and nobody adopted multiqueue

### 2.1 It is crosvm's code

Every file in the fork's `fs/` still carries the original header, e.g.
`fs/server.rs:1`, `fs/linux/passthrough.rs:1`, `fs/fuse.rs:1`:

```
// Copyright 2019 The Chromium OS Authors. All rights reserved.
```

and libkrun imported the tree wholesale in `6c20ad62` (2020-05-19, "initial commit"),
with `fs/device.rs` last touched by the *incoming* history (`f06f607c`,
`186083df`, `362a991c`, `1511be78`, `dc7f0958`, `2375ad3c` - all upstream-side) plus
the fork's own `eac94787`.

### 2.2 crosvm: the queue count is a constructor parameter, always passed 1

crosvm's device has always been N-capable: the config word is written from the
worker count (`num_request_queues: Le32::from(num_workers as u32)`, `queue_sizes =
num_workers + 1`), and the vhost-user backend hard-caps it:

```rust
// crosvm devices/device_virtio_fs/src/vhost_user.rs:50
const MAX_QUEUE_NUM: usize = 2; /* worker queue and high priority queue */
// ...:286
num_request_queues: Le32::from(1),
```

The interesting part is *why* every caller passes 1 - the comment is still there
(`devices/device_virtio_fs/src/lib.rs`, worker count argument):

```rust
// TODO(chirantan): Use more than one worker once the kernel driver has been fixed to not panic
// when num_queues > 1.
Fs::new(features, &self.tag, 1, ...)
```

Both halves landed on 2019-07-17 (`152117f3d3` "devices: Add virtio-fs device", whose
device file carries `// The fs device does not have a fixed number of queues.`, and
`bd4723b218` "Add fs device to --shared-dir", which introduces the `1` and the TODO).
That TODO is a 2019 statement about a 2019 guest kernel; the `v7.2` driver in cang's
guest has no such problem (it *asks* for `1 + num_request_queues` queues,
`virtio_fs.c:968-1000`, and only clamps down to `nr_cpu_ids`). Nothing in crosvm
revisited it, and libkrun inherited the shape and then froze it: `NUM_QUEUES = 2`
with no `num_workers` parameter at all.

### 2.3 The spec: no feature bit, only a config word

```c
struct virtio_fs_config { char tag[36]; le32 num_request_queues; le32 notify_buf_size; };
```

- "`num_request_queues` is the total number of request virtqueues exposed by the
  device. Each virtqueue offers identical functionality and there are no ordering
  guarantees between requests made available on different queues. Use of multiple
  queues is intended to increase performance."
- "The driver MAY use from one up to `num_request_queues` request virtqueues."
- "The device MUST set `num_request_queues` to 1 or greater."

virtio-fs's only device-specific feature bit is `VIRTIO_FS_F_NOTIFICATION (0)`;
there is **no generic multiqueue feature** in the virtio spec (the "MQ" feature
people remember, `VIRTIO_NET_F_MQ`, is virtio-net's). So the device's own config write
is the whole gate - which is why the fix is a `Vec` and a config word, not a feature
negotiation.

### 2.4 QEMU: the transport supports N (default 1), since QEMU 4.2

```c
// hw/virtio/vhost-user-fs.c:414
DEFINE_PROP_UINT16("num-request-queues", VHostUserFS, conf.num_request_queues, 1),
DEFINE_PROP_UINT16("queue-size", VHostUserFS, conf.queue_size, 128),
```

from `98fc1ada4c` "virtio: add vhost-user-fs base device" (2019-09-30), present
unchanged at `v4.2.0`. The default is 1, and `num-request-queues` is **undocumented**
(`docs/system/devices/virtiofsd.rst` mentions it nowhere).

### 2.5 Both virtiofsd implementations refuse more than one request queue

The C virtiofsd (QEMU <= 7.2) refuses at startup (`fuse_virtio.c:765`):

> "Ignore additional request queues for now. passthrough_ll.c must be audited for
> thread-safety issues first. It was written with a well-behaved client in mind and may
> not protect against all types of races yet." -> `"multiple request queues not yet
> implemented, please only configure 1 request queue"`

The Rust virtiofsd says the same thing as a constant, and I verified it in the file
(`src/vhost_user.rs:41-47`):

```rust
const QUEUE_SIZE: usize = 32768;
// The spec allows for multiple request queues. We currently only support one.
const REQUEST_QUEUES: u32 = 1;
// In addition to the request queue there is one high-prio queue.
// Since VIRTIO_FS_F_NOTIFICATION is not advertised we do not have a
// notification queue.
const NUM_QUEUES: usize = REQUEST_QUEUES as usize + 1;
```

and it advertises it to the front-end (`vhost_user.rs:503-506`:
`num_request_queues: Le32::from(REQUEST_QUEUES)`), i.e. **the reference backend
deliberately advertises one queue to QEMU even when QEMU could give it 16**.

Both daemons parallelise *inside* the queue instead, with a thread pool:
`--thread-pool-size` (C virtiofsd default 64; Rust virtiofsd default 0 = disabled).
The Rust pool is the template, and the design is worth recording because it is
smaller than multiqueue (verified in `src/vhost_user.rs`):

- one `Arc<Server<F>>` per device, one pool: `struct VhostUserFsThread { server:
  Arc<Server<F>>, pool: Option<ThreadPool>, ... }` (`:120-127`, built at `:130-156`
  with `ThreadPoolBuilder::new().pool_size(thread_pool_size)`, and an
  `after_start` hook that `unshare(CLONE_FS)`s each worker, `:144-152`);
- serial path: `process_queue_serial` (`:254`), used when the pool is `None`;
- pool path: `process_queue_pool` (`:213-252`) - for **every** available descriptor
  chain, clone `Arc`s (memory, server, vring) and `pool.spawn_ok(async move { ... })`,
  run `process_message`, then `Self::return_descriptor(&mut worker_vring.get_mut(),
  head_index, event_idx, len)` - i.e. the used-ring insertion happens in a *worker*,
  under the vring mutex;
- the pool is bounded by the pool size and the dispatcher blocks when it is full.

So the reference answer to this exact performance question, in the reference
implementation, in 2023-2026, is **a pool over one queue**.

## 3. The guest side with `num_request_queues = 1`

All quotes are `fs/fuse/virtio_fs.c` at `v7.2` (identical line numbers on
`torvalds/linux` `master` today, so this is stable).

### 3.1 The driver creates `1 + num_request_queues` queues, one hiprio + N requests

```c
// virtio_fs.c:41-44
enum {
	VQ_HIPRIO,
	VQ_REQUEST
};
```

```c
// virtio_fs.c:959-1000
	struct irq_affinity desc = { .pre_vectors = VQ_REQUEST };
	...
	virtio_cread_le(vdev, struct virtio_fs_config, num_request_queues,
			&fs->num_request_queues);
	if (fs->num_request_queues == 0)
		return -EINVAL;

	/* Truncate nr of request queues to nr_cpu_id */
	fs->num_request_queues = min_t(unsigned int, fs->num_request_queues,
					nr_cpu_ids);
	fs->nvqs = VQ_REQUEST + fs->num_request_queues;
	...
	for (i = VQ_REQUEST; i < fs->nvqs; i++) {
		char vq_name[VQ_NAME_LEN];

		snprintf(vq_name, VQ_NAME_LEN, "requests.%u", i - VQ_REQUEST);
		virtio_fs_init_vq(&fs->vqs[i], vq_name, VQ_REQUEST);
		...
	}

	ret = virtio_find_vqs(vdev, fs->nvqs, vqs, vqs_info, &desc);
```

With the fork's `num_request_queues = 1`: `nvqs = 2`, so the guest creates `hiprio` and
exactly one request queue, named `requests.0`. `num_request_queues = 0` is a hard
`-EINVAL`, so a device that advertises virtio-fs must expose at least one request
queue.

### 3.2 Work is spread per **CPU**, not per inode - and with one queue it is not spread

```c
// virtio_fs.c:881-917  virtio_fs_map_queues()
	if (!vdev->config->get_vq_affinity)
		goto fallback;

	for (q = 0; q < fs->num_request_queues; q++) {
		mask = vdev->config->get_vq_affinity(vdev, VQ_REQUEST + q);
		if (!mask)
			goto fallback;

		for_each_cpu(cpu, mask)
			fs->mq_map[cpu] = q + VQ_REQUEST;
	}
	return;
fallback:
	/* Attempt to map evenly in groups over the CPUs */
	masks = group_cpus_evenly(fs->num_request_queues, &nr_masks);
	/* If even this fails we default to all CPUs use first request queue */
	if (!masks) {
		for_each_possible_cpu(cpu)
			fs->mq_map[cpu] = VQ_REQUEST;
		return;
	}
```

`mq_map` is `index = cpu id, value = request vq id` (`:75`), and submission uses the
**current CPU**:

```c
// virtio_fs.c:1499-1519  virtio_fs_send_req()
	fs = fiq->priv;
	queue_id = fs->mq_map[raw_smp_processor_id()];
	...
	fsvq = &fs->vqs[queue_id];
	ret = virtio_fs_enqueue_req(fsvq, req, false, GFP_ATOMIC);
```

There is **no per-inode, per-file or hash-based placement**: with one request queue
every branch of `virtio_fs_map_queues()` writes `VQ_REQUEST` for every CPU, so every
request from every guest task, on every CPU, for every inode, lands on the single
`vqs[VQ_REQUEST]` (`fuse_req_hash` at `:1469` is only the reply-lookup bucket).

The enqueue itself is a per-queue spinlock plus `virtqueue_add_sgs`, with kick
suppression by `VIRTIO_RING_F_EVENT_IDX` (`:1452-1484`):

```c
	spin_lock(&fsvq->lock);
	...
	vq = fsvq->vq;
	ret = virtqueue_add_sgs(vq, sgs, out_sgs, in_sgs, req, GFP_ATOMIC);
	...
	notify = virtqueue_kick_prepare(vq);

	spin_unlock(&fsvq->lock);

	if (notify)
		virtqueue_notify(vq);
```

So submission from many guest threads to the one queue is *already* concurrent and
lock-protected - the lock is per queue and held for a handful of instructions - and
the guest already batches notifications and the host already drains the whole ring per
event (`worker.rs:234-236`: `while let Some(head) = queue.pop(&self.mem)`). There is no
per-op handshake to remove on the submission side.

### 3.3 What the guest does when the ring is full: parks and retries on the same queue

`-ENOSPC` from `virtqueue_add_sgs` puts the request on `fsvq->queued_reqs` and a worker
retries it on **that same** `fsvq` (`:1521-1531`; retry at `:594`), never another
queue. So in-flight capacity is: ring size (1024 descriptors on the fork side,
`fs/mod.rs:46`) + unbounded `queued_reqs` backlog; `max_background` only bounds
*background* requests. Note 08 already established the operational consequence: a full
ring returns `-ENOSPC`, which the guest defers silently - "the single queue is
saturated" is not observable that way.

### 3.4 Does the guest mention `VIRTIO_F_MULTIQUEUE` anywhere? No

`MULTIQUEUE` has **zero hits in `virtio_fs.c` at `v7.2`**, `virtio_has_feature()` has
zero hits in the file (`feature_table[]` is empty, `:1242`), and the only `VIRTIO_F*`
token in the driver is `VIRTIO_FS_SHMCAP_ID_CACHE` (`:1093`). The queue count is read
from config and used directly. Confirms section 2.3 and kills the map's framing.

## 4. So what actually serialises today, and what the numbers say

### 4.1 What is measured

From the map and ticket 04 (1 Hz per-thread sampling of the VMM,
`tools/rust-compile-bench/mon-threads.sh` -> `summarise-threads.sh`), on the
guest-virtiofs arm of the synthetic workload (2401 crates / 16801 `.rs` files, cold
`target/`, `CARGO_BUILD_JOBS=26`):

- build 299-311 s (recorded 298.9 / 306.5 / 311.4), guest-local `/dev/shm` control
  232 s -> **penalty 67-79 s, i.e. ~30%**, `1.29-1.34x`;
- `fs_worker` (the only thread serving the share, `worker.rs:150`) **12.3% of one
  core** over a ~315 s span -> ~39 s of CPU;
- guest vCPUs: `fc_vcpu_0` 65.1%, the rest ~22% each -> ~6.6 of 28 cores busy (23%).

Caveat carried from the map: another session may have been running a smoke during
those runs ([ticket 12](12-clean-window-re-record.md) re-records them), and the last
`summary.tsv` in `/home/dev/cang-bench` contains duplicate/garbage rows from the
harness run that was stopped - so treat the *ratios* as the claim and the seconds as
provisional.

### 4.2 The 12% does not mean "no headroom" - but here it does mean "not the constraint"

Two facts make the low CPU number trustworthy *for this workload*, and neither is
"the thread was idle":

1. **The notification path is already in-kernel on the host.** cang attaches its
   virtio devices through the MMIO manager, which registers a KVM `ioeventfd` per
   queue **and** an `irqfd` for the device's interrupt line
   (`src/libkrun/src/vmm/device_manager/kvm/mmio.rs:133-143`):

   ```rust
   for (i, queue_evt) in mmio_device.queue_evts().iter().enumerate() {
       let io_addr = IoEventAddress::Mmio(
           self.mmio_base + u64::from(devices::virtio::NOTIFY_REG_OFFSET),
       );
       vm.register_ioevent(queue_evt, &io_addr, i as u32)
           .map_err(Error::RegisterIoEvent)?;
   }
   vm.register_irqfd(mmio_device.interrupt_evt(), self.irq)
       .map_err(Error::RegisterIrqFd)?;
   ```

   So a guest kick does not exit to the VMM, and a completion interrupt is injected by
   the host kernel; `signal_used_queue` writes that eventfd only
   (`devices/src/virtio/device.rs:72-76`). The remaining
   round-trip cost is thread wakeups and the host syscalls, not VM exits.

2. **The worker's service is CPU-dominated.** On a warm host page cache a passthrough
   `lookup`/`open`/`read`/`write` is a syscall or two plus descriptor parsing, a
   `RwLock`/`BTreeMap` lookup and copies (`passthrough.rs:392,397`; `server.rs`'s
   per-op handlers) - all charged to the thread as `utime+stime`. There is no place
   for it to sleep the 88% of the arm the sampler says it is not on-CPU, short of host
   I/O stalls, which a build that has just written its inputs does not have much of.

Given those, the worker's *service occupancy* is approximately its CPU share:
`rho ~ 0.12` (generously 0.25), and the total service delivered over the arm is
`S ~ 39 s`.

### 4.3 The bound: what a pool or multiqueue can win at that occupancy

For a single server, the *queueing* component of a request's sojourn time is
`W_q = (rho / (1 - rho)) * E[S]`. At `rho = 0.12` that is `0.14 * E[S]`, and everything
it could ever remove is bounded by `0.14 * S ~ 5.4 s` of aggregate request time.

That aggregate is not wall time, but it is the *ceiling*, and it is already small:

| reading | rho | `W_q / E[S]` | aggregate queueing over the arm | as % of the 67-79 s penalty | as % of the 305 s arm |
|---|---|---|---|---|---|
| CPU share = service share (likely) | 0.12 | 0.14 | ~5 s | ~7% | ~1.7% |
| service takes 2x its CPU (generous) | 0.25 | 0.33 | ~26 s | ~35% | ~8% |
| worker saturated | ~1.0 | unbounded | - | would be the whole penalty | - |

Only the third row makes a pool the lever, and it is the row the measurement excludes
(39 s of CPU cannot be 100% of a 305 s arm's service). The harness's own significance
rule is ~5% on the guest-virtiofs arm, so *even the generous row* is borderline and
the likely row is invisible.

Equivalently: extra queues and extra threads change **how many** requests are in
service at once. They do not make any single request's round trip shorter. At 12%
occupancy there is essentially nothing to overlap.

### 4.4 What the ~70 s is consistent with instead: a per-op round trip, paid 5-13 M times

Two independent constraints bracket the op count `N` and the added latency per op
`Delta = penalty / N`:

- **A (worker CPU budget):** `N = 39 s / c`, where `c` is the host CPU per op. A warm
  passthrough metadata op costs ~3-8 us (one or two syscalls, descriptor parse, inode
  and handle lookups, reply encode); 1 MiB data copies are ~0.2 us/KiB and the total
  bytes a build moves add well under a second, so `c` is metadata-dominated.
  -> `N ~ 4.8 M - 13 M`.
- **B (latency plausibility):** `Delta = 67-79 s / N` must be at least one wakeup
  chain (ioeventfd -> fs_worker epoll wake -> syscalls -> used-ring write ->
  irqfd -> guest IRQ -> FUSE task wake), which is several microseconds, and at most
  the few tens of microseconds a loaded host can add.
  -> with the `N` above, `Delta ~ 5-16 us`; conversely `Delta` in that band needs
  `N ~ 4-16 M`.

The intersection is **N ~ 5-13 M FUSE requests per build, Delta ~ 5-16 us each**, i.e.
**~2000-5400 requests per crate, or 300-800 per source file**. That is the right order
for rustc (a `statx` storm over the sysroot and dependency trees, one open/read per
source, plus `target/` writes), and it is 3-10x the cost of the same op against a
local filesystem (a warm `statx`/`open` pair is ~1-2 us), which is exactly the
observed 30%.

Note what this decomposition says about *where* the penalty is: 70 s / 305 s is not a
shortage of parallel service capacity (the guest is at 23% of its vCPUs and the server
at 12% of one core); it is the price of each of millions of round trips on a build
whose wall time is set by dependency chains and scheduling, not by throughput. That is
also consistent with the *VM* penalty being 2.2x the host while the FUSE penalty on top
is only 1.3x: both are latency phenomena on the same slack-rich machine.

## 5. What multiqueue / threading would take, and what it would buy

### 5.1 The change inventory (nothing here is a feature negotiation)

| # | change | site |
|---|---|---|
| 1 | `QUEUE_CONFIG` static array -> per-device `Vec<QueueConfig>` built in `Fs::new` (console is the in-tree precedent) | `fs/mod.rs:45-47`, `fs/device.rs:76-78`, `:173-175`; pattern at `console/device.rs:82-85,100` |
| 2 | `config.num_request_queues = n` (and clamp `n <= available_parallelism`) | `fs/device.rs:78` |
| 3 | `FsWorker` over N+1 queues: today `work()` indexes exactly `HPQ_INDEX` and `REQ_INDEX`; one thread per request queue (crosvm's original shape) or a pool over all of them | `fs/worker.rs:155-210` |
| 4 | `server: FsServer` -> `Arc<FsServer>` (or one server per thread, which breaks the shared inode map - not an option) | `fs/worker.rs:79-91,133-145` |
| 5 | `worker_thread: Option<JoinHandle>` -> `Vec<...>`; `activate()` panic guard and `reset()` join updated | `fs/device.rs:209-211`, `:255-262` |
| 6 | an `FsDevice` knob (`set_request_queues`) mirrored through the ffier binding, plumbed in `linked.rs::fs_device_new`, and set from cang's launcher - the shape `block` already uses for `set_parallel_reads` | `device_builders.rs:651-690`, `linked.rs:317-326`, `linked.rs:340-346` |

No guest-side work, no virtio header change, no feature bit (section 2.3), and the
transports already size themselves from `queue_config().len()` (section 1.1). The
guest would then create `1 + n` queues and spread CPUs over them
(`virtio_fs.c:968-1000`, `:881-917`).

### 5.2 Shared state is not the problem

- `Server::handle_message` and every op are `&self` (`server.rs:85-86`, `:190-1410`);
- `PassthroughFs` keeps all mutable state behind locks/atomics: `inodes:
  RwLock<MultikeyBTreeMap<...>>` (`passthrough.rs:392`), `handles: RwLock<BTreeMap<...>>`
  (`:397`), `writeback: AtomicBool` (`:408`);
- `AugmentFs` is the same (`augment_fs.rs:55,58`);
- `InodeAllocator` is already shared by `Arc` (`worker.rs:109`).

Upstream virtiofsd runs its whole pool against the same two-lock model, which is the
empirical proof that no inode-map refactor is needed. The work is (a) *who* touches the
used ring and signals, and (b) join/teardown.

### 5.3 The pool design already exists in this fork, for virtio-blk

`79228a72` ("virtio/blk: serve guest reads in parallel by default") parallelised guest
reads, and its message is the same diagnosis this note reaches:

> a guest running parallel builds page-ins the disk image from one serialized device
> thread otherwise

The implementation is a reusable template (`block/worker.rs`): a bounded `ReadPool`
with a job channel, a completion channel, a completion `EventFd` and a `pending`
counter (`:102-173`), the dispatcher drains completions and *it* does `add_used` +
`signal_used_queue` (`:365-376`, `:443-476`), with backpressure when
`pending == workers.len()`, and `resolve_read_pool_size` defaulting to
`available_parallelism().clamp(2, 8)` (`:602-612`). An `FsPool` would be the same
object with `handle_message` as the job body - i.e. **the used ring stays owned by the
existing single thread**, which sidesteps the mutex/notification-race worry that
note 01 raised entirely.

For reference, virtiofsd's shape (§2.5) differs only in that its workers do the
`return_descriptor` themselves under a vring mutex (`vhost_user.rs:247`).

### 5.4 What only multiqueue adds, and why it is close to inert in cang's transport

Per-queue serving threads add nothing a pool over one queue does not already have
(concurrency, shared server, bounded work). What multiqueue adds *on top* is
guest-side spread: per-CPU queue pinning (`virtio_fs.c:1511`), one guest spinlock per
queue (`:1452`), and - on a transport with MSI-X - one interrupt vector per queue. In
cang's MMIO transport there is exactly **one IRQ line per device**
(`kvm/mmio.rs:142-145` registers a single `irqfd` for the whole device), so all queues
would share one interrupt and one ISR path; the only per-queue difference is which
`ioeventfd` data match a kick carries (`:133-140`). Multiqueue here would therefore be
strictly more code than the pool, for the same concurrency and no interrupt spread.

### 5.5 The measurements that would falsify this note

Each is cheap, and the first two need no fork change:

1. **Concurrency-scaling probe inside one guest arm** (decisive, cheapest). A tiny
   loop doing `statx`+`open`+`read` on `/workspace` at concurrency 1, 4, 16, 32,
   reporting ops/s. If ops/s is ~flat, the server is serialising and a pool wins; if it
   scales to ~26 threads (matching `CARGO_BUILD_JOBS=26`), the server has headroom and
   the penalty is per-op latency. Expected here: throughput scales until the guest's
   CPU or the host's cores bind.
2. **Sum the `fc_vcpu_*` columns** of the two guest arms' existing per-thread captures
   (`threads-guest-local-*.txt` vs `threads-guest-virtiofs-*.txt`). If the virtiofs arm
   burns materially more *guest* CPU than the local arm, part of the 30% is the guest's
   own FUSE client work - which no server-side change (pool, queues, threads) can
   recover, and which moves the lever to "fewer requests" (compiler-side) rather than
   "more servers". This is a same-instrument comparison, so it also survives the
   contended-window caveat.
3. **Ring occupancy, fork-side.** One atomic per `process_queue` iteration sampling
   `queue.avail_index - queue.used_index` into a histogram, printed at `reset()`. This
   replaces the 12% proxy with the real utilisation and makes `rho` a measurement.
   (`process_queue`, `worker.rs:234-268`; `reset`, `device.rs:255-262`.)
4. **Request count and per-op wall time**, same instrumentation: `N` and a per-opcode
   latency histogram settle section 4.4's bracket, and the *wall* (not CPU) time per op
   decides whether the service time is CPU-dominated - the one assumption this note
   leans on.
5. **The A/B nobody should skip if 1-2 come out the wrong way:** a 4-thread `FsPool`
   with the numbers above predicts < 2% on the arm (invisible), a saturated worker
   predicts >= 15%. Anything in between needs the 5% floor and more runs than the
   harness currently takes.

## 6. What this note settles, and what to do with it

- The limitation is **a constant in the fork** (`NUM_QUEUES = 2`,
  `num_request_queues = 1`), inherited from crosvm's 2019 device whose callers always
  passed `num_workers = 1` because of a guest-kernel-panic TODO, and made into a
  one-thread server by libkrun's own 2024 change. Nothing negotiates it, no feature bit
  gates it, and cang exposes no knob.
- **Multiqueue is cheaper than the map feared but not cheaper than the pool**, and in
  cang's MMIO transport it would not even spread interrupts. It stays off the table for
  a reason that is now precise: it changes service *capacity*, and capacity is not what
  this build lacks.
- This **agrees with [ticket 04](04-first-transport-change.md)'s decision** (no worker
  pool yet) and replaces its premise with a bounded one: at the measured occupancy a
  perfect pool recovers <= ~5 s of a ~305 s arm. It also **sharpens what would reopen
  it**: a ring-occupancy histogram, or a concurrency-scaling probe that stays flat.
- The lever that matches the diagnosis is **fewer round trips per build** (and a
  cheaper per-op path), which is consistent with ticket 03's coherence rule squeezing
  the caching options out, and with the map's "what remains ~30% would take" note.
- Not settled: whether the remaining per-op cost is host-side (wakeups, syscalls) or
  guest-side (FUSE client, page-cache invalidation under `CachePolicy::Auto`), and how
  much of the 30% is the guest's own FUSE CPU. Measurement 2 above is the cheap way to
  split it, and it needs no new runs beyond [ticket 12](12-clean-window-re-record.md).
