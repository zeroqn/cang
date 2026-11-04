---
label: wayfinder:research
title: What does one FUSE round trip actually cost in VM exits, and what is the exit floor?
status: open
blocked_by: []
claimed_by: pi research child (2026-11-04)
---

# 49 - The exit budget of one FUSE round trip

Ticket: exit-removal candidates for the `/workspace` share, after notes 44/45/46
priced the request and found no single owner of the 516 us per create.

Read-only: no build, no VM boot, no fork edit. Sources are the fork itself
(`deps/libkrun` @ `18e0e371706f21e4d9d325c5ee53a3abdd067851`, tag
`v2.0.0-cang.17`), the guest kernel at `v7.2` fetched from `git.kernel.org`
(the pinned tree is `linux-7.2.10`, `deps/libkrunfw/Makefile:1`), the kernel's
own KVM API doc at the same ref, and the measured notes of this map.

Everything in §1-§2 is quoted from source. §3 prices it with note 22's
third-party figures and this map's measurements; §3 is arithmetic, not a
measurement.

## Verdict

1. **A round trip costs three hardware VM exits, and only two of them reach
   userspace.** The kick is one exit that `kvm.ko` absorbs through an ioeventfd
   (§1); the reply costs the guest two more, the `INTERRUPT_STATUS` read and the
   `INTERRUPT_ACK` write, both of which are MMIO to an address nobody registered
   (§2). There is no spin, no polling and no shared-memory notification scheme
   on the device side: the fork sleeps in `epoll_wait(-1)` on the queue eventfd.
2. **`VIRTIO_RING_F_EVENT_IDX` is offered, negotiated and already doing its
   job.** It coalesces the *completion* side to exactly one interrupt per guest
   callback-enable window, which is measured at 11-43 replies per interrupt
   under load. It does nothing for the *kick* side, where the device always
   re-arms at `next_avail`, so every single request still kicks.
3. **Per create, the exits are ~5-10 us of 516 us of host CPU (1-2 %), or 4-8 %
   of the ~120 us critical-path wall.** Amortised completion signalling is the
   reason the CPU share is small; if every exit were free the 8.2 s iteration
   would become ~7.6-7.9 s.
4. **Ranked by (saving x confidence) / effort: register the `INTERRUPT_ACK`
   write as an ioeventfd** (host-only, ~15 lines, deletes 1 of the 2 userspace
   exits) **> suppress the kick and poll after publishing** (deletes the last
   per-request exit at the price of a bounded spin) **> a cheaper ioeventfd
   registration** (sub-microsecond) **> MSI-X over PCI** (deletes both ISR
   exits, but needs a transport the fork does not have and cang does not use).
   EVENT_IDX is not an option - it is already shipped. Batching completions
   behind one interrupt per N replies is measured, banked, and *harmful* at
   depth 1.
5. **What is not achievable: an exit-free round trip on the transport cang
   boots today, and anything close to tmpfs.** An MMIO *read* cannot be trapped
   in-kernel, so virtio-mmio's ISR read always exits, and the guest's kick only
   disappears if the device asks it not to and polls instead. Delete every exit
   and the share still runs a compile iteration at ~7.5-7.9 s against tmpfs's
   1.3-2.1 s (note 45 §1).

## 1. The request path: one exit, handled inside the kernel

### 1.1 The guest's kick

`virtio_fs_enqueue_req()` builds the scatterlist, adds it, and kicks:

```c
1461 	ret = virtqueue_add_sgs(vq, sgs, out_sgs, in_sgs, req, GFP_ATOMIC);
...
1477 	if (!in_flight)
1478 		inc_in_flight_req(fsvq);
1479 	notify = virtqueue_kick_prepare(vq);
1480
1481 	spin_unlock(&fsvq->lock);
1482
1483 	if (notify)
1484 		virtqueue_notify(vq);
```
(`fs/fuse/virtio_fs.c:1461-1484`, v7.2)

`virtqueue_notify()` calls the transport's `notify` op
(`drivers/virtio/virtio_ring.c:3071-3084`), and for virtio-mmio that op is a
single 4-byte MMIO write of the queue index:

