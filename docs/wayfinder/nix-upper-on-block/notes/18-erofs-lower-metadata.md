---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# (#1) An erofs lower is 1.5-1.8x cheaper than btrfs for the cold metadata walk - and lz4 gives it back

Same guest, same 3897-file `find <store> -maxdepth 3 -type f`, four repeats each, guest
page cache dropped before every arm, host dropping the `nix-lower` image and the erofs
files (mincore-proved) before releasing each arm. The erofs images are `mkfs.erofs 1.9`
builds of the image's own `/nix/store` (686 paths): `plain` 8.4 GB uncompressed,
`lz4c64` 4.2 GB (`-zlz4 -C 65536`).

| repeat | btrfs lower (ms) | erofs plain (ms) | erofs lz4c64 (ms) |
|---|---|---|---|
| 1 | 447 | 482 | 742 |
| 2 | 373 | 241 | 440 |
| 3 | 362 | 250 | 425 |
| 4 | 392 | 240 | 416 |
| **typical** | **362-447** | **240-250** | **416-440** |

- **Uncompressed erofs: ~1.5-1.8x faster** than the btrfs lower for the identical walk.
- **lz4 erofs: no better than btrfs, and clearly worse than plain erofs** (416-440 vs
  240-250) - the same compression penalty note [17](17-per-read-cost-is-fs-compression.md)
  measured on btrfs (2.0-2.8x per small cold read), reproducing inside a read-only image
  format. So the two notes agree: the metadata walk wants *uncompressed* metadata.
- First-arm outliers in both erofs columns (482, 742) are the loop file's own first cold
  pass; repeats 2-4 are the stable numbers, and the btrfs arms show no such trend.

## Why the erofs result is a lower bound on the gain

The erofs images sat as files on the guest's own root filesystem and were read through a
`loop` device: every erofs metadata read therefore pays `loop -> guest page cache -> the
root fs -> virtio-blk`, and that backing file is itself on the host's
`compress=zstd:1` state root. A real erofs lower mounted directly off a virtio-blk device
would skip both layers - so 240 ms is a *pessimistic* erofs number, not a fair one.

## What banking it needs (a cang change, not a knob)

1. build the lower as erofs instead of btrfs (`mkfs.erofs -T 0`, no compression), and
2. teach the guest half to mount that image read-only as the overlay lower - the same
   place `mount_block_lower` composes the btrfs lower today, with the marker check that
   already gates `guest_init_mounts_block_lower`.

## Limitations of this run, recorded

- **No device counters**: the probe derived the root device from `/proc/mounts`, which
  reports `root` rather than `/dev/vdcN` for this guest, so `before=[]`/`after=[]` is
  empty in every arm. The read-*count* evidence for the btrfs walk (941 reads) comes from
  note [15](15-cold-metadata-is-device-reads.md); this run measures time only.
- No warm arms, so the cold/warm split is taken from earlier boots rather than repeated
  here.
- Four repeats per arm, single boot, 6 GiB guest.

## Evidence

`run-erofs-20261115T134510Z` (`workspace/evidence/meta.txt` per arm, `setup.txt`,
`progress.txt`, `copy.log`/`mount.log`). Builds: `mkerofs3.sh` -> `erofs2/plain.erofs`,
`erofs2/lz4c64.erofs` (both kept). Probe `probe-erofs.sh`, runner `run-erofs.sh`,
control `control11.sh` (drops the nix-lower image *and* the erofs copies, per-file
mincore proof; gate now refuses on `MemAvailable < 6 GiB` or `AnonPages > 8 GiB` rather
than on `MemFree`, because a large reclaimable page cache made the old gate refuse a
healthy host).
