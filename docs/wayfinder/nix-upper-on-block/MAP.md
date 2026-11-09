---
label: wayfinder:map
title: The writable /nix upper on the guest-local block disk
---

## Destination

`/nix` writes stop crossing virtio-fs: the overlay's **upper** (and `work`) move
from the host-directory share onto the guest-local persistent btrfs disk the
workspace already carries (`cang-build.raw`), composed by the guest as kernel
overlayfs exactly as the read-only lower already is - **but only after a
measurement says it pays**. The map ends at one of two places:

- the arm shows the win, and cang ships it: `--nix=lower:...,upper:...` replaces
  `--nix-lower`, `--fs=` is renamed `--virtiofs=`, existing host-directory
  uppers are migrated once into the disk, `cang-build.raw` grows itself under a
  host-headroom rule, and the live numbers are recorded in
  `docs/images-and-storage.md`;
- or the arm shows no win, and the map ends by recording that number and leaving
  the upper where it is (the flag rename still ships).

"Reached" means the numbers come from live guests, not from a compile or a CI
green.

## Notes

- **This map carries execution** (bob, 2026-11-10): measurement, implementation,
  docs and the release are in scope, not only decisions - the destination is a
  shipped change or a recorded no.
- Tracker: **local markdown**, this directory. `gh` is unauthenticated in these
  sessions; bob pushes branches and tags. Land through `main`.