```c
264 static bool vm_notify(struct virtqueue *vq)
265 {
266 	struct virtio_mmio_device *vm_dev = to_virtio_mmio_device(vq->vdev);
267
268 	/* We write the queue's selector into the notification register to
269 	 * signal the other end */
270 	writel(vq->index, vm_dev->base + VIRTIO_MMIO_QUEUE_NOTIFY);
271 	return true;
272 }
```
(`drivers/virtio/virtio_mmio.c:264-272`)

### 1.2 Who consumes it, and how

The fork registers that address once per queue as an **ioeventfd**, matched on
the written queue index, and the interrupt eventfd as an **irqfd**:

```rust
137         for (i, queue_evt) in mmio_device.queue_evts().iter().enumerate() {
138             let io_addr = IoEventAddress::Mmio(
139                 self.mmio_base + u64::from(devices::virtio::NOTIFY_REG_OFFSET),
140             );
141
142             vm.register_ioevent(queue_evt, &io_addr, i as u32)
143                 .map_err(Error::RegisterIoEvent)?;
144         }
145
146         vm.register_irqfd(mmio_device.interrupt_evt(), irq)
147             .map_err(Error::RegisterIrqFd)?;
```
(`deps/libkrun/src/libkrun/src/vmm/device_manager/kvm/mmio.rs:137-147`)

`NOTIFY_REG_OFFSET` is `0x50` (`deps/libkrun/src/devices/src/virtio/mod.rs:95`),
i.e. exactly `VIRTIO_MMIO_QUEUE_NOTIFY`; the whole 4 KiB page is put on the MMIO
bus (`kvm/mmio.rs:151-153`, `MMIO_LEN = 0x1000` at `:86`). KVM then absorbs the
write in the kernel:

> "This ioctl attaches or detaches an ioeventfd to a legal pio/mmio address
> within the guest.  A guest write in the registered address will signal the
> provided event instead of triggering an exit."
> - `Documentation/virt/kvm/api.rst:2102-2104`

> "If datamatch flag is set, the event will be signaled only if the written
> value to the registered address is equal to datamatch"
> - `Documentation/virt/kvm/api.rst:2128-2129`

So the `0x50` write costs **one hardware VM exit and zero userspace entries**:
the vCPU leaves the guest, `kvm_mmio` finds the ioeventfd, signals the eventfd
and resumes the guest. The userspace fallback that the fork still carries
(`deps/libkrun/src/devices/src/virtio/mmio.rs:361-368`, `0x50 =>
eventfd.write(1)`) only runs on a datamatch miss, which the guest's queue-index
write cannot produce.

**Nothing spins and nothing is polled.** The device thread blocks in an epoll
wait with an infinite timeout, on the two queue eventfds, the stop fd and (with
the pool) the completion fd:

```rust
474         let mut epoll = Epoll::new().unwrap();
475
476         let _ = epoll.ctl(
477             ControlOperation::Add,
478             virtq_hpq_ev_fd,
...
499         let mut epoll_events = vec![EpollEvent::new(EventSet::empty(), 0); 32];
500         loop {
501             match epoll.wait(epoll_events.len(), -1, epoll_events.as_mut_slice()) {
```
(`deps/libkrun/src/devices/src/virtio/fs/worker.rs:474-501`)

On wake it drains the eventfd and runs the spec's
disable/drain/re-enable loop:

```rust
557     fn handle_event(&mut self, queue_index: usize) {
558         debug!("Fs: queue event: {queue_index}");
559         if let Err(e) = self.queue_evts[queue_index].read() {
560             error!("Failed to get queue event: {e:?}");
561         }
...
566         loop {
567             if let Err(e) = self.queues[queue_index].disable_notification(&self.mem) {
...
572             self.process_queue(queue_index);
573
574             match self.queues[queue_index].enable_notification(&self.mem) {
575                 Ok(true) => {}
576                 Ok(false) => break,
```
(`deps/libkrun/src/devices/src/virtio/fs/worker.rs:557-582`)

