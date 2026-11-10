---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Can O_DIRECT reach the cang guest? (it already does, and the host-side knob is a no-op)

**Answer.** Yes - a guest payload can open anything with `O_DIRECT` today, nothing in
cang blocks it, and on the `/nix` lower it is **13-20x faster than buffered** on the
same bytes. That fact is the most useful measurement in this effort, and it is *not*
a fix: `/nix` is consumed by `execve`, `mmap` of `.so`s, compiler and browser mmaps,
and `mmap` is incompatible with `O_DIRECT`; forcing it would also throw away the
guest page cache, which is what makes a store's re-reads free. The host-side
equivalent - libkrun's `BlockDevice::set_direct_io`, which cang did not wire - was
implemented and measured: **no effect**.

## What was built

`crates/cang-libkrun/src/api.rs` + `linked.rs`: `block_device_set_direct_io(handle,
bool)` over libkrun's existing `BlockDevice::set_direct_io` (which reaches imago's
`.direct()` and therefore `O_DIRECT` on the host image). Launcher: applies it to
read-only disks only, behind `CANG_BLOCK_DIRECT_IO`, so the default boot path is
untouched. Built as `/nix/store/lx1p66v0d69s5ckwmqqlvjln3ylx30j3-cang-0.21.0`
(that binary contains the env name, the pinned one does not - `grep -a -c`).

## The controlled measurement (host cache dropped per arm, by the host)

The control this campaign needed ([note 06](06-read-wall-attribution.md) said it was
missing): a host-side loop watches `/workspace/evidence/want`, runs
`posix_fadvise(DONTNEED)` over the *entire* `.raw` before releasing the guest arm,
and samples `/proc/diskstats` around it. Every arm below read **0.65-0.66 GB from
`sdb`** - i.e. the host cache really was cold, and both filesystems moved identical
bytes.

609 MiB store file, guest `dd`, 4 boots:

| arm | cang build | guest ms | MB/s |
|---|---|---|---|
| buffered 1 MiB | `CANG_BLOCK_DIRECT_IO=1` | 7391 / 10106 / 12326 / 6422 | 52-99 |
| buffered 1 MiB | control (default) | 12413 / 6982 / 7480 / 7831 | 52-92 |
| buffered 1 MiB | shipped pin | 8086 / 7627 / 8130 | 74-79 |
| **`O_DIRECT` 1 MiB** | any | **515 / 530 / 897 / 732 / 621 / 415 / 364 / 494** | **712-1756** |
| buffered 32 / 128 / 512 KiB | shipped | 18651 / 9105 / 10069 | 34-70 |
| `O_DIRECT` 32 KiB | shipped | 3961 | 161 |

So: host `O_DIRECT` changes nothing (contra the host-cache story), guest `O_DIRECT`
changes everything, at every block size, in every boot, on both kernel lines
(6.12.115 and 7.3.1) and both firmwares. Guest CPU is 0.3-1.8 s of an 8 s arm, so it
is not CPU either.

## What that leaves