- Skills every session should consult: `cang-virtiofs-share-topology-and-flags`
  (the share, which knobs are per-connection vs per-device, how a second export
  is designed), `virtiofs-overlayfs-upper-layer-support` (the overlay upper on
  FUSE, the fork's `RENAME_WHITEOUT`, the `ovl_make_workdir` probes),
  `cang-live-guest-evidence-harness` (delegated arms, fresh-log check, clean
  window), `cang-change-verification-gates` (fmt/clippy/nixfmt, artifact
  attribution), `cang-raw-disk-resize-maintenance` (the raw-disk grow shape and
  its rerun defect), `cang-host-state-and-storage` (state root vs graphroot,
  reclaim), `cang-wayfinder-corpus-work` (this corpus's conventions).

- **Destination decisions (bob, 2026-11-10):**
  - **measure first, then ship** - a no-code A/B decides, and the map ends at
    the shipped change or at the recorded number.
  - **one flag surface**: `--nix=lower:...,upper:...` only, `--nix-lower` goes
    away, and `--fs=` is renamed `--virtiofs=` so each subsystem owns a flag.
  - **existing uppers are migrated once** into wherever the upper lands: the
    cached store objects (host: `harmony` 3.0 GB / 2501 store paths) are not
    thrown away.
  - **share `cang-build.raw`** rather than add a disk. **Amended 2026-11-10: no
    auto-grow** - a mid-run grow is impossible while the guest runs (virtio-blk
    takes the disk size at device creation and libkrun has no capacity-change
    path), and growing is manual through the existing
    `cang build-disk resize --size`, which grows the image and the guest btrfs
    in one maintenance VM.
  - **a blk upper forces a blk lower** (bob, 2026-11-10): `upper:blk` with
    `lower:virtiofs` is refused by name instead of served - there is no third
    composition in which the guest overlays a virtio-fs lower, so a new
    read-only virtio-fs bind of `<rootfs>/nix` is not built at all.

- **Starting facts (verified 2026-11-10, this checkout + host):**
  - The guest already composes `/nix` with kernel overlayfs whenever the lower
    is block: `mount_block_lower` in
    `crates/cang-guest-init/src/guest_init/components/nix/root.rs` mounts
    `lowerdir=/run/cang/nix-lower,upperdir=/run/cang/nix-overlay/upper,workdir=...,userxattr`
    over `/nix`, with upper/work reached through the `cang-nix-overlay-src`
    share (`runtime/launch/components/persistent_disks/nix_lower.rs:82-88,202`;
    env contract `crates/cang-guest-init/src/guest_init/components/env.rs:29-35,178`).
    Only the *upper's filesystem* changes; the mount and the overlay plumbing
    already exist.
  - The guest kernel has what this needs: `CONFIG_OVERLAY_FS=y`,
    `CONFIG_BTRFS_FS=y`, `CONFIG_BLK_DEV_LOOP=y`,
    `CONFIG_OVERLAY_FS_UNPRIVILEGED=y`, while `CONFIG_OVERLAY_FS_XINO_AUTO` and
    `CONFIG_BTRFS_FS_POSIX_ACL` are **not** set
    (`deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto:1508,2868,2897,2904`). The
    guest already mounts a writable btrfs raw disk (`cang-build.raw`).
  - The obstacle is the host, not the guest: per-image-generation seeding is host
    directory surgery - `prepare_upper_with`/`seed_from_previous`
    (`crates/cang/src/runtime/session/nix_overlay.rs:318,345`) clone the previous
    generation with `btrfs subvolume snapshot` (`:521`), install the image's DB at
    `.cang-image-db` and park the previous one (`:551`), inside a workspace lease
    (`:46`). A raw image cannot be mounted rootless on the host, so that seeding
    has to move into the guest.
  - The image's own Nix DB is already inside the read-only lower, because the
    lower is `mkfs.btrfs --rootdir=<rootfs>/nix --reflink --shrink`
    (`nix_lower.rs:253`).
  - `tools/nix-generation-smoke` already proves the generation switch end to end
    (generation is a btrfs subvolume -> inode 256, snapshot seed, staged image DB
    consumed, previous DB merged, `verify_bad=0`, `cang-nix-store-repair --dry-run`
    clean). Its README also names a real hazard of exactly this shape: two layers
    numbering inodes from 257 under one synthesized `st_dev` produced a
    wrong-object lookup.
  - The build disk is a fixed 96 GiB sparse btrfs image, one per workspace
    (`persistent_disks/build.rs:31-46,57`), mounted by the guest at
    `/home/dev/.cache/cang-build`; the guest already grows its filesystem to the
    device size on mount (per `build.rs:65-69`), growing the host file is a
    manual `cang build-disk resize --size` (`crates/cang/src/cli/mod.rs:366`), and
    the image has no lock of its own (unlike the lower image's `ImageLock`,
    `nix_lower.rs:109`).
  - On this host the state root is `/home/dev/disp` (btrfs,
    `compress=zstd:1,user_subvol_rm_allowed`, 82 GiB free of 150 GiB). Uppers
    measured: `harmony` 3.0 GB / 2501 store paths, `workspace` 34 MB, every other
    workspace under 3 MB.
  - `--fs=` appears in 39 places across `README.md`, `cli/mod.rs`,
    `runtime/vm/libkrun/launcher.rs`, docs and repository tests.
  - Both overlays pass `userxattr` and nothing else
    (`components/nix/root.rs:440-447`, `session/nix_overlay.rs:890`). In the
    guest the recorded reason is the *upper being a virtio-fs share* - "with
    userxattr (the upper is a virtio-fs share, so overlayfs cannot use trusted
    xattrs there)", `components/nix/root_tests.rs:431-433` - so a btrfs upper
    makes `trusted.overlay.*` available for the first time; no `data=`, `xino`,
    `index` or `metacopy` option is passed anywhere, and the mount is an exec of
    `mount -t overlay overlay -o ... /nix` (`root.rs:422-437`), not a syscall.
  - Today a failed block-lower mount is **swallowed**: the guest logs
    "read-only /nix lower unusable, keeping the host-composed overlay"
    (`root.rs:472-478`) and prep still reports Ready. That fallback exists
    because the host overlay is still there; with the upper on the disk it is
    not, so the failure has to become loud or recoverable.
  - No candidate upper filesystem is mounted `nodev` (build disk mounted with no
    options, `components/disk/build.rs:159-161`; containers disk only
    `user_subvol_rm_allowed`, `disk/containers.rs:7`), so a store path holding a
    device node can still be copied up. The build disk is mounted by
    `runtime/cang.rs:200-219` before the nix-prep worker starts (`:291-293`), and
    that mount is a tolerated *attempt* (`cang.rs:210-218`).
  - The pairing is not unprecedented in this tree: the legacy
    `CANG_NIX_OVERLAY=1` path mounts a guest btrfs disk at `/run/cang/nix-disk`
    and overlays `/nix` over it with the same option string
    (`components/nix/root.rs:225-264`). It is dead on a normal launch, but it is
    working code of an overlay with a btrfs-in-raw-image upper.
  - Every *live* overlay upper in the repo today is a host btrfs
    directory/subvolume, and the task rootfs is a btrfs snapshot rather than an
    overlay (`runtime/session/mod.rs:529-531`). Guest Podman storage is the btrfs
    driver, not overlay (`components/podman/config.rs:41-47`).
  - **Verified 2026-11-10 against the pinned tarball** (note 02, spot-checked by
    hand in `linux-7.3.1/fs/overlayfs/super.c`): the fatal requirements branch is
    `if (ovl_dentry_remote(ofs->workdir) && (!d_type || !rename_whiteout ||
    ofs->noxattr))` (`super.c:759-769`), so a local btrfs upper never trips it; and
    `if (ofs->numfs - !ovl_upper_mnt(ofs) == 1)` forces `xino_mode = 0` with
    "xino=on is useless with all layers on same fs, ignore" (`super.c:1144-1147`),
    which is why two btrfs *subvolumes* collide and two independent btrfs
    *filesystems* cannot.
  - The price of a separate upper filesystem: **copy-up cannot reflink across two
    btrfs filesystems** (`-EXDEV`, `fs/btrfs/remap_range.c:384-385`), so a
    copied-up lower file costs a full data copy - size the disk for real data, not
    for shared extents. A full raw host file surfaces as `-EIO`, not `-ENOSPC`;
    btrfs data reservations never overcommit (`-ENOSPC` at write), while metadata
    overcommit can abort a transaction and flip the filesystem read-only.
    Overlayfs translates none of it (`fs/overlayfs/*` has no `ENOSPC`/`EDQUOT`
    site).
  - **Measured 2026-11-09 (note 03): a btrfs subvolume root cannot be an
    overlayfs upperdir.** Every write through the composed overlay returned
    `EXDEV` (mkdir, touch, cp, unlink) with nothing in `dmesg`; a plain directory
    on the same filesystem, same lower and same options, works (A/B in one guest).
    The subvolume belongs to the *generation* directory, with `upper` and `work`
    plain directories inside it - which is also the host's own seeding shape.
  - The smoke scored that broken run **PASS**: `tools/nix-lower-cang-smoke` read
    the mount table and the store, never the guest's own verdict. It now requires
    `state=ready` in `/run/cang/nix-prep.status`.

## Decisions so far
- [Where the upper lives on the shared build disk, and how that disk grows](tickets/04-upper-home-and-auto-grow.md):
  one generation directory per **image digest** inside the build disk
  (`nix-overlay/<digest-key>/{upper,work}`, with the **generation** directory the
  btrfs subvolume and `upper`/`work` plain dirs inside it - a subvolume root
  cannot be an upperdir, note 03), and **no auto-grow**: manual `cang build-disk resize
  --size`, which already grows image + guest btrfs. A mid-run grow is impossible
  (virtio-blk capacity is fixed at device creation).

- [Does a btrfs upper meet every overlayfs requirement in the guest kernel we ship?](tickets/02-overlayfs-btrfs-upper.md):
  yes, unconditionally - the only fatal `upper fs missing required features`
  branch is gated on the upper being *remote*, so a local btrfs upper is checked
  as warnings only, and no new mount options are needed. The real constraint is
  **placement**: the upper and work must sit on one mount, on a filesystem that is
  not the lower's, with the store tree inside one subvolume - because the
  `257`/`257` inode-identity bug is the *same-superblock* case (two subvolumes of
  one btrfs), where overlayfs trusts inode uniqueness and ignores `xino`.
- [One flag per subsystem - the `--nix=lower:...,upper:...` grammar and the `--fs=` rename](tickets/03-flag-surface.md):
  `--nix=KEY:VALUE` repeatable, keys `lower`/`upper`, values
  `auto|blk|virtiofs`; contradictions rejected at clap parse time and unservable
  block layers at launch with a named reason; the axes resolve **pair-wide**
  (`blk` anywhere means both block, `virtiofs` anywhere means both virtio-fs) and
  `auto` walks both-block -> today's `lower:blk` + host upper (for images that
  predate the upper contract) -> both virtio-fs. `--nix-lower`, `--fs=` and the
  `CANG_FS_*` env vars are removed outright, no aliases.
- [Implement the `--nix=` axis and retire `--nix-lower`](tickets/10-nix-axis-and-retire-nix-lower.md):
  the grammar and pair-wide resolution are in (`blk` on either axis moves both,
  `virtiofs` on either keeps both on the host share, disagreeing is a named
  error), `--nix=upper:blk` is refused by name until the disk exists, and
  `--nix-lower` is gone with no alias. **Amended 2026-11-10 (bob):** the two
  layers must be one filesystem family, so a block lower beside a host-share upper
  - today's default - is refused until the upper disk lands, and `auto` resolves to
  both virtio-fs in the meantime (commit 74702df).

- [What does the writable /nix upper cost today, and what would the block disk save?](tickets/01-upper-write-cost.md):
  the **`>= 20 %` write threshold is not cleared**: the one uncontended A/B pair
  (4 arms, after a co-tenant left) puts the block-disk upper **10.9 % ahead** on a
  684-path / 7.98 GB `nix copy` (paired -4.0 % / -17.6 %), while three contended
  sets straddled zero (+24.7 %, -6.5 %, +14.5 %). What the move *does* buy, and now
  uncontended, is metadata and GC: walk **7.7x**, `du` **15.4x**, delete **13.3x**
  faster, so write+delete is 166.4 s (virtio-fs upper) against 103.3 s (block
  upper). Two limits stay open: the workload is **12.06 MB per path**, 10x fatter
  than the harmony upper's ~1.2 MB, and a metadata-*shaped* arm is impossible with
  this image (685 store paths; any closed 3 GB subset expands back to 684 paths).
  **Overridden by bob (2026-11-10): the write+delete cycle is the metric, not the
  bare write** - 37.9 % cheaper on the block upper - so the route continues and the
  design tickets are unblocked. The 12.5 % cold-re-read regression is carried
  forward as a cost.

## Not yet specified

- The implementation slices themselves - host disk preparation and the
  `--nix=`/`--virtiofs=` wiring, the guest-side seeding step, the migration
  path, docs/README, patch and release. Each can be sliced only once the
  flag-surface, upper-home, seeding-parity and migration tickets have settled the
  shape it implements.
- Whether the host overlay and its workspace lease are skipped entirely once both
  axes are block - a large host-side simplification that rides on the
  seeding-parity ticket (the implementation ticket assumes they are).
- Failure semantics when there is no host overlay left to fall back to: today a
  failed block-lower mount keeps the host overlay and reports Ready
  (`components/nix/root.rs:472-478`); with both axes block the fallback must be
  named, retried or refused instead of silently degraded.
- Whether `find /nix/store` keeps its 206-215 ms once the upper is btrfs rather
  than FUSE - the merged walk is the number the lower decision turned on.

## Out of scope

- Moving `/workspace` off virtio-fs: settled - the workspace stays a live share,
  and `target/` plus `~/.cargo` already sit on the block disk.
- Re-litigating the read-only lower: shipped default-on, measured at 206-215 ms
  against 864-2033 ms.
- Sharing the host's `/nix/store` into the guest, or changing substitution
  policy and parallelism - a different lever on a different cost.
- The FUSE-level tuning surface for `/workspace`; that has its own map
  (`../workspace-virtiofs-performance`).
