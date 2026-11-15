---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The btrfs lower is compressed; keeping btrfs is fine, but "less disk" only holds while it is

## 1. Is the lower uncompressed? No

`filefrag -v` on the `nix-lower` image: **63 433 of 88 374 extents (71.8 %) are
`encoded`**, i.e. zstd - the image inherits `compress=zstd:1` from the state root's mount
(`rw,relatime,compress=zstd:1,ssd,discard=async,space_cache=v2,...`). Apparent size
12.4 GB, on disk **7.9 GB**.

## 2. Keeping btrfs costs nothing in capability, only in disk

btrfs can serve an uncompressed image without changing format - the compression is a
per-file/subvolume property, not a filesystem choice. Measured on the same btrfs
filesystem, same 2 GiB of bytes, cold before each run
([note 17](17-per-read-cost-is-fs-compression.md)): compressed p50 240/215 us per 16 KiB
read, `compression=none` p50 116/78 us. So the btrfs-uncompressed arm is already known to
be 2.0-2.8x faster per small cold read; it just needs the image *written* that way
(`btrfs property set -f <file> compression none` before writing, or a subvolume whose
`compression` property is `none`). Reflink and snapshots are unaffected by either choice.

## 3. But the disk ranking is the opposite of "btrfs occupies less"

| lower | on disk | cold metadata walk (measured) |
|---|---|---|
| btrfs + zstd:1 (today) | **7.9 GB** | 362-447 ms |
| erofs, uncompressed | 8.4 GB | 240-250 ms |
| btrfs, uncompressed | **12.4 GB** (apparent) | not yet measured; expected ~250-300 ms |
| erofs + lz4 -C 65536 | **4.2 GB** | 416-440 ms |

So btrfs wins on space *only while it is compressed*, and that is exactly the setting
that costs the latency. Uncompressed btrfs is the **largest** option on the board
(+4.5 GB per image, and there are three `nix-lower` identities -> +13.5 GB), while
uncompressed erofs is both smaller than uncompressed btrfs and 1.5-1.8x faster than
compressed btrfs. Reflink/snapshot semantics are the real argument for btrfs, not space;
space argues for erofs (or for keeping btrfs compressed and accepting the metadata cost).

## 4. Correction to how this comparison was described

Note [18](18-erofs-lower-metadata.md) says the erofs image was built from "the image's own
/nix/store". Verified: the guest's lower serves **621** store directories and all 621 are
present in that rootfs store (685 entries, so the erofs tree is a superset by 64 paths).
An earlier check of mine reported 0 overlap; that was a script bug (`awk '{print $2}'` on
single-column `find` output), not a real content difference. The comparison is
content-equivalent for every path the lower serves.

## Evidence

`filefrag -v` extent audit and `findmnt` mount options on the image; sizes from `du
--apparent-size` and `du`; neighbour numbers from notes 17 and 18.
