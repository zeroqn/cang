# Images, storage, and memory

## Image selection

Image selection is materialized through Buildah for the btrfs-snapshot path: with no
image option, cang first inspects `localhost/cang:latest` and uses it with
`--pull=never` when present, otherwise cang uses `ghcr.io/zeroqn/cang:latest`
with `--pull=missing`. The flake's canonical `.#container` output builds that
local `localhost/cang:latest` image with `cang-guest-init enter` as its guest
contract. `--pull-latest` refreshes the canonical image through Buildah before
cache lookup, and `--image` uses exactly the supplied image reference with
`--pull=missing`. `--image` and `--pull-latest` are mutually exclusive.

### Published image compression

`publish_image.yml` and `publish_dev_image.yml` push the image with skopeo
(`--dest-compress-format zstd --dest-compress-level 12
--dest-force-compress-format`), not with `docker push`.
The flake's `.#container` archive carries uncompressed layer tars, and Docker
publishes those as they are - a pull of this image used to transfer all 8.4 GB of
layers. Per-layer zstd brings the same ten layers to 2.5 GB (measured; per-layer
`gzip -6` would be 3.0 GB), so the published manifest is OCI with
`application/vnd.oci.image.layer.v1.tar+zstd` layers. Buildah - and so every cang
pull path - reads that media type; Docker Engine's graphdriver does not, so
`docker pull` is not a supported way to obtain the cang image.

`--dest-force-compress-format` matters because GHCR still holds these layers as
the uncompressed tars the pre-zstd pushes uploaded. Without it skopeo finds a blob
by its uncompressed digest, reuses it, and records it under a `+gzip`/`+zstd`
media type that its bytes do not have; buildah happens to sniff the real
compression and pulls it anyway, but the manifest then lies about every one of
those layers.

Compressing layers does not by itself make a changed layer cheaper: a layer whose
content changed is still re-downloaded in full. Only `zstd:chunked` layers
together with `pull_options.enable_partial_images` in the containers storage
config let a puller reuse the files it already has, and cang inherits the user's
containers configuration rather than setting that option.

## Image cache management

Cang also exposes a local image-cache management surface:

```bash
cang images list
cang images sync ghcr.io/example/cang:dev
cang images sync ba5a514
cang images remove --dry-run feedfacecafe
cang images remove feedfacecafe
cang images remove ghcr.io/example/cang:d
```

`cang images list` is read-only and reports a Buildah-aligned table with
`REPOSITORY`, `TAG`, short `IMAGE ID`, short `DIGEST`, `CACHE`, `BUILDAH`, and
`PATH` columns. Cached rows remain digest-keyed internally, but the default view
omits the redundant digest key and shows about twelve digest/image-id characters
for copyable selectors. Buildah inventory rows that do not have a matching cang
cache entry are included as `CACHE=uncached` and `BUILDAH=local-only`; old or
untagged local Buildah rows preserve Buildah's literal `<none>` repository/tag
display.

`cang images sync <reference-or-selector>` preserves full image-reference sync
behavior and can also resolve a unique visible local selector, such as a
repository/tag prefix, digest prefix, or Buildah image-id prefix, before
materializing through Buildah. If no local visible row matches, the argument is
treated as the image reference to sync; ambiguous local selectors fail before
staging.

`cang images remove <image-selector>` removes only a matching cang cache entry.
It accepts exact full digests (`sha256:...`), exact digest keys
(`sha256-...`), and unique visible-row prefixes from `images list` such as
digest, repository/tag, selected reference, or Buildah image-id prefixes.
Ambiguous selectors are refused with candidate rows, and selectors that match
only `CACHE=uncached` Buildah rows are refused because there is no cang cache
entry to delete. Removal remains cache-first: cang attempts
`buildah rmi <selected-reference>` only when a fresh Buildah image inspect
proves the selected reference still resolves to the same digest recorded in the
cache metadata. Missing, digestless, ambiguous, or mismatched local Buildah
images are left in place and reported as skipped.

`cang images remove --dry-run <image-selector>` resolves the same selector and
guard chain without mutating cache or local Buildah state. The preview reports
the exact cang cache entry and the final local Buildah target that would be
removed after the existing fallback chain (selected reference, cached image ID,
then Buildah inventory reference). Unlike real remove, dry-run fails when that
local Buildah removal would be skipped for any reason.

## Task rootfs backend