The guest's **page-cache fill path for this device** is 8-20x slower than the same
number of bytes through `O_DIRECT`. Per-request numbers from the shipped boots:
buffered issues ~235 x 2.6 MiB requests at ~34 ms each, `O_DIRECT` ~1300 x 0.5 MiB at
~0.4 ms each; `read_ahead_kb` 0/128/8192/65536 moves buffered only between 4.6 and
8.1 s, and `max_sectors_kb` 512/128 makes it worse. The leading suspect is therefore
the one thing this fork has already had a bug in at exactly that boundary
(memory #334): a large guest buffer that spans two guest memory regions is split
per region in `descriptor_utils.rs::buffer_region_slices`, and a 2.6 MiB buffered
descriptor crosses region edges far more often than a 0.5 MiB one. That is a
hypothesis with a specific, cheap test: instrument the split path (slice count and
bytes per descriptor) for a buffered vs an `O_DIRECT` arm and compare.

**Evidence.** `/home/dev/disp/blkread/run19-*` (first controlled boot),
`run20-*` (readahead arms), `run21-directio-*` / `run22-control2-*` (the
`CANG_BLOCK_DIRECT_IO` A/B), `run23-bs-*` (block-size sweep); each carries
`workspace/evidence/phases.txt`, `logs/host-arms.log.{before,after}` (the host's own
device sectors per arm) and `logs/erofs.rc`. Probes `probe18.sh`/`probe19.sh`/
`probe20.sh`, host control `control.sh`, booters `run19.sh`-`run23.sh`.

## The mechanism, found by witnessing the host during each arm

The host-side control was extended to sample `meminfo`, `vmstat`, the
kcompactd/kswapd tick counters and the VM worker's RSS before and after every guest
arm (`control2.sh`; boot `run24-witness-*`). One boot, 7 arms, host cache dropped
per arm, 0.64-0.67 GB read from `sdb` in every arm:

| arm | ms | MemFree Δ | Cached Δ | **AnonPages Δ** | pgalloc Δ | **compact_stall** | kcompactd ticks |
|---|---|---|---|---|---|---|---|
| buffered-1 | 9644 | -1134 MiB | +574 | **+509 MiB** | 1.07 M | **117** | 78 |
| direct-1 | 476 | -72 MiB | +620 | **-512 MiB** | 0.78 M | 8 | 39 |
| buffered-2 | 7798 | -704 | +585 | **+155** | 0.95 M | **51** | 59 |
| direct-2 | 583 | -475 | +623 | **-142** | 0.78 M | 14 | 21 |
| nocache-1 | 823 | -720 | +615 | +60 | 0.77 M | 6 | 5 |
| buffered-3 | 8208 | -1118 | +557 | **+533** | 1.05 M | **74** | 2 |
| direct-3 | 538 | -133 | +622 | **-473** | 0.75 M | 6 | 62 |

**A buffered arm makes the host allocate ~150-530 MiB of anonymous memory and
compact 51-117 times; an `O_DIRECT` arm allocates none (AnonPages falls) and
compacts 6-14 times.** *(Correlation only - the causal test in the next section
failed.)* That is what the 8-20x is: a guest page-cache fill first-touches
guest physical pages, and each first touch is a host anonymous-page allocation on a
host that is 11.5 GB deep in page cache for a 7.9 GiB image (3.3 GB free, MemTotal
16.9 GB). `O_DIRECT` reuses the reader's already-mapped buffer pages, so it never
asks the host for memory. Nothing anywhere in the fork sets `MAP_POPULATE`,
`mlock` or `MADV_HUGEPAGE` on the guest RAM region, so every guest page the guest
kernel hands out is faulted in lazily by the host at first touch.

That also explains two earlier results that had no explanation: buffered reads do
not scale with parallel readers (59 s at 8 readers against 39 s at 1 - the host's
reclaim/compaction serialises), and the same arm varies 25-37 s between boots (the
host's free-memory state at that moment).

### The causal test of that mechanism: failed (boot 25)

The guest was asked to allocate and free ~1 GiB of its own memory (a 1 200 MiB
`/dev/shm` write, then `rm`) before reading, so that its page cache would have
already-host-allocated pages to reuse instead of asking the host mid-read. The host
witness confirms the allocation happened and stuck (AnonPages +1 049 MiB, MemFree
-951 MiB on a 12.8 s pre-touch arm) - and the following buffered arms were **not
faster**: 13 902 ms, then 6 289 ms, i.e. the same 6-14 s spread as every other boot,
while `O_DIRECT` stayed at 518-621 ms. The buffered arms still allocated a further
+434 / +449 MiB of host anonymous memory *after* 1 GiB had just been freed by the
guest.

So "the host has to allocate the guest's pages during the fill" is **not** the
mechanism, and pre-faulting the guest RAM is **not** the fix - retracted. What the
witness does still say, and what remains unexplained: buffered arms correlate with
host allocation and compaction, `O_DIRECT` arms never do, and buffered reads do not
scale with parallel readers while `O_DIRECT` reaches 3.4 GB/s.

### What is still on the table

Candidates, each with the measurement that would decide it - none run yet:

1. **Per-request latency in the guest's readahead submission.** Buffered issues ~235
   requests averaging 2.6 MiB at ~34 ms each; `O_DIRECT` ~1 300 averaging 0.5 MiB at
   ~0.4 ms each, on the same device and filesystem. Instrument the *guest* side
   (`/sys/kernel/debug/tracing` if available in the image, or a bpftrace-free
   alternative: `/proc/diskstats` per arm already shows it) and the **host** side of
   the same requests: libkrun's virtio-blk worker with per-request timestamps would
   say whether the 34 ms is spent before or after the guest hands over the
   descriptor. If it is inside the host, it is the fork's block device; if it is
   inside the guest, it is the kernel's bio/page-cache path.
2. **The multi-region descriptor split** (`descriptor_utils.rs::buffer_region_slices`,
   memory #334): a 2.6 MiB guest buffer crosses region boundaries far more often than a
   0.5 MiB one. Count slices per request for a buffered vs a direct arm - one
   instrumented firmware build, no behavioural change.
3. **`read_ahead_kb` interacts with the request size**: already swept (0/128/8192/
   65536 KiB → 4.6-8.1 s), so it is not a knob, but a *small* `read_ahead_kb` plus
   `O_DIRECT`-like semantics for the store is not achievable from userspace.

**Do not** ship anything from this note except the measurement: the only product-
relevant facts are that packed erofs loses (note 05), the btrfs lower is never
beaten by any alternative tried, and the guest's buffered read path for this device
is 8-20x slower than `O_DIRECT` for reasons not yet established.

## Attribution: the cost is in the guest kernel's read path, not in the device

The witness measured host memory pressure; the decisive question was where the time
goes. Answered with per-request instrumentation in the fork's virtio-blk worker
(`CANG_BLK_TRACE=<path>`, appended by both the device worker and the guest probe so
one file interleaves both sides): every served read logs `len`, `pread_us` (host
read) and `copy_us` (the copy into guest memory, i.e. where guest page faults land).

Boot 28, one 609 MiB store file, 5 alternating arms, host cache dropped per arm:

| arm | requests | MB | avg KiB | med pread | sum pread | med copy | sum copy | host total | wall | **guest-side** |
|---|---|---|---|---|---|---|---|---|---|---|
| buffered-1 | 840 | 761 | 884 | 18 us | 2.92 s | 4 us | 1.95 s | 4.87 s | 10.25 s | **5.38 s** |
| direct-1 | 1353 | 649 | 468 | 8 us | 0.08 s | 2 us | 0.02 s | 0.10 s | 0.35 s | **0.25 s** |
| buffered-2 | 294 | 648 | 2152 | 417 us | 0.12 s | 173 us | 2.27 s | 2.38 s | 9.28 s | **6.90 s** |
| direct-2 | 1358 | 649 | 467 | 9 us | 0.11 s | 2 us | 0.02 s | 0.14 s | 0.38 s | **0.24 s** |
| buffered-3 | 373 | 657 | 1720 | 47 us | 0.11 s | 10 us | 0.79 s | 0.90 s | 6.41 s | **5.51 s** |

- **The stable cost is guest-side: 5.4-6.9 s in every buffered arm against 0.24-0.25 s
  in every `O_DIRECT` arm.** The host's own work is 0.9-4.9 s and wildly variable,
  i.e. it cannot explain a repeatable 6-10 s.
- **The copy into guest memory is the one host-visible cost that tracks the gap**
  (0.79-2.27 s buffered vs 0.02 s direct, median 4-173 us vs 2 us): copying into
  pages the guest kernel just took from its page cache faults them in on the host,
  while `dd`'s `O_DIRECT` buffer is touched every iteration and costs nothing. That
  is the page-fill cost - real, but only a fifth to a third of the gap.
- **The rest is the guest kernel submitting and waiting serially**: with the device
  idle, buffered arms spend 5-7 s outside it, and the request shape explains why -
  buffered issues *fewer, larger* requests (294-840 x 0.9-2.2 MiB) and does not
  overlap them, while `O_DIRECT` issues ~1350 x 0.47 MiB and pipelines them at
  87x lower per-request latency. This matches every earlier observation: buffered
  reads do not scale with parallel readers, `read_ahead_kb` does not help, and
  `max_sectors_kb` in either direction does not help.

**Conclusion for cang.** The cold-read wall for `/nix` is **not** the host's storage,
not the block device, not cang's launch path, and not the firmware: it is the guest
kernel's buffered readahead path for this device, which is a guest-kernel/fork
question (`virtio_blk` request merging and queue depth, not `mkfs`/mount/format
choices). Nothing cang can set at launch closes it, which is why every knob tried -
`read_ahead_kb`, `max_sectors_kb`, filesystem, packing, host `O_DIRECT` - moved
nothing. A payload that controls its own I/O can simply use `O_DIRECT` and is 8-20x
ahead today.

**Tooling left in tree.** `CANG_BLK_TRACE` in `deps/libkrun`'s block worker
(env-gated, no effect unset, no clock reads unless enabled); the build that carries
it is `/nix/store/hnxld8bbw47f38nk2mdc93hwspk7lhd8-cang-0.21.0` via
`nix build .#cang --override-input libkrun-src path:$PWD/deps/libkrun` - the flake
otherwise builds libkrun from its pinned input, so a fork edit is invisible to
`nix build .#cang` without that override.

## The multi-queue test: also refuted

The last mechanical suspect was submission width: the fork advertised **one** virtqueue
(`NUM_QUEUES = 1`), and the guest confirmed one mq directory. Widened to four -
`NUM_QUEUES = 4`, `QUEUE_CONFIG` repeated, one `BlockWorker` per queue, and the
`VIRTIO_BLK_F_MQ` feature bit plus `num_queues = 4` in the device config, without
which a guest ignores the extra virtqueues entirely.

**The arm was verified, not assumed** (`probe24.sh` records it): boot 31's guest shows
`mq_dirs=4`, `nr_tags=128` per queue, cpu_list `0-6 / 7-13 / 14-19 / 20-25`. And the
result is no change:

| arm (4 queues, boot 31) | n | MB | avg KiB | sum pread | sum copy | host | wall | guest-side |
|---|---|---|---|---|---|---|---|---|
| buffered-1 | 661 | 720 | 1063 | 3.01 | 1.87 | 4.88 | 10.12 | **5.24** |
| buffered-2 | 315 | 648 | 2010 | 0.32 | 2.57 | 2.89 | 8.75 | **5.85** |
| buffered-3 | 294 | 648 | 2152 | 0.53 | 0.84 | 1.36 | 6.49 | **5.12** |
| direct-1 | 1359 | 649 | 466 | 0.11 | 0.05 | 0.16 | 0.42 | 0.26 |
| direct-2 | 1359 | 649 | 466 | 0.21 | 0.05 | 0.26 | 0.51 | 0.25 |

Guest-side time stays at 5.1-5.9 s against `O_DIRECT`'s 0.25 s, and the request shape
does not change (fewer, larger requests, not overlapped). A wider device only helps a
guest that wants to overlap; this read path does not. The change was reverted - four
workers each with their own IO pool would cost threads for nothing - and the fork's
diff is back to the trace instrumentation alone.

**So the stack of eliminations is complete.** Host storage, host page cache, image
fragmentation, filesystem (btrfs vs ext4), packing (erofs), request size
(`max_sectors_kb`), readahead (`read_ahead_kb`), queue count, host `O_DIRECT`, and
guest RAM pre-faulting are each measured and each removed. What is left in the number
is the guest kernel's own buffered readahead for a virtio-blk device - a guest-kernel
question (bio/plug/readahead scheduling), not a cang launch setting and not a fork
device bug that these arms expose. The practical answer stands: a payload that issues
its own I/O with `O_DIRECT` is 8-20x ahead of the same payload reading buffered, and
nothing cang sets at launch changes that.

## `MADV_POPULATE_WRITE` on guest RAM: tested, no effect

The earlier retraction used the wrong mechanism: a guest that allocates and frees
1 GiB touches its own pages, which the host may reclaim again. The launch-time
mechanism is `madvise(MADV_POPULATE_WRITE)` over each guest memory region, which
faults the whole `--mem` in once and keeps it - added next to the fork's existing
`advise_hugepages_for_guest_memory()` and gated on `CANG_GUEST_RAM_POPULATE=1`.

**The arm is proven, not assumed** (worker RSS sampled by the host at 2 s intervals,
same `--mem 4`, payload sleeping):

| boot | t=4 s | t=6 s | t=8 s | steady |
|---|---|---|---|---|
| `CANG_GUEST_RAM_POPULATE=1` | 1.68 GiB | 3.21 GiB | **4.01 GiB** | — |
| control | 0.41 GiB | 0.73 GiB | 0.84 GiB | 0.74 GiB |

So the whole guest RAM is resident from boot in the populate arm, exactly as
`MAP_POPULATE` would give.

**Result: no change.** Boot 32, same five arms, host cache dropped per arm:

| arm | reqs | MB | Σ pread | Σ copy | host | wall | guest-side |
|---|---|---|---|---|---|---|---|
| buffered-1 | 284 | 646 | 0.49 | 4.59 | 5.08 | 11.56 | **6.47** |
| buffered-2 | 295 | 648 | 0.13 | 1.89 | 2.03 | 9.84 | **7.81** |
| buffered-3 | 370 | 653 | 0.22 | 1.16 | 1.39 | 5.39 | **4.00** |
| direct-1 | 1361 | 649 | 0.14 | 0.06 | 0.19 | 0.48 | 0.29 |
| direct-2 | 1375 | 649 | 0.14 | 0.03 | 0.17 | 0.41 | 0.25 |

The copy-into-guest-memory cost does not fall either (1.16-4.59 s against 0.79-2.57 s
before), which corrects the reading of that number: it is not host page-fault cost on
guest RAM - that would have vanished with the RAM pre-populated - but the guest
kernel's own page allocation and copy inside its page-cache fill.

**Both halves of the hypothesis are now closed by measurement:** populating guest RAM
at launch (this section) and pre-touching it from the guest (above) change nothing,
because the time is spent inside the guest kernel, not in the host's fault path.
