---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Concurrent `/nix` reads: no thread pool is 15-30 % faster at 1-4 readers; host O_DIRECT helps
# only at depth 1 and loses at depth 7, because btrfs silently buffers compressed extents

Two host-side toggles on the block path, both already in libkrun's API (no fork hunk), both
env-gated for the experiment and reverted afterwards
(`blkread/blk-serial-direct-toggles.patch`): `set_parallel_reads(false)` =
"CANG_BLOCK_SERIAL_READS", `set_direct_io(true)` = "CANG_BLOCK_DIRECT_IO", applied in
`crates/cang-libkrun/src/linked.rs::block_device_new` so *every* block device - the `/nix` lower
(`/dev/vdc`) and the build disk carrying the upper (`/dev/vdb`) - gets the same treatment.

## 1. Method

One boot per mode, `--nix=lower:blk --nix=upper:blk`, 6 GiB guest, 26 vCPUs, gates passed
(`MemAvailable` >= 6 GiB, `AnonPages` <= 8 GiB). Each boot: 7 slices of 128 MiB from files under
`/nix/store` (the overlay a command actually reads), arms at 1, 8, 4, 2, 1, 8 readers, guest
`drop_caches` plus a mincore-proved host drop of the image before *every* arm. Guest side stays
the workload's: buffered `pread` into 1 MiB buffers (`readbench`), reporting per-reader p50/p95.
n = 4 boots' worth of arms at depth 1 and 7, n = 2 at depths 2 and 4; one reader always reports
0 at depth "8" because the manifest held 7 slices, so that arm is depth 7.

## 2. Throughput (aggregate MB/s, median of the arms)

| host mode | 1 reader | 2 | 4 | 7 | all samples (depth 1) |
|---|---|---|---|---|---|
| pool + buffered (today's default) | 1 824 | 2 261 | 2 005 | 1 882 | 655, 1 688, 1 824, 1 842 |
| pool + O_DIRECT | 1 917 | 2 142 | 1 911 | 2 035 | 1 726, 1 776, 1 917, 2 043 |
| serial (no pool) + buffered | 2 087 | **2 681** | **2 541** | 1 975 | 2 018, 2 023, 2 087, 2 429 |
| serial + O_DIRECT | **3 006** | 2 614 | 2 164 | 1 694 | 2 792, 2 817, 3 006, 3 154 |

Latency follows the same ordering at depth 1 (p50 69/59/51/39 us for pool / pool+direct /
serial / serial+direct) and inverts at depth 7 (p95 max 23-44 ms in every mode, worst in the
serial arms: one reader waits behind the others when there is no pool).

So: **removing the IO pool is worth 15-30 % at 1-4 readers**, and adding host O_DIRECT on top is
worth another ~45 % at exactly one reader (3 006 against 1 824, 1.6x) but **loses ~10 % at depth
7**. That matches the mechanism: the pool costs a bounce-buffer copy per request, and a warm host
page cache absorbs a burst that direct I/O cannot.

## 3. The O_DIRECT result is partly an illusion, and the reason matters

The lower image is zstd-compressed (`--reflink` build of a `compress=zstd:1` mount), and **btrfs
falls back to buffered I/O for compressed extents**. Measured on the host against the same image:

```
cacheproof.py <image> --drop      -> 0 of 3 026 176 pages resident
dd if=<image> of=/dev/null bs=1M count=64 iflag=direct
cacheproof.py <image>            -> 12 608 pages (49 MB) resident
```

So `O_DIRECT` was only partly honoured: 49 MB of a 64 MiB direct read still landed in the host
page cache. That is why the direct arms gain so little at depth and why the single-reader figure
should be read as "partly cached", not as a cache-bypassing measurement. An uncompressed image
would show the true O_DIRECT effect; this experiment did not have one (that arm was reverted in
note 20 for other reasons).

## 4. What this changes

- The pool is a **per-request copy plus a lost cache**, not a throughput win, at the depths that
  matter for `/nix` metadata and small payload reads - consistent with note 09's single-reader
  1.5x for a serial prototype and with note 28's thread analysis.
- The default should not change on this evidence alone: the serial arms lose at depth 7 in these
  same boots (1 694-1 975 against 1 882-2 035), so the pool still pays for many-reader bursts.
  A depth-adaptive policy, or sizing the pool to the guest's queue depth, is the shape the data
  suggests - not a flat "no pool".
- Absolute numbers here are 10x the 144 MB/s in note 11 for the same file: that note was measured
  on a host under memory pressure (note 13), and this boot's gates recorded 14.8 GB available.

## Evidence

Nine boots under `blkread/run-io-s<serial>d<direct>-*`; per-arm JSON in each
`workspace/evidence/arms.jsonl` (per-reader MB, ms, p50/p95/max), `phases.txt` with device
counters, `setup.txt` with the `/nix` mount line and the `stat -f` reading. Toggle patch kept at
`blkread/blk-serial-direct-toggles.patch`; the cang tree is reverted and its tests pass.
Caveat recorded in the runs: `/nix`'s mount identity was captured at probe start, so
`mountinfo_nix` is empty in the boots where the overlay had not been composed yet - the reads
still came from `/nix/store`, and the composed line is present in four of the nine.