Cang uses **task rootfs backend** terminology for the host-side mechanism that
materializes the clean task root filesystem. The default backend is
`btrfs-snapshot`: cang keeps a digest-keyed btrfs image-source snapshot cache
under its per-user image state directory and snapshots that cached source into a
fresh per-task rootfs on same-digest restarts. Cache misses still use one
`buildah unshare` transaction to create a temporary Buildah working container,
mount the selected image rootfs, validate exactly one executable
`cang-guest-init`, snapshot the mounted rootfs into cang task state, and
remove the Buildah working container; known-digest misses then snapshot that task
rootfs into the digest-keyed source cache and write cache metadata. Cache hits
may inspect/refresh image metadata but avoid the Buildah working-container
lifecycle (`buildah from`, `mount`, `umount`, and `rm`). Unknown-digest runs use
the direct Buildah materialization path and do not write cache entries. There is
no `auto` backend, no initial cang `reflink` backend, and no copy/reflink
fallback for the default btrfs path; choose `fuse-overlay` explicitly when the
future portable overlay path is wanted.

## Host `/nix` overlay

For `/nix`, normal cang launches now use a workspace-scoped host kernel
overlayfs rather than attaching `cang-nix.raw`. The lowerdir is the selected
image cache rootfs under
`$STATE/cang/microvm/images/btrfs-snapshots/<digest-key>/rootfs/nix`; the
upper, work, merged, and lease files live under the workspace slug state root at
`$STATE/cang/<workspace-slug>/nix-overlay/`. Host-overlay launches run the
libkrun helper transaction through `buildah unshare`, so the VM worker mounts
and later unmounts the overlay in the same rootless Buildah namespace that can
see the selected image-cache lowerdir. The mount still happens immediately
before prepared-root grafting, and the merged view is bound to guest `/nix`.
Existing `cang-nix.raw` files are not migrated or deleted automatically.
When a mutable image tag resolves to a new digest, the host-overlay lowerdir
follows the newly selected image cache entry while the workspace-scoped upper,
work, and merged directories are reused. This intentionally preserves packages
or files written into the overlay upperdir while exposing updated lower-image
store objects that are not shadowed by upperdir entries or overlay whiteouts.
It is not a Nix database merge or repair step: persistent Nix profiles, gcroots,
database rows, and whiteouts can still describe a mixed state and may require
manual cleanup or a workspace overlay reset if they become inconsistent.

- `/nix` prep is signaled to the guest with `CANG_NIX_OVERLAY=1`, and
  `CANG_NIX_HOST_OVERLAY=1` says a host-composed overlay was grafted at `/nix`
  for it to keep; a launch that serves both layers from the block disk sends only
  the first, because it grafts none (see the next section). No `/nix` disk
  id/label is emitted in either mode.
- host-overlay `/nix` requires `buildah` on `PATH`; permission-denied kernel
  overlay failures should be diagnosed from inside `buildah unshare`, because
  plain outer-namespace `mount -t overlay` does not have the required rootless
  idmap/storage context.
- Nested/rootless Podman storage uses the workspace-scoped
  `cang-containers.raw` btrfs disk by default. The host exposes that disk as
  `CANG_CONTAINERS` / `cang-containers` for guest rootless container storage,
  and guest Podman uses the `btrfs` storage driver.
- `cang --container-store raw-disk` remains accepted as an explicit
  compatibility spelling for the only supported container-store backend.
  `--container-store bind` is not supported, and cang does not migrate old
  host-directory container stores.

### `/nix` on the block devices, driven by the workspace disk

The host overlay above is the fallback. A launch that names a workspace disk - the
btrfs tree described in [usage](usage.md) - gets `/nix` from virtio-blk instead,
and the *guest* composes the overlay: the read-only btrfs image of the image's store
(mounted at `/run/cang/nix-lower`) is `lowerdir`, and the writable layer lives in
the **workspace tree**. Reading the store off a block device is what pays:
`find /nix/store` 206-215 ms against 864-2033 ms and `du -s` 267 ms against
9.2-10.4 s with the same tree over virtio-fs, before the upper moved as well.

There is no flag for any of it. `/nix` follows the workspace: `--workspace-disk`
means the block pair, no workspace disk means the host-composed overlay. The
`--nix=lower:...,upper:...` axis this section used to describe is gone; the guest-side
`CANG_NIX_LOWER_*` variables remain, as the contract between the two halves.

The upper is one generation directory per image digest inside the tree:

    /workspace/.cang/nix/<digest-key>/          btrfs subvolume
    /workspace/.cang/nix/<digest-key>/upper     overlayfs upperdir
    /workspace/.cang/nix/<digest-key>/work      overlayfs workdir

The *generation* directory is the subvolume, not the upper: a btrfs subvolume root
cannot be an overlayfs upperdir (every write returns `EXDEV`, with nothing in
`dmesg`). `upper` and `work` are plain directories inside it, which is also what
makes a generation snapshot-able: children that are plain directories are cloned
in constant time, where a nested subvolume would not be copied into a snapshot at
all.

