# 46 - Where the per-request cost sits, and the recommendation that follows

**Measured 2026-11-04**, same tree and guest as notes 44/45. This note exists to
answer one question before anyone spends a fork or kernel cycle: *if the share
costs ~110 us per request, which component owns it?* Answer: **no single one**,
so no single change buys the 4x the compile iteration needs.

## 1. The split, from the storm's own bracketed window

The 20.08 s / 156 680-create storm of note 44 §3, with per-thread CPU snapshotted
across guest-written begin/end markers, attributed (all figures host CPU, which
includes the vCPUs' own time):

| component | CPU-s | us per create | share |
|---|---|---|---|
| guest vCPUs (`fc_vcpu` x26) | 38.9 | 248 | **48 %** |
| fs dispatcher thread | 8.1 | 52 | 10 % |
| fs pool (4 threads) | 14.0 | 90 | 17 % |
| other VMM threads (main loop, vsock, block, net) | 19.8 | 126 | 24 % |
| **total** | **80.9** | **516** | |

With 4.0x parallelism (80.9 CPU-s over a 20.08 s wall) that yields 7 803
creates/s. The same arithmetic holds for the compile iteration shape: ~69k
requests in 8.2 s at ~8.5k requests/s, i.e. ~120 us of wall per request with
about one effective stream.

Two readings:

- **The cost is two-sided and even**: guest kernel + guest userspace (48 %) versus
  the host FUSE server (27 %: dispatcher + pool) with the rest in VMM plumbing.
  Halving *either* side moves throughput by at most ~1.3-1.5x; halving both
  reaches 2x. There is no 4x in per-request work.
- **The dispatcher is 10 %**, not a bottleneck; the pool is 17 %; together they
  are a quarter of the cost of serving a request, so "make the server faster"
  cannot be the plan either.

## 2. What the interrupt counter says about exits

In a separate 15.0 s window (7 232 creates/s, 108 480 creates) the fs request
queue took **9 756 interrupts = 650/s = ~11 creates per interrupt**, on top of the
kick that starts each batch. Completion signalling is therefore already
amortised ~11-40x (note 44 §5 measured 43 replies per notification in a denser
window), so the *exit* count is not proportional to the request count: at most
0.09-0.25 exits per request on the completion side, plus one 4-byte MMIO kick per
batch. Cutting those exits entirely can remove a few percent, not the 516 us.

## 3. Recommendation

Ranked by expected value per unit of effort, given the split above:

1. **Decide whether 8.2 s is acceptable** for a compile-test iteration on a live
   host tree (against 1.45 s on the host, 1.3-2.1 s on guest tmpfs). If it is,
   this thread of the map is finished: negative entries at 30 s is shipped, and
   nothing else on the share moves an iteration by more than a few percent.
2. **If it is not acceptable**, the next artifact is not a FUSE change but
   [ticket 18](tickets/18-workspace-target-on-block.md) (a block device carrying
   `target/`): removing the round trips is the only route to tmpfs numbers, and it
   is a design decision to grill, not a tweak to implement.
3. **Only if a fork/kernel cycle is explicitly wanted**, in this order: (a)
   virtio-pci + MSI-X for the share, which removes the completion-side
   `INTERRUPT_STATUS` read and `INTERRUPT_ACK` write (note 22) and is A/B-able
   with today's harness in a day; (b) a libkrunfw patch to run
   `virtio_fs_requests_done` inline instead of via the workqueue; (c) raising
   `max_background`, which gates only asynchronous requests and so is unlikely to
   show anything on cargo's synchronous stats. Expected ceiling for all three
   together: ~1.5-2x on metadata throughput, i.e. ~8.2 s -> ~5 s.

## 4. A measurement that failed, recorded so it is not repeated

An attempt to split the cost *inside* the guest (guest `/proc/stat` deltas across
a marker-bracketed storm) produced 451 CPU-seconds for a 15 s window on a 26-vCPU
guest — 30 cores, more than the guest has. The guest's `/proc/stat` tick
accounting is not usable for this; the host-side per-thread attribution above is
the trustworthy one, and reproducing it needs the marker-bracketed sampler
(`/home/dev/cang-bench/split-runner.sh` brackets the window; its thread discovery
must match on the `fs worker` thread, not on the command line, or it samples a
shell).
