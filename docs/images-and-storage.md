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
(`--dest-compress-format zstd --dest-compress-level 12`), not with `docker push`.
The flake's `.#container` archive carries uncompressed layer tars, and Docker
publishes those as they are - a pull of this image used to transfer all 8.4 GB of
layers. Per-layer zstd brings the same ten layers to 2.5 GB (measured; per-layer
`gzip -6` would be 3.0 GB), so the published manifest is OCI with
`application/vnd.oci.image.layer.v1.tar+zstd` layers. Buildah - and so every cang
pull path - reads that media type; Docker Engine's graphdriver does not, so
`docker pull` is not a supported way to obtain the cang image.

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

- host-overlay `/nix` is signaled to the guest with `CANG_NIX_OVERLAY=1` and
  `CANG_NIX_HOST_OVERLAY=1`; no `/nix` disk id/label is emitted in this mode.
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

### Both `/nix` layers on block devices

The host overlay above is the fallback. When cang can serve it, `/nix` is instead
built entirely from virtio-blk disks, and the *guest* composes the overlay: the
read-only btrfs image of the image's store (mounted at `/run/cang/nix-lower`) is
`lowerdir`, and the writable layer lives on the workspace's build disk. Reading
the store off a block device is what pays: `find /nix/store` 206-215 ms against
864-2033 ms and `du -s` 267 ms against 9.2-10.4 s with the same tree over
virtio-fs, before the upper moved as well.

`--nix` names both layers and they resolve as one decision: `blk` on either axis
moves both (the guest composes the overlay out of two disks), `virtiofs` on
either keeps both on the host share, and naming them differently is a named
error. The older `--nix-lower` flag and its
`CANG_NIX_LOWER_BLOCK` environment spelling are gone: the guest-side
`CANG_NIX_LOWER_*` variables remain, as the contract between the two halves.

The writable upper follows the same axis. With both layers block, the guest owns
the whole verdict: the read-only image is `lowerdir` and the upper and work dirs
live on the workspace's *build disk* (`cang-build.raw`, mount
`/home/dev/.cache/cang-build`), one generation directory per image digest:

    /home/dev/.cache/cang-build/nix-overlay/<digest-key>/          btrfs subvolume
    /home/dev/.cache/cang-build/nix-overlay/<digest-key>/upper     overlayfs upperdir
    /home/dev/.cache/cang-build/nix-overlay/<digest-key>/work      overlayfs workdir

The *generation* directory is the subvolume, not the upper: a btrfs subvolume root
cannot be an overlayfs upperdir (every write returns `EXDEV`, with nothing in
`dmesg`). The host cannot prepare any of this - a raw image is not mountable
rootless - so the guest seeds the generation itself, as root, right before it
composes the overlay: a new digest is a `btrfs subvolume snapshot` of the previous
generation (constant time, zero bytes) with its work dir emptied, and the image's
own Nix DB is staged from the read-only lower, which *is* the image's `/nix`. A
generation that already exists is left completely alone, so a second boot of the
same image writes nothing. Because there is no host-composed overlay underneath
it, a failure here fails `/nix` prep with a named reason instead of reporting
success over an unmounted store.

Both layers moving is what keeps the pairing legal: a block upper must not write
into a host-share upper (that mixed pair is refused, below), so `blk` on either
axis means both.

The first block boot of a workspace whose `/nix` upper was the host's does not
start empty: the guest imports the store and Nix DB that the host share had
accumulated, once, into the fresh generation. It imports those two subtrees rather
than the whole upper because a `/nix` root that ran under the host overlay holds
live artefacts - `var/nix/daemon-socket/socket` is a socket, and `cp -a` aborts on
one. Subsequent image digests clone the previous generation instead, and retired
generations are pruned: a seed keeps the generation that booted plus the newest
other completed one (the generation the next digest clones from), and deletes the
rest so the disk does not grow one generation per image forever.

Retired generations are collected at seed time, and on demand with
`cang nix gc` (`--all` collects the generation in use as well), which runs in the
same kind of narrow maintenance VM as `cang disk resize` because the disk cannot be
mounted on the host.

The upper shares the build disk's capacity, so it grows with it:
`cang disk resize --build 100G` extends the image and the guest's btrfs in one
maintenance VM (see [usage](usage.md)). A mid-run grow is not available - the
virtio-blk device's capacity is fixed when the device is created - so the disk is
sized at launch and grown only between launches.

Measured with both layers block, against `--nix=lower:virtiofs` (the host-composed
overlay over virtio-fs) in the same run, same image, `tools/nix-lower-cang-smoke`:
`find /nix/store` 2422 ms against 8508 ms, `du -s /nix/store` 2833 ms against
11228 ms, and a cold read of the store's 20 largest files 8932 ms against 5894 ms
- metadata work is what the disk wins, cold data reads are what the host's page
cache wins.

An ordinary cold 7.x-kernel read of the store used to intermittently return
stale contents for a 40 KiB window: the guest's btrfs logged `csum failed` (the
checksum of a zero page), zeroed the window and returned `EIO` - or `SIGBUS` for a
mapping over it, which is what killed a cold chromium start. The cause was in
libkrun, not in this module, the image or btrfs: a virtio-blk descriptor whose
buffer spanned two adjacent guest-memory regions was rejected whole, and the
request completed with no data and no status, which the guest's driver scored as
success - so btrfs verified pages that were never written. libkrun's map has such
a boundary at the end of the kernel image it inserts (`[16 MiB, 16 MiB +
kernel_size)`), inside RAM the guest is free to allocate from, and the 7.x kernel
merges reads into multi-folio, multi-megabyte requests whose scatter-gather
entries cross it (the 6.12 line emits page-sized entries, which cannot). Buffers
are now split per region (`descriptor_utils::buffer_region_slices`), so the
request shape no longer matters.

`tools/nix-lower-cang-smoke` asserts the no-lost-page property on every release:
40 files, 1.7 GB of cold reads at the disk's default request size. With the split
in place an instrumented 7.3.1 firmware reads them with no lost page in 9 of 9
sweeps, where the same device lost 1-19 pages per sweep before.

A separate, *deterministic* fault is also fixed: two launches sharing one `[state]
location`, where the second rebuilt the image while the first one's VM had it
attached, handed that VM blocks from the other image (2 of 2 measured). cang now
locks the image per launch - shared for the life of the VM, exclusive for a
rebuild - so a launch that would have to rebuild under a live VM keeps the
host-composed overlay and says so. Give concurrent launches their own `[state]
location` and they share the image read-only instead.

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
- Every failure keeps the host-composed overlay and names itself on stderr
  (`cang: /nix block lower unavailable, using the host-composed overlay: <err>`
  on the host, `cang-guest-init: read-only /nix lower unusable, keeping the
  host-composed overlay: <err>` in the guest). A launch never fails because of
  this path.

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
swap: during `enter`, before the shell or any background preparation starts,
`cang-guest-init` sets the device capacity in `/sys/block/zram0/disksize`,
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
