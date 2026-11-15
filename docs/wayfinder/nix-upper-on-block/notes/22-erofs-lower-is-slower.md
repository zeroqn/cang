---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Switched the lower to erofs and measured it: it is 2x the requests and 3-4x slower. Reverted.

Implemented the whole erofs lower - host `mkfs.erofs -T 0`, `CANG_NIX_LOWER_FSTYPE` in
the guest contract, `mount -t erofs`, a labelless-disk lookup (an erofs image carries no
filesystem label, so `blkid -L CANG_NIX_LOWER` finds nothing), `erofs-utils` in the helper
dir, docs and the space preflight. It works end to end: the guest mounts
`/dev/vdc /run/cang/nix-lower erofs ro,relatime,user_xattr,acl`, sees all 3 897 store files,
and builds in ~6 s. **It is slower, so it is reverted** (the patch is kept at
`blkread/erofs-lower-switch.patch`).

## 1. Measurements, same walk, same counters, one boot each

`find <store> -maxdepth 3 -type f`, 3 897 files, guest `drop_caches` and a mincore-proved
host drop of the image before every arm; device counters read from `/sys/block/vd*/stat`
(the erofs boot's `vdc` at 16 163 712 sectors):

| lower | device reads per walk | wall | per read |
|---|---|---|---|
| btrfs (941 reads measured in note 15) | **941** | 362-500 ms | ~430-530 us |
| erofs, `mkfs.erofs -T 0` default | **1 940** | **1 355 / 1 937 / 2 389 ms** | ~700-1 230 us |

Dense random 16 KiB reads of one 610 MB file: erofs mean 859 / 2 008 us (p50 412 / 1 481)
against btrfs's 294-740 us (p50 278-626) measured in note 21.

So erofs issues **about twice** the requests for the same metadata walk and each one costs
more. Erofs metadata blocks are 4 KiB while btrfs packs 16 KiB nodes; a walk that touches
nearly every inode therefore pays roughly twice as many round trips, and on this host a
round trip (not the host's bytes) is the whole cost.

## 2. This retracts note 18's erofs win

Note [18](18-erofs-lower-metadata.md) reported 240-250 ms for an erofs-plain `find` against
btrfs's 362-447 ms and called erofs 1.5-1.8x faster. That comparison mounted the erofs image
through `loop` **from a file on the guest's own root filesystem**, and the host-side cache
control dropped only the `nix-lower` image - never that loop file. The erofs arms were
therefore reading a warm copy from the guest page cache, which is why they looked fast and
why they reported no device reads at all (that boot's counters were also broken by the same
`rootdev=root` parse). On the real virtio-blk path, cold, erofs is slower, not faster.

Note 18's own words warned the numbers were a "lower bound" because of the loop layer; they
were not a lower bound, they were a cache hit.

## 3. What this leaves

- **btrfs stays.** Its 16 KiB metadata nodes issue half the requests of erofs's 4 KiB
  blocks for an inode-heavy walk, and that request count is the cost that matters here.
- The erofs work is not wasted as evidence: it proves the mechanism (denser *filesystem*
  layout does not mean fewer *requests*) and the patch is one `git apply` away if a future
  measurement with different mkfs options (`-E noinline_data`, larger block/chunk settings)
  changes the picture. Any such attempt must count requests per walk, not just time.
- **Every future lower comparison records reads-per-walk.** That single column would have
  caught both this and note 18 immediately.

## Evidence

`run-find-20261115T160222Z` (erofs lower: 661/680/597/494 ms, the arm where the erofs mount
first worked), `run-count-20261115T160636Z` (`count.txt`: 1 940 device reads per walk,
1 355/1 937/2 389 ms, plus the dense randlat arms), `run-diag-20261115T155413Z` (`diag.txt`:
`blkid` showing `TYPE="erofs"` and no label, `CANG_NIX_LOWER_FSTYPE=erofs`, a manual
`mount -t erofs` succeeding while the guest-init's own mount failed on the label lookup),
`erofs-lower-switch.patch` (the reverted implementation), `run-find-20261115T154602Z`
(the first attempt, which failed exactly on that label lookup).