There is no `VIRTIO_F_NOTIFICATION_DATA`, no shared-memory doorbell and no
shm-based notification anywhere on this path: the fork offers only two feature
bits (`fs/device.rs:78`, quoted in §2.3), so the guest uses `vm_notify`, not
`vm_notify_with_data` (`virtio_mmio.c:274-282`).

### 1.3 It is one kick per *request*, not one per batch

`virtqueue_kick_prepare()` is called per enqueue (`virtio_fs.c:1479`), so a
batch of *k* requests added by *k* enqueues is *k* kick decisions. With
`VIRTIO_RING_F_EVENT_IDX` negotiated the decision is

```c
219 static inline int vring_need_event(__u16 event_idx, __u16 new_idx, __u16 old)
220 {
...
226 	return (__u16)(new_idx - event_idx - 1) < (__u16)(new_idx - old);
227 }
```
(`include/uapi/linux/virtio_ring.h:219-227`)

and the fork re-arms `avail_event` to the index it has already drained, every
time it re-enables notifications:

```rust
650         if enable {
651             if self.event_idx_enabled {
652                 // We call `set_avail_event` using the `next_avail` value, instead of reading
653                 // and using the current `avail_idx` to avoid missing notifications. More
654                 // details in `enable_notification`.
655                 self.set_avail_event(mem, self.next_avail.0, Ordering::Relaxed)
656             } else {
```
(`deps/libkrun/src/devices/src/virtio/queue.rs:650-656`)

With `event_idx = X` (the drained index), `old = X`, `new = X+1`:
`(X+1) - X - 1 = 0 < (X+1) - X = 1`, so the guest kicks. It keeps kicking: the
device only rewrites `avail_event` in `enable_notification` (`queue.rs:688-704`),
so every subsequent add also satisfies the inequality. **One hardware exit per
request, unconditionally, from the guest's side.** (Note 46 §2 says "plus one
4-byte MMIO kick per batch" - it is per *request*.)

## 2. The completion path: two exits, amortised over N replies

### 2.1 Publication is guest-memory writes; the interrupt is an irqfd

`add_used()` stores the used element and then the used index with release
ordering (`queue.rs:544-580`), `needs_notification()` decides whether the guest
wants to hear about it (§2.3), and the signal is one eventfd write:

```rust
596             if let Err(e) =
597                 self.queues[queue_index].add_used(&self.mem, completion.index, completion.len)
...
601             if self.queues[queue_index]
602                 .needs_notification(&self.mem)
603                 .unwrap_or(true)
604             {
605                 self.interrupt.signal_used_queue();
606             }
```
(`deps/libkrun/src/devices/src/virtio/fs/worker.rs:596-606`; the inline path is
identical at `:674-682`)

`signal_used_queue` sets the `VIRTIO_MMIO_INT_VRING` bit and hands the interrupt
eventfd to the irqchip (`deps/libkrun/src/devices/src/virtio/mmio.rs:120-129`,
`:160-168`), which on x86_64/KVM is nothing but

```rust
 60     fn set_irq(
...
 65         if let Some(interrupt_evt) = interrupt_evt {
 66             if let Err(e) = interrupt_evt.write(1) {
```
(`deps/libkrun/src/devices/src/legacy/kvmioapic.rs:60-69`)

> "Allows setting an eventfd to directly trigger a guest interrupt. ... When an
> event is triggered on the eventfd, an interrupt is injected into the guest
> using the specified gsi pin."
> - `Documentation/virt/kvm/api.rst:3187-3193`

Assertion costs **no VM exit at all** - it is host-to-guest.

### 2.2 The guest's handler costs two userspace exits

