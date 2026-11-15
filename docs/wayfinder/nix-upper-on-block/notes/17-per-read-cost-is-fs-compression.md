---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# (#2) The per-read cost is btrfs `compress=zstd:1` on the state root: 2-3x per cold read

The state root's mount is
`rw,relatime,compress=zstd:1,ssd,discard=async,space_cache=v2,...`, and the `nix-lower`
image inherits it: `filefrag -v` reports its extents as **`encoded`** (compressed), 88 307
of them for 12 GB. This note isolates what that costs a small cold read, on the same
disk, same filesystem, same content, with the page cache dropped before every run
(`posix_fadvise(POSIX_FADV_DONTNEED)`, mincore-verified).

## 1. Same disk, same subvolume, same 2 GiB of bytes, 16 KiB random reads

| file | storage | extents | p50 µs | mean µs | p90 µs |
|---|---|---|---|---|---|
| `zstd.bin` (mount default) | `compress=zstd:1` | 5 802 | **240 / 215** | 263 / 241 | 404 / 382 |
| `none.bin` (`compression=none` before write) | uncompressed | **2** | **116 / 78** | 122 / 86 | 160 / 117 |
| `nocow.bin` (`chattr +C` + `compression=none`) | nodatacow, uncompressed | 2 | 115 / 78 | 129 / 82 | 170 / 106 |

(two runs each, alternating, cold each time). **Compression costs 2.0-2.8x per small cold
read; `nodatacow` adds nothing on top of that.** The compressed file also fragmented into
5 802 extents against 2.

## 2. Why this is the metadata penalty

Note [15](15-cold-metadata-is-device-reads.md): a cold `find` of 3 897 files costs ~316 ms
above the warm 54 ms while moving only 482 KB in **941 read I/Os**, i.e. ~336 µs per read.
This note's compressed per-read cost is 240 µs p50 and its uncompressed cost is 78-116 µs,
so compression plausibly accounts for **roughly half to two thirds** of that penalty
(≈215-250 µs × 941 ≈ 200-235 ms of ~316 ms).

## 3. What it would take to bank it

The lever is where the image is *created*, not in the guest: write the `nix-lower` image
(a 12 GB sparse file whose data is mostly already-compressed Nix archives and ELF) with
`compression=none`, or place it in a subvolume whose `compression` property is `none`
(properties are inherited by files created in the subvolume; the unprivileged
`btrfs property set -f <file> compression none` before writing works, as used above).

Trade-off, measured with the same content: `du` reports **2.0 GB for all three 2 GiB
files** - this slice of the lower does not compress at all, so `zstd:1` is charging 2-2.8x
latency for no space. Across the whole image the mount does save something (12 GB apparent
against 7.9 GB on disk), so the honest statement is: **the compressible part of the image
buys ~34 % space and the incompressible part buys nothing, while every small read pays
the decompression cost.** A per-path decision (compress what compresses, leave the rest
alone) is the version worth implementing, and it is a cang-side image-creation change.

## 4. Disk state during this work

The state disk was 93 % full (11 GB free) and my own campaign scratch held 61 GB of stale
state roots plus 23 GB of stale erofs payloads from the 2026-11-09/10 campaign. I removed
those payload directories (`run-20261109T164024Z/{block,virtiofs}/state`, `run4-…`,
`run5-…`, `run7-…`), keeping every log, evidence file and script; the disk is now 70 %
used with 42 GB free. No cang state root outside my campaign scratch was touched.

## 5. Prior art for (#1) found while cleaning

`/home/dev/disp/blkread/erofs/{lz4.log,zstd.log}` and `fsimg/{ext4.img,…}` are from a
2026-11-10 session that already built `erofs` images of the same store with mkfs.erofs 1.9
(`plain.erofs`, `lz4.erofs`, `zstd.erofs`, `lz4c64/c256` variants - I removed those
payloads for space). So (#1) is not unexplored: the method exists, the payloads are
regenerable, and the next step there should start from those logs rather than from scratch.

## Evidence

`randlat` (static, random-offset `pread` latency distribution, `buffered`/`direct`, seed
and size-limit args) run against `zstd.bin`/`none.bin`/`nocow.bin` in
`/home/dev/disp/cangtest/sub` (removed after measurement), image extents and mount options
from `filefrag -v` and `findmnt`; `erofs/*.log` and `fsimg/` inspected in place.
