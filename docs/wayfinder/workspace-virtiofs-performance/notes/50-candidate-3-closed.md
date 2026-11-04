# 50 - Candidate 3 closed: three research passes, one verdict

**2026-11-04.** Note 46 recommended three fork/kernel candidates if a cycle were
ever spent on the per-request cost, with a guessed ~1.5-2x ceiling. Three
research passes read the fork, the pinned guest kernel and the transport paths
from source. All three candidates are closed, the guesses were wrong in one
direction (too optimistic) and one earlier claim in these notes was wrong in the
other (see §4). This note is the reconciliation; the detail lives in notes
[47](47-virtio-pci-msix-feasibility.md), [48](48-guest-kernel-completion-and-concurrency.md)
and [49](49-exit-budget-per-round-trip.md).

## 1. The exit budget is not where the cost is

Note 22 counted "three VM exits per round trip" and note 46 repeated it. From
source ([note 49](49-exit-budget-per-round-trip.md)):

- the **kick** is a 4-byte write to the queue's notify address, which is an
  ioeventfd matched on the queue index (`kvm/mmio.rs:137-144`) - KVM absorbs it,
  **zero userspace exits**, and the worker sleeps in `epoll_wait` on the eventfd;
- the **completion** costs **two** userspace exits (guest `readl(INTERRUPT_STATUS)`
  and `writel(status, INTERRUPT_ACK)`, `virtio_mmio.c:293-294`), and only once per
  interrupt because `EVENT_IDX` is negotiated (`fs/device.rs:78`) - measured at
  11-43 replies per interrupt. Injection itself is an irqfd and is free.
- So the exit cost is **2 exits per 11-43 replies**, i.e. ~1 % of an iteration, at
  ~1.81 us per exit.

**The floor if every exit were free is ~7.6-7.9 s against the current 8.2 s.**

## 2. MSI-X: real but not worth 900 lines

[Note 47](47-virtio-pci-msix-feasibility.md): the fork *has* a modern virtio-pci
transport with a KVM manager and an end-to-end `FsDevice`-on-PCI test, but **no
MSI-X** (`MSI_CONFIG`/`QUEUE_MSIX_VECTOR` read `VIRTIO_MSI_NO_VECTOR`, no MSI-X
capability), and **no notify ioeventfd** on the PCI path. cang only ever builds an
`MmioDeviceManager`. PCI mode also requires ACPI and refuses shm devices, so
`--gpu=drm` blocks a wholesale switch.

MSI-X would remove exactly the two ISR accesses - and save **0.06-0.17 % of the
516 us per create, 0.3-0.75 % of an 8.2 s iteration**, for ~700-900 lines across
~13 files. Not a candidate.

## 3. The kernel has nothing to patch

[Note 48](48-guest-kernel-completion-and-concurrency.md): the functions note 46
named (`virtio_fs_requests_done`, `virtio_fs_request_ready`) **do not exist** in
the pinned stable v7.2.10. The path is `virtio_fs_vq_done` (hardirq) ->
`schedule_work(system_percpu_wq)` -> `virtio_fs_requests_done_work`, which drains
every used buffer in **one** scan, and **non-blocking replies are already
completed inline**; only `may_block` replies are deferred. So the workqueue hop is
per *batch*, not per request, and hoisting it saves ~0.1 % of the guest's 48 %
share. Submissions are already kick-suppressed by `EVENT_IDX`.

## 4. A correction this pass forced

`max_background` is **not** the kernel default of 12 that notes 44 §6, 45 §6 and
46 §3(c) claimed: libkrun's FUSE INIT reply sets `max_background = u16::MAX` and
`congestion_threshold = 49151` (`fs/server.rs:1110-1111`), and the kernel adopts
them on a privileged init mount. Note 01 had it right; the later notes inherited
a wrong inference. Either way the knob gates only *async* allocations, so every
request in a compile iteration bypasses it. Notes 44/45/46 and ticket 21 are
corrected in place.

## 5. The one thing worth spending on the kernel

The pinned `-kvm` config has `# CONFIG_FTRACE is not set` and
`# CONFIG_DEBUG_FS is not set` (`deps/libkrunfw/config-*-kvm`), which is why note
22 had to *bound* the 55 us of guest cost per request rather than measure it.
Enabling `CONFIG_FTRACE` (+ `FUNCTION_TRACER`, `DYNAMIC_FTRACE`) in that config is
a small, local, non-shipping change that makes the guest half measurable - local
builds are non-LTO with sccache and need no cang or image rebuild
(`nix build .#cang-dev --override-input libkrunfw-src …` +
`CANG_LIBKRUNFW_LIBRARY`). Worth doing before any future request-cost work; not
worth doing for its own sake.

## 6. Verdict

There is **no fork or kernel change on this axis worth building**. The share's
compile-iteration cost (8.2 s against 1.45 s on the host) is split 48 % guest /
27 % FUSE server / 24 % VMM plumbing, and every component is already at its
cheapest setting that these notes could find. The remaining 4x is not in the data
path's efficiency - it is in **not making the round trips**, i.e. ticket 18
(`target/` on a block device) or accepting a guest-local tree. That decision is
bob's, and it is the map's only open direction.