```c
285 static irqreturn_t vm_interrupt(int irq, void *opaque)
286 {
287 	struct virtio_mmio_device *vm_dev = opaque;
288 	struct virtqueue *vq;
289 	unsigned long status;
290 	irqreturn_t ret = IRQ_NONE;
291
292 	/* Read and acknowledge interrupts */
293 	status = readl(vm_dev->base + VIRTIO_MMIO_INTERRUPT_STATUS);
294 	writel(status, vm_dev->base + VIRTIO_MMIO_INTERRUPT_ACK);
295
296 	if (unlikely(status & VIRTIO_MMIO_INT_CONFIG)) {
...
301 	if (likely(status & VIRTIO_MMIO_INT_VRING)) {
302 		virtio_device_for_each_vq(&vm_dev->vdev, vq)
303 			ret |= vring_interrupt(irq, vq);
304 	}
```
(`drivers/virtio/virtio_mmio.c:285-307`)

Neither `0x60` nor `0x64` is ioeventfd-registered (§1.2 registers only `0x50`),
so both arrive in userspace and are dispatched on the bus:

```rust
1557                 VcpuExit::MmioRead(addr, data) => {
1558                     if let Some(ref mmio_bus) = self.mmio_bus {
1559                         mmio_bus.read(0, addr, data);
1560                     }
1561                     Ok(VcpuEmulation::Handled)
1562                 }
1563                 VcpuExit::MmioWrite(addr, data) => {
1564                     if let Some(ref mmio_bus) = self.mmio_bus {
1565                         mmio_bus.write(0, addr, data);
1566                     }
1567                     Ok(VcpuEmulation::Handled)
1568                 }
```
(`deps/libkrun/src/libkrun/src/vmm/linux/vstate.rs:1557-1568`)

Each of the two then does a `BTreeMap` range scan plus a `Mutex` lock on the
transport (`deps/libkrun/src/devices/src/bus.rs:146-171`), landing in the
transport's ISR read (`virtio/mmio.rs:287`, `0x60 => self.interrupt.status()`)
and its ack handler (`virtio/mmio.rs:369-375`, `0x64 => ... on_ack(v)`).
Everything after that - the callback, `virtqueue_get_buf`, the workqueue hop
(`virtio_fs.c:919-927`, `:828-879`) - is guest memory and guest scheduling, not
exits.

### 2.3 EVENT_IDX is negotiated today - and coalesces the right half

What the fork offers:

```rust
 78         let avail_features = (1u64 << VIRTIO_F_VERSION_1) | (1u64 << VIRTIO_RING_F_EVENT_IDX);
```
(`deps/libkrun/src/devices/src/virtio/fs/device.rs:78`)

What the guest does with it: the transport's `finalize_features` lets virtio_ring
keep the bit (`drivers/virtio/virtio_mmio.c:109-132`, `vring_transport_features`
at `drivers/virtio/virtio_ring.c:3548-3568` case `VIRTIO_RING_F_EVENT_IDX`), and

```c
1376 	vq->event = virtio_has_feature(vdev, VIRTIO_RING_F_EVENT_IDX);
```
(`drivers/virtio/virtio_ring.c:1376`)

The device side honours it by flipping the queue into event-idx mode from the
acked features:

```rust
221         let event_idx_enabled =
222             (locked_device.acked_features() & (1 << VIRTIO_RING_F_EVENT_IDX)) != 0;
223         for device_queue in &mut device_queues {
224             device_queue.queue.set_event_idx(event_idx_enabled);
225         }
```
(`deps/libkrun/src/devices/src/virtio/device.rs:221-225`)

`fs/fuse/virtio_fs.c` never calls `virtio_has_feature()` at all (grep: no hits;
already recorded in note 01 §1.2) - the negotiation is entirely
transport/ring-layer, which is why nothing in the FS driver needs to change for
it.

### 2.4 Why that gives exactly one interrupt per N replies

`virtio_fs_requests_done_work()` drains with the spec's disable/enable loop:

```c
 841 	do {
 842 		virtqueue_disable_cb(vq);
 843
 844 		while ((req = virtqueue_get_buf(vq, &len)) != NULL) {
...
 853 	} while (!virtqueue_enable_cb(vq));
```
(`fs/fuse/virtio_fs.c:841-853`)

and `virtqueue_enable_cb` sets `used_event = last_used_idx`
(`drivers/virtio/virtio_ring.c:1109-1131`). On the device side the decision is

