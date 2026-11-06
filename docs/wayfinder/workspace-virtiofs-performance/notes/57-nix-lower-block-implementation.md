# Note 57: implementation spec - /nix lower layer as a reflinked btrfs block file

2026-11-06. Note 56 measured why: the lower is 7.9 GiB over 73 790 store paths
read on every command, and it is 7.5-43x faster on virtio-blk. This is how to
build and wire it.

## Where `/nix` comes from today

`HostNixOverlay` (`runtime/launch/config/model.rs`, built in
`runtime/session/nix_overlay.rs::intent_from_task_rootfs`):

```
lowerdir = <state>/microvm/images/btrfs-snapshots/sha256-<digest>/rootfs/nix
upperdir = <state>/cang/<workspace>/nix-overlay/upper
workdir  = <state>/cang/<workspace>/nix-overlay/work
mergeddir= <state>/cang/<workspace>/nix-overlay/merged
```

The host mounts that overlay itself, inside `buildah unshare`, and the guest sees
only `mergeddir` - so today all three layers are directories on the host btrfs,
reached over virtio-fs. The per-digest `rootfs` is already a cheap subvolume
snapshot; the duplication problem is not there.

## The change

Serve **only the lower** from a read-only btrfs block file, keep upper/work
exactly as they are (host btrfs, dedup with the host's own store and across
generations - see note 56).

### Structure inside `microvm/images/btrfs-snapshots/`

```
sha256-<digest>/rootfs/...          unchanged: per-digest subvolume snapshot
nix-lower/<identity>/               btrfs subvolume snapshot of <digest>/rootfs/nix
nix-lower/<identity>.raw            read-only btrfs image, built from the above
nix-lower/<identity>.state          store-path count + label + built-at, for reuse checks
```

`<identity>` keys the **content of the store**, not the image digest, so two image
digests that ship the same `/nix` share one block file:

```
identity = sha256 of the sorted `ls <rootfs>/nix/store` names
```

### Building the block file - proven recipe

`mkfs.btrfs --rootdir` populates a filesystem image without mounting it, and
`--reflink` clones the file data instead of copying it. That is the whole
"lowest dup files" story: the image's data extents are the source files' extents.

Must run inside the storage user namespace, where the store appears owned by root:

```sh
podman unshare mkfs.btrfs -q -L CANG_NIX_LOWER \
    --rootdir=<identity>/ --reflink --shrink <identity>.raw.tmp
mv <identity>.raw.tmp <identity>.raw
```

Measured on this host (probe, `2026-11-06`): a 105 MB apparent image cost
**2.7 MB on disk**. `mkfs.btrfs` prints one harmless
`ERROR: zoned: unable to stat ...` line for a non-zoned target and exits 0; it must
not be treated as failure (check the exit status).

### Wiring

- Attach `nix-lower/<identity>.raw` as a **read-only** virtio-blk disk
  (`DiskAttachment { id: "cang-nix-lower", label: "CANG_NIX_LOWER", read_only: true }`);
  `read_only` already reaches `block_device_new`.
- Env: `CANG_NIX_LOWER_DISK_ID`, `CANG_NIX_LOWER_DISK_LABEL`, and a flag
  `CANG_NIX_LOWER_BLOCK=1`. Absent the flag the guest keeps today's behaviour, so a
  launch that could not build the image still works.
- **The guest must now compose the overlay itself.** The host cannot mount a block
  device, so `mergeddir` no longer exists: the guest mounts the read-only disk at
  `/run/cang/nix-lower` and mounts
  `-t overlay overlay -o lowerdir=/run/cang/nix-lower,upperdir=<upper>,workdir=<work>,userxattr /nix`
  with `<upper>`/`<work>` passed in from the host as two new env paths (today the
  host mounts them itself; they are ordinary host directories, so the guest can
  bind them or take them over the existing share).
- Everything else - `install_staged_image_db`, `repair_store`, `preseed_upper`,
  the generation lease - is unchanged: it all operates on upper/work, not the lower.

## Why not the whole rootfs

Measured: `/nix/store` is 7.9 GiB of the image and the tree every command touches;
the rest of `/` is small and read once. Converting the whole rootfs per digest
would cost a snapshot per digest and gain much less.

## Fallback

Two knobs are enough to stay safe: no block file (mkfs failed, no space) means the
host-composed overlay as today; `CANG_NIX_LOWER_BLOCK=0` forces that path for an
A/B.

## Built, then blocked by the kernel: overlayfs cannot use a virtio-fs upper

Implemented as above and verified live (`nix-lower-block` session, report at
`/home/dev/disp/nix-lower-report.md`). The image side is exactly as predicted:

| check | result |
| --- | --- |
| build | 38.6 s, 12.4 GB apparent (11.5 GiB), **56.6 MiB of new disk** (reflinked) |
| reuse (2nd, 3rd launch) | `reused`, 0.6 ms |
| guest mount | `/dev/vdc btrfs ro,...` at `/run/cang/nix-lower` |
| store content | 685 `ls` names / 73 790 `find` nodes, identical to the image |
| `find` over it | **274 / 274 / 292 ms** vs **2 258 / 1 321 / 510 ms** on the virtio-fs lower |
| `du -s` | **343-2 025 ms** vs 10 495-11 592 ms |
| 200 file reads | 94 ms vs 331 ms |

And then the whole plan stops at the kernel, in the guest's dmesg:

```
overlayfs: upper fs does not support tmpfile.
overlayfs: upper fs does not support RENAME_WHITEOUT.
overlayfs: upper fs missing required features.
```

`fs/fuse/inode.c`:

```c
static struct file_system_type fuse_fs_type = {
	.owner		= THIS_MODULE,
	.name		= "fuse",
	.fs_flags	= FS_HAS_SUBTYPE | FS_USERNS_MOUNT | FS_ALLOW_IDMAP,
	...
```

`FS_RENAME_WHITEOUT` is absent, and overlayfs requires it of the **upper's
filesystem type** at mount time. That is a static kernel flag: no mount option, no
FUSE INIT flag, and no change to cang's virtio-fs server can set it. So the upper
of an overlay cannot live on virtio-fs, which means the guest cannot compose a
`/nix` overlay while upper/work stay host directories - and the host cannot compose
one over a block lower, because mounting btrfs needs privileges it does not have.

Conclusion: **a block lower and a virtio-fs upper cannot be combined.** The
remaining routes, none of which is free:

1. **upper/work on a block btrfs image** (a third per-workspace disk): works with
   kernel overlayfs, gives up the reflink dedup with the host `/nix/store` - the
   reason note 56 kept the upper where it is.
2. **fuse-overlayfs in the guest**: keeps the dedup and the block lower, and puts a
   userspace FUSE layer in front of every `/nix` access - unmeasured, and the
   opposite direction from what note 56 measured.
3. Keep the host-composed overlay (today) and drop the block lower.

Shipped state: the host half is opt-in behind `CANG_NIX_LOWER_BLOCK=1` and defaults
off (default-on would pay 38 s to then fall back). It is useful the moment route 1
or 2 is chosen; until then it is a verified builder plus a measurement harness.
