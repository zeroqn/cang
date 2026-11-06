# Note 56: the /nix lower layer is the expensive half of the overlay

2026-11-06. Answers "should we move `/nix` onto a block device, and which half".

## The two halves are not equal

The guest's `/nix` is already an overlay (`nix/root.rs`):

```
mount -t overlay overlay -o lowerdir=/nix,upperdir=<disk>/upper,workdir=<disk>/work,userxattr /nix
```

- **lower** - the image's `/nix/store`: 73 790 top-level store paths, 7.9 GiB,
  read-only, and read on *every* command anyone runs (every binary in the guest
  comes from it).
- **upper** - only what this workspace's nix has added since the image was made.
  Small, and its objects are reflink-shared with the host's own `/nix/store`
  because it lives in the host's btrfs state tree.

So the access frequency and the media question are opposites: the half that is
read constantly is the read-only one.

## Measured: the same bytes on both media

Copied the first 1 200 store paths (685 paths, 7.9 GiB - the copy filled up) to
the containers disk (btrfs on virtio-blk, the medium a block lower would use) and
ran the same operations over both trees. Two rounds, same data on both sides:

| operation | virtiofs (today's lower) | btrfs on virtio-blk | ratio |
| --- | --- | --- | --- |
| `find` walk, 73 790 nodes | 2 322 / 4 890 ms | **309 / 315 ms** | **7.5-16x** |
| `du -s` (full walk, sums sizes) | 10 572 / 10 669 ms | **244 / 248 ms** | **43x** |
| 500 files, 4 KiB read each | 9 564 / 10 820 ms | 1 895 / 2 135 ms | ~5x |
| `readlink` sweep | 41 046 / 46 258 ms | 38 743 / 39 700 ms | ~1.06x |
| `stat` per store path | 2 183 / 2 319 us | 2 104 / 2 132 us | ~1.0 |

The last two rows are **spawn-dominated**, not filesystem measurements: the loops
fork `stat`/`readlink` from `/nix/store` per item, so they pay a process start
(~2 ms in this guest) on both sides. The clean rows are the single-process walks:
`find` 7.5x, `du` 43x, and content reads ~5x.

Which is the point: the lower is metadata- and read-heavy, and that is exactly
what virtio-fs charges per operation.

## So: change the lower, keep the upper

- **lower**: one btrfs image per image generation, attached **read-only**, and
  reflinked per guest from a host master (`cp --reflink=always`, O(1), shared
  extents). Several guests can use the same host file because it is never written.
  No loop device - a file *is* a virtio-blk device.
- **upper + work**: leave them where they are, on the host btrfs via virtio-fs.
  Not for speed - they are the small half - but for **dedup**: nix objects the
  guest substitutes stay reflink-shared with the host's `/nix/store` and with every
  other workspace and generation on that volume, and the host's own
  new-generation seeding is a clone of the previous upper (a btrfs snapshot). Put
  the upper on a block image and that sharing becomes a byte copy.

Cost of the plan: an 8 GiB read-only image per generation on the host volume,
reflinked (no extra space per guest), plus a guest-init step that mounts it before
the overlay - the mount the code already performs, only with `lowerdir` pointing at
the disk instead of the rootfs.