```rust
739         if self.event_idx_enabled {
740             let used_event = self.used_event(mem, Ordering::Relaxed)?;
741             let old = used_idx - self.num_added;
742             self.num_added = Wrapping(0);
743             return Ok(used_idx - used_event - Wrapping(1) < used_idx - old);
744         }
```
(`deps/libkrun/src/devices/src/virtio/queue.rs:739-744`)

With `used_event = L` and `num_added = 1` per publication: reply 1 gives
`(L+1) - L - 1 = 0 < 1` -> **interrupt**; reply 2 gives `1 < 1` -> no interrupt;
reply 3 gives `2 < 1` -> no interrupt; and so on until the guest re-arms. That is
*exactly* the measured 11-43 replies per host-to-guest notification (note 44 §5:
6 809 interrupts in 10.46 s at 7 002 creates/s; note 46 §2: 9 756 in 15.0 s).

So the completion-side exit cost is **2/N userspace exits per round trip**, and
the kick-side cost is **1 hardware exit per round trip** whether or not anything
is batched:

| per FUSE round trip | depth 1 (compile-iteration shape) | burst, N = 11 (storm) |
|---|---|---|
| kick `writel(0x50)` | 1 HW exit, in-kernel, 0 userspace | 1 per request (same) |
| irqfd assertion | 0 | 0 |
| `readl(0x60)` + `writel(0x64)` | 2 userspace exits | 2/11 = 0.18 |
| **total** | **3 HW exits, 2 of them userspace** | **1.18 HW exits, 0.18 userspace** |

Nothing about the count is a bug in the fork: it is the arithmetic of a
virtio-mmio transport with one request in flight. It is the same three-exit count
note 22 §1.3 derived; this note only re-derives it against the current fork HEAD
and separates the kick (kernel-handled) from the ISR pair (userspace).

## 3. What an exit is worth, and its share of the 516 us

The only per-exit numbers this repo has are note 22 §1.3's, and they are
third-party bare-metal measurements (the host itself is a KVM guest, so treat
them as a floor):

| item | figure | source |
|---|---|---|
| userspace MMIO read/write handler | **1.81 us** avg (0.80-11.58, n=4011) | note 22 §1.3, microkvm `docs/en/benchmark.md` |
| irqfd injection | 5.81 us avg (n=11) | same |
| a kernel-side exit | "a few micro-seconds" | `Documentation/virt/kvm/halt-polling.rst:14-21`, cited in note 22 |
| whole transition budget of a round trip (3 exits + injection + wake) | **8-18 us** | note 22 §1.3 |
| kick exit alone | ~1-2 us | note 22 §1.3 (an assumption, not a measurement) |

Against note 46's split (516 us of host CPU per create, ~120 us of critical-path
wall, note 46 §1):

- **Depth 1** (the compile-iteration shape: ~69k requests in 8.2 s, ~8.5k
  requests/s, "about one effective stream", note 46 §1): 2 userspace exits =
  3.6 us at the 1.81 us floor, plus the 1-2 us kernel-handled kick ->
  **5-10 us per round trip**, i.e. **1-2 % of the 516 us of CPU** and **4-8 % of
  the ~120 us of wall**.
- **Burst (N = 11)**: 0.18 userspace exits = ~0.3 us, i.e. **0.06 %** of the
  create's CPU. This is the completed amortisation note 46 §2 describes, and it
  is why the storm is not exit-bound.

Two consequences worth stating plainly, because they bound everything in §4:

1. The *CPU* share is small but the *wall* share is not: for a depth-1 caller
   the two ISR exits sit on the critical path between "reply published" and
   "request re-issued", which is why deleting them can move the iteration by
   several percent while barely moving the CPU ledger.
2. 69k requests x 5-10 us = **0.35-0.7 s of the 8.2 s iteration**. That is the
   entire prize.

## 4. The options, ranked by (saving x confidence) / effort