The host prepares none of it - a raw image is not mountable rootless - so the guest
seeds the generation itself, as root, right after it mounts the workspace disk and
before it composes the overlay. A new digest is a `btrfs subvolume snapshot` of the
previous generation (constant time, zero bytes) with its work dir emptied, and the
image's own Nix DB is staged from the read-only lower, which *is* the image's
`/nix`. A generation that already exists is left completely alone, so a second boot
of the same image writes nothing.

`/nix` then has exactly **one** mount. The host grafts nothing at `/nix` for this
pairing - its own overlay would be a virtio-fs-backed `/nix` covered by the
guest's, reachable by no access and read by a guest whose overlay had failed - so
`CANG_NIX_HOST_OVERLAY` is not set, the mount point is a plain directory (the
rootfs snapshot's), and the guest's `overlay /nix overlay
lowerdir=/run/cang/nix-lower,upperdir=…` is the only `/nix` in `/proc/mounts`.
Every path under `/nix` is therefore on the block devices. Because there is no
overlay underneath it, a failure here fails `/nix` prep with a named reason
instead of reporting success over a store that was never composed.

The upper shares the workspace image's capacity, so it grows with it:
`--workspace-size` sizes the image at creation, `cang workspace serve` grows the
image itself before it mounts it (when the tree's recorded free space is under a
tenth of it and the host keeps twice the step's headroom), and the guest expands
btrfs to the new device size on the next mount. A one-shot `--workspace-disk`
launch does not auto-grow: `cang disk resize --workspace-disk PATH SIZE` is the
command, and `docs/usage.md` states the split plainly. Measured with the block pair, against the
host-composed overlay in the same run, same image,
`tools/nix-lower-cang-smoke`: `find /nix/store` 2422 ms against 8508 ms,
`du -s /nix/store` 2833 ms against 11228 ms, and a cold read of the store's 20
largest files 8932 ms against 5894 ms - metadata work is what the disk wins, cold
data reads are what the host's page cache wins.

- The image is built with `mkfs.btrfs -q -L CANG_NIX_LOWER
  --rootdir=<image rootfs>/nix --reflink --shrink` inside the Buildah storage
  namespace and cached at
  `<state>/cang/microvm/images/btrfs-snapshots/nix-lower/<identity>.raw` with a
  `.state` sidecar naming the image and its store paths. It is ~12 GiB
  apparent; it is reused while that identity is unchanged, so only the first
  launch after an image or store change pays the build (tens of seconds), and
  the guest's own work starts correspondingly later on that launch. Delete the
  `nix-lower` directory to reclaim the space or force a rebuild.
- It needs `mkfs.btrfs`/`btrfs` (`btrfs-progs`) and `buildah` on the host, plus
  free space under the state root for one image per cached image generation.
- The guest half lives in the `cang-guest-init` the launch actually runs, so an
  image built by an older cang (guest-init 0.17.2 or earlier) ignores the
  switch; cang reads that binary before building anything and says so:
  `cang: /nix block lower skipped: '<path>' predates block mode`.
- The build is priced before it starts: with the target on the image store's
  own filesystem `--reflink` shares the extents, so only a 512 MiB floor is
  required, and across filesystems the tree's apparent size is. A filesystem
  that cannot hold it is refused up front rather than left to a multi-gigabyte
  `mkfs.btrfs` that fails late.
- Whether a failure keeps the launch depends on which `/nix` the launch has as a
  pair. `--nix=lower:auto` (the default) keeps the host-composed overlay and says
  so on stderr - `cang: /nix block lower unavailable, keeping the host-composed
  overlay (--nix=lower:auto): <err>` - and `--nix=lower:blk` refuses the launch
  with that reason instead. Once the block pair is served there is no overlay to
  fall back to, so a guest-side mount failure fails `/nix` prep by name
  (`cang-guest-init: … (upperdir '…', lowerdir '/run/cang/nix-lower')` in the
  guest's status file and log) rather than reporting a `/nix` that was never
  composed.

## Guest memory and zram swap

When `--mem` is omitted, cang now sizes the direct-libkrun VM to 80% of host
memory rounded down to whole GiB, matching the libkrun VM memory policy. Pass
`cang --mem <GiB>` to override that default. Guest bootstrap also sets
`SCCACHE_DIR=/home/dev/.cache/sccache`, backed by cang's shared state
`sccache` bind mount.

cang also attaches the virtio-balloon device. libkrun implements only the
balloon's free-page-reporting queue - the inflate/deflate queues are stubs that
log and drop - so this does not make `--mem` elastic: the guest still sees the
memory it was configured with, and the guest kernel's virtio-mem is off. What it
buys is host-side reclamation. When the guest reports pages it has freed, the
VMM `madvise`s that part of the guest RAM mapping, so the host gets those
physical pages back instead of holding every page the guest has ever touched
until the VM exits. This is visible on the host as the VM worker's RSS dropping
while the guest runs on, and in the guest as a bound driver under
`/sys/bus/virtio/drivers/virtio_balloon/` (the pinned kernel builds the driver
in).

Because the balloon can only return what the guest has already freed, an idle
guest that still holds a few hundred megabytes of page cache - the image's
`/nix` store, files read over virtiofs - keeps that memory away from the host at
exactly the moment the host starts swapping. The helper therefore watches the
host's own pressure and asks the guest to release the cache. Two samples run at
1 Hz: `full avg10` from `/proc/pressure/memory`, whose `full` line counts only
time that every runnable task was stalled, and the swap-out rate from
`/proc/vmstat`. Either crossing its threshold sends a `ReclaimMemory` frame over
the exec control channel; the guest writes `1` to `/proc/sys/vm/drop_caches` and
answers with the growth in `MemFree` that the release produced, so the helper's
log line reports what it bought:

```text
host memory pressure: guest released its clean page cache trigger=swap-out freed_kib=412320
```

The guest kernel's free-page reporting then hands those pages back, which is the
same `madvise(MADV_DONTNEED)` path as above, so the VM worker's RSS drops while
the guest keeps running. The guest pays for it by re-reading whatever it had
cached: `drop_caches` never discards dirty pages, so a guest in the middle of a
write keeps its data, and only the page cache goes, not the dentry and inode
slabs. A release is rate-limited to one per 30 seconds, and the whole watcher is
off with `CANG_MEMORY_RECLAIM=0`. `CANG_MEMORY_RECLAIM_PSI_FULL` (percent,
default 1) and `CANG_MEMORY_RECLAIM_SWAP_OUT` (4 KiB pages per second, default
64) move the thresholds - a threshold of `0` fires on the first sample, which is
how the release path is verified on a host under no pressure.

Guest RAM is fixed for the life of the microVM, so the guest also gets zram
swap: `cang-guest-init` sets the device capacity in `/sys/block/zram0/disksize`,
bounds what the device may spend in `/sys/block/zram0/mem_limit`, signs it with
`mkswap`, and activates it with `swapon -p 100`. The capacity equals guest RAM
and the memory budget is a quarter of it. Capacity counts uncompressed bytes
and zram holds a page compressed, so compressible content costs a fraction of
the space it occupies, while the budget stops pages that do not compress from
being parked at roughly 1:1. The pinned `libkrunfw` kernel is built with
`CONFIG_SWAP` and `CONFIG_ZRAM` (zstd default, lzo available). Swap makes cold
anonymous pages reclaimable, which turns memory pressure into slower progress
instead of a guest OOM kill; it does not add memory, and a kernel without zram
records `state=unavailable` in `/run/cang/swap.status` rather than failing the
session.

Activation is the guest's slowest boot step (measured 0.6-1.3 s of the 1.8 s
`cang enter` path), and writing the capacity is nearly all of it: zram
allocates metadata for the whole device, so the cost is CPU-bound and scales
with guest RAM (1.41 s of `disksize`, 0.18 s of `mem_limit`, 0.10 s of `mkswap`,
0.20 s of `swapon` in a 12.7 GiB guest). `cang enter` therefore does not do it:
it starts a detached worker (`cang-guest-init internal swap ensure
--after-exec`) as its last step and hands it a pipe whose write end closes when
`enter` execs (or, on the managed-session path, when it hands the terminal to
the shell). The worker blocks until that happens, so no part of the enter path
waits for the swap device or competes with it - measured beside the enter path,
the sizing slowed that path by its own cost one-for-one. The device is active
within about a second of the shell starting; a workload that allocates
everything it can in that first second runs without swap, which is the same
resilience it had before the first `swapon`.

## Launch config

Cang config lives at:

```text
$XDG_CONFIG_HOME/cang/cang.toml
```

or, when `XDG_CONFIG_HOME` is unset:

```text
$HOME/.config/cang/cang.toml
```

Supported launch-planning keys are:

```toml
[state]
location = "/home/dev/cang-state"

[task-rootfs]
backend = "btrfs-snapshot" # or "fuse-overlay"
```

`[state].location` changes the base cang state location; cang appends
`/cang/<workspace-slug>`. `--rootfs-backend` overrides
`[task-rootfs].backend` for a single run.
