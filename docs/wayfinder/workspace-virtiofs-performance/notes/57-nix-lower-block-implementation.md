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