| # | option | exits removed | saving at depth 1 | confidence | effort | rank |
|---|---|---|---|---|---|---|
| 1 | ioeventfd the `INTERRUPT_ACK` write | 1 of 2 userspace | ~1.8-4 us (~1.5-3 % of wall) | high | ~15 lines, host-only | **best** |
| 2 | suppress the kick, poll after publishing | the kick | ~1-2 us + one fs-worker wake | medium | ~30 lines in `worker.rs`, burns host CPU | second |
| 3 | `KVM_CAP_IOEVENTFD_ANY_LENGTH` / no datamatch | 0 (cheaper exit) | sub-us | medium | tiny, needs a raw ioctl | third |
| 4 | MSI-X over virtio-pci | 2 of 2 userspace | 3.6-8 us, the whole ISR pair | high on mechanism, medium on delivery | a transport project | last |
| 5 | `VIRTIO_F_EVENT_IDX` | 0 | 0 | high | none - already shipped | not an option |
| 6 | hold replies, one interrupt per N | 0 at depth 1 | **negative** (adds a round trip) | high | low | reject |

### 4.1 Rank 1: absorb the ACK write (host-only, ~15 lines)

The ack is a *write*, and a write can be ioeventfd-registered; the read cannot.
Registering `mmio_base + 0x64` with `datamatch` 1 and 3 (the values
`vm_interrupt` actually writes: `VIRTIO_MMIO_INT_VRING` = 1, `| CONFIG` = 3)
deletes one of the two userspace exits per interrupt, with no guest change and
no fork logic beyond the registration next to the existing one
(`kvm/mmio.rs:137-147`).

What is lost when `on_ack()` (`virtio/mmio.rs:149-157`) never runs:

- **The line de-assertion.** On x86_64/KVM that is already a no-op:
  `clear_irq`'s default is `Ok(())` (`deps/libkrun/src/devices/src/legacy/irqchip.rs:129-131`)
  and `KvmIoapic` implements only `set_irq` (`kvmioapic.rs:60-69`). The fork's
  own comment says so: "De-asserting the line is a no-op for irqchip backends
  that deliver interrupts as one-shot pulses through `register_irqfd()`"
  (`virtio/mmio.rs:141-148`).
- **The status bits.** They become sticky: `read 0x60` keeps returning
  `VIRTIO_MMIO_INT_VRING`, so the guest's handler always believes a vring
  interrupt is pending. That is benign: `vring_interrupt` re-checks the used
  ring and returns `IRQ_NONE` when there is nothing (`virtio_ring.c:3272-3299`),
  and every interrupt this device raises does have work behind it. It costs one
  extra `more_used()` read per interrupt, in guest memory.

Risk to check before shipping: the fork's own comment (`virtio/mmio.rs:141-148`)
says the level *does* matter for riscv64's `KvmAia`, so gate the registration to
x86_64/KVM where `KvmIoapic` (`kvmioapic.rs:51-69`) is the only irqchip and
`clear_irq` is the trait default (`irqchip.rs:129-131`). Whether the aarch64 GIC
chip overrides `clear_irq` was not checked here.

Confirmation is cheap with the existing harness: the guest's `/proc/interrupts`
count must be unchanged while the iteration wall time moves by ~1-3 %, and
`CANG_FS_STATS=1`'s opcode histogram must be identical.

### 4.2 Rank 2: make the kick disappear for back-to-back requests

The kick is the last per-request exit, and it is removable in principle: the
device may publish an `avail_event` *ahead* of `next_avail` (suppressing the
next K-1 kicks - `vring_need_event` then evaluates false, `virtio_ring.h:226`)
provided it takes responsibility for noticing the new buffers itself. It can,
because at the moment it publishes a completion the dispatcher thread is already
awake in `handle_completions()` (`worker.rs:586-608`): after publishing, re-check
`queue.len()` and drain again instead of returning to `epoll_wait`, for a
bounded window (N polls or T us), and re-arm `avail_event = next_avail` before
going back to sleep.

Saving: the kick exit (~1-2 us) plus one eventfd signal and one fs-worker wake
per request. Cost: host CPU in the poll window, and the first request after an
idle gap still kicks. Confidence is only medium because the compile-iteration
gap between a reply and the next request is tens of microseconds - which is what
the poll window has to cover to catch it, i.e. this is a spin, and note 44 §3
already shows the dispatcher at 40 % of a core. Bound the window hard and A/B it
before believing it.

