---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The state root's zstd costs 30 % of host device service and nothing end to end; the
# uncompressed build costs 2.7 s and no disk

Two questions answered with measurements: what the compression actually costs the guest,
and what it costs to remove it.

## 1. Removing it is cheap: 37.3 s -> 40.0 s, same disk

`mkfs.btrfs --rootdir <image nix> --shrink` on the real store (685 paths, 7.7 GB), same
host, back to back, into files pre-set to `compression=none`:

| build | wall | extents | `encoded` | apparent | on disk |
|---|---|---|---|---|---|
| `--reflink` (what cang ships) | **37.3 s** | 88 054 | 62 898 | 13 GB | 7.8 GB |
| copy, no `--reflink` | **40.0 s** | 241 | **0** | 13 GB | **7.8 GB** |

Earlier claims in this map of "+4.5 GB" and "a full 12 GB copy" were wrong: the free-space
regions are holes in both shapes and the store is 7.7 GB of real data, so an uncompressed
image occupies the same 7.8 GB. The honest cost is **+2.7 s per image build** (paid once per
image generation, then reused) plus losing extent sharing with the snapshot. (Both arms
ended with `ERROR: unable to traverse directory … Operation not permitted` because they ran
as an unprivileged user outside cang's storage namespace; the full 13 GB image was written
before that, so the comparison stands, and cang's own build runs privileged.)

## 1b. An erofs lower builds *faster* than the btrfs one, not slower

The assumption worth checking, since erofs is the option on the table: `mkfs.erofs 1.9`
against the same store (73 789 entries, 14 513 directories, 7.7 GB), timed on this host:

| build | wall | image on disk |
|---|---|---|
| `mkfs.btrfs --rootdir --reflink --shrink` | **37.3 s** | 7.8 GB (13 GB apparent) |
| `mkfs.erofs -T 0` (uncompressed) | **5.9 s** | 7.8 GB |
| `mkfs.erofs -zlz4 -C 65536 -T 0` | **16.1 s** | **4.0 GB** |
| `mkfs.btrfs --rootdir --shrink` (copy, no reflink) | 40.0 s | 7.8 GB |

So an uncompressed erofs lower is **6x cheaper to build** than the btrfs one and the same
size on disk, and the lz4 erofs is still 2.3x cheaper while being **half the size** of
either - it just reads metadata slower in the guest (note
[18](18-erofs-lower-metadata.md): 416-440 ms against plain erofs's 240-250 ms and btrfs's
362-447 ms). Nothing about the erofs option is expensive at build time.

## 2. What the compression costs the guest: 30 % of host device time, ~0 end to end

Dense-region A/B, which is what the earlier image comparison lacked: one 610 MB lower file,
300 random 16 KiB reads inside its first 64 MiB, guest `drop_caches` and a mincore-proved
host drop of the image before every cold arm. Two boots, one image shape each, guest device
counters now working (`/sys/block/vd*/stat`, 307 reads per arm, i.e. 300 + 7).

| image | arm | mean µs | p50 µs | p90 µs | guest device read_ms |
|---|---|---|---|---|---|
| compressed (62 961 `encoded`) | c1 | 655 | 584 | 877 | 162 |
| | c2 | 410 | 374 | 546 | 95 |
| | c3 | 358 | 330 | 437 | 84 |
| uncompressed (0 `encoded`) | c1 | 740 | 626 | 1085 | 68 |
| | c2 | 328 | 311 | 375 | 66 |
| | c3 | 332 | 295 | 382 | 60 |
| | c4 | 294 | 278 | 355 | 60 |

- **Host device service drops ~30 %** uncompressed (84-162 ms against 60-68 ms per 300
  reads) - the compression cost is real, and it is on the host.
- **Guest-observed latency does not follow**: p50 overlaps completely (compressed
  330-584 µs, uncompressed 278-626 µs). At 16 KiB per read the ~100 µs of host
  decompression is a minority of a 300-600 µs round trip through virtio-blk and the
  guest's own btrfs path.
- Warm (guest page cache) reads are 3-4 µs with zero device reads, so the gap between cold
  and warm is entirely the I/O path, not the filesystem.

That reconciles every earlier number in this map: the metadata walk costs ~400 ms for 941
reads ≈ 430 µs per read, of which the host's share is 84-162 ms; removing compression buys
~30-60 ms of a 400 ms walk, which is under the noise of this host and exactly why the guest
arms showed no difference.

## 3. What is left, and it is not the mount

The remaining ~250-300 ms of the walk is per-I/O cost that no storage-side property
changes: **fewer requests** is the only lever left, which is what note 18's erofs-plain
result (240-250 ms) looks like, and why it beat btrfs by 1.5-1.8x while erofs-lz4 (same
request count, compressed) matched btrfs rather than beating it.

## Evidence

Build timing: `mktime2.sh` on the real store, logs kept per arm. Guest A/B:
`run-dense-20261115T151310Z` (compressed image, `ec5b74e0…` rebuilt by the shipped
`--reflink` path) and `run-dense-20261115T151721Z` (uncompressed, 340 extents, 0 `encoded`,
built by the no-reflink patch which was afterwards reverted); `workspace/evidence/readlat.txt`
and `setup.txt` per run, `logs/env.txt` for the gate values and binary md5. Probe
`probe-dense.sh`, runner `run-dense.sh`, control `control11.sh`, instrument `randlat`.