### 4.3 Rank 3: a cheaper ioeventfd registration

`KVM_CAP_IOEVENTFD_ANY_LENGTH` exists for exactly this:

> "With KVM_CAP_IOEVENTFD_ANY_LENGTH, a zero length ioeventfd is allowed, and
> the kernel will ignore the length of guest write and may get a faster vmexit."
> - `Documentation/virt/kvm/api.rst:2134-2137`

The fork registers with a datamatch (`kvm/mmio.rs:142`), which forces a length
check and a value compare per kick. Registering one eventfd for the whole notify
register, without datamatch, would also collapse the "one eventfd per queue" into
one - the worker drains both queues from one wake anyway (`worker.rs:507-512`).
Micro-optimisation: worth folding into rank 2's change, not worth its own build.

### 4.4 Rank 4: MSI-X over virtio-pci, the only complete fix

MSI-X would delete both ISR exits at once, because a vector-delivered interrupt
has no ISR register to read: `vm_interrupt`'s `readl`/`writel` pair exists only
for virtio-mmio (`virtio_mmio.c:293-294`).

Why it is last despite being the largest saving:

- **The fork's PCI transport exists but has no MSI-X.** It advertises
  `VIRTIO_MSI_NO_VECTOR` for both the config vector and the per-queue vector -
  i.e. "not available":
  ```rust
  684             (common_cfg::MSIX_CONFIG, WORD_SIZE) => {
  685                 data.copy_from_slice(&VIRTIO_MSI_NO_VECTOR.to_le_bytes());
  ```
  (`deps/libkrun/src/devices/src/virtio/pci.rs:684-687`, and
  `:706-708` for `QUEUE_MSIX_VECTOR`), and its interrupt path still reads the
  legacy ISR region (`pci.rs:615`). So today a virtio-pci device in this fork
  would keep the ISR read and gain nothing.
- **cang does not use the PCI manager.** Every device, the share included, goes
  to `MmioDeviceManager` (`crates/cang-libkrun/src/linked.rs:28`, `:50`,
  `:306`, `:144-148`); the PCI path (`device_builders.rs:496-517`,
  `kvm/pci.rs:102-139`) is reachable but unused by cang, and it wires INTx, not
  MSI-X.
- **The rest of the work is not transport-local**: an MSI-X table and PBA in a
  BAR, `KVM_SET_GSI_ROUTING` MSI entries, and a vector per queue, plus the
  guest's `virtio_pci` MSI-X enable (`virtio_pci_common.c`) and ACPI/ECAM work
  that already exists (`kvm/pci.rs:141-148`).

Effort is measured in weeks; the prize is 3.6-8 us of a 120 us wall.

### 4.5 Not options: EVENT_IDX (shipped) and completion batching (already banked)

`VIRTIO_F_EVENT_IDX` is negotiated (§2.3), and it buys nothing more at depth 1:
`vring_need_event` is true for a one-deep caller by construction (§1.3). Adding a
deliberate coalescing window on the *reply* side (publish, wait T, interrupt
once) trades ~2 us of exit for a T-long stall on every synchronous request - at
depth 1 that is a 100 us-scale loss to buy a 2 us win. Reject.

## 5. The floor, stated plainly

- **Nothing on today's transport can go below two exits.** The ISR read is an
  MMIO *read*, and an ioeventfd only matches writes (`api.rst:2102-2104`), so
  virtio-mmio cannot absorb it; the handler must also acknowledge, which *is* a
  write and so is absorbable (§4.1). The interrupt itself is already free: an
  irqfd injection is host-to-guest with no exit (`api.rst:3187-3193`).
- **The absolute floor is 0 exits, but only in steady state and only with both
  transport changes.** With MSI-X (no ISR register, `virtio_mmio.c:293-294` is
  the only place the pair exists) *and* kick suppression (device polls,
  `virtio_ring.h:226`), a back-to-back request stream needs neither an MMIO
  read, an MMIO ack, nor a kick: only the irqfd, which is not an exit. Entering
  that stream still costs one kick exit, and it costs the host a polling thread.
  The practical floor for the code as it stands is one kernel-handled exit
  (~1-2 us); today's figure is 5-10 us.
- **If every exit were free**: per create 516 us -> ~506-511 us of CPU, the
  ~120 us wall -> ~112-115 us, and the 8.2 s iteration -> **~7.6-7.9 s**. The
  gap to tmpfs (1.3-2.1 s, note 45 §1) is untouched, because ~110 us per request
  of guest-kernel and host-server work is not exit time (notes 44 §3, 46 §1).
- **No exit-side change can substitute for removing requests.** The
  request-removing levers already shipped (negative entries: note 45 §5) are
  worth 33 %, i.e. 20x anything in §4.

## 6. What would falsify this

- **N in the compile-iteration shape.** §3's depth-1 arithmetic assumes the
  iteration runs with ~1 request in flight. The harness can settle it: guest
  `/proc/interrupts` delta for the virtio-fs line across a marker-bracketed
  no-op iteration, divided by the request count from `CANG_FS_STATS=1`. If N is
  5-10 rather than 1, the userspace-exit term is 5-10x smaller and rank 1 is worth
  ~0.3 % instead of ~2 %.
- **The per-exit cost on this host.** 1.81 us is bare metal elsewhere; the host
  here is a KVM guest. A guest-side loop that touches a device register (e.g. a
  probe mode that reads `INTERRUPT_STATUS` in a tight loop) would give the local
  number for one boot, and would re-price §3 in one direction or the other.
- **The sticky-status risk in §4.1.** One boot with the registration in place
  answers it: `dmesg` for spurious-IRQ reports, `/proc/interrupts` for the count,
  and the iteration wall time for the saving.

## Sources

| what | where |
|---|---|
| fork, read at `18e0e37` (tag `v2.0.0-cang.17`) | `src/devices/src/virtio/{fs/{worker.rs,device.rs,mod.rs},queue.rs,mmio.rs,device.rs,pci.rs,mod.rs}`, `src/devices/src/bus.rs`, `src/devices/src/legacy/{kvmioapic.rs,irqchip.rs}`, `src/libkrun/src/vmm/device_manager/kvm/{mmio.rs,pci.rs}`, `src/libkrun/src/vmm/linux/vstate.rs`, `src/libkrun/src/api/device_builders.rs`, `src/libkrun/src/vmm/builder.rs` |
| cang's device wiring | `crates/cang-libkrun/src/linked.rs` (`MmioDeviceManager`, `Slot::FsDevice`) |
| guest kernel, `v7.2` | `drivers/virtio/virtio_mmio.c`, `drivers/virtio/virtio_ring.c`, `include/uapi/linux/virtio_ring.h`, `fs/fuse/virtio_fs.c` - `https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/<path>?h=v7.2` |
| pinned kernel identity | `deps/libkrunfw/Makefile:1` (`KERNEL_VERSION = linux-7.2.10`), `:5` (linux-hardened `v7.2.10-hardened1`) |
| KVM API doc | `Documentation/virt/kvm/api.rst?h=v7.2` (`:2102-2104`, `:2128-2129`, `:2134-2137`, `:3187-3193`) |
| per-exit prices, and the three-exit count this note re-derives | note 22 §1.3 (+ its sources table: microkvm `docs/en/benchmark.md`, `Documentation/virt/kvm/halt-polling.rst:14-21`) |
| replies per interrupt, request rates | notes 44 §5, 46 §2 |
| the 516 us split, ~120 us wall per request, 69k requests / 8.2 s | note 46 §1, note 45 §2 |
| `halt_poll_ns=200000` on this host (exit/wake latencies are kind, host CPU is not) | note 20 §5, note 22 §1.3 |
| tmpfs comparison for the iteration | note 45 §1 |
| the earlier MSI-X ranking this note re-prices (2 exits of 3, "bad ratio") | note 22 §2 rank 9, `notes/34-lookup-per-request-latency-levers.md:351` |
