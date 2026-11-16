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
- [How an existing host-directory upper becomes guest-side state](tickets/06-migrate-existing-uppers.md):
  the first block boot imports the host share's cache **once** - `store` and
  `var/nix/db` only, because a whole-tree `cp -a` aborts on the live
  `var/nix/daemon-socket/socket` - and a sibling generation on the disk always wins
  (a snapshot beats a copy). Retired generations are pruned at seed time: keep the
  one that booted plus the newest other completed one.
- [Seeding parity - does the guest reproduce the host per-generation seeding, or reconcile instead?](tickets/05-seeding-parity.md):
  **parity, done in the guest.** A fresh digest is a `btrfs subvolume snapshot`
  of the previous generation with its work dir emptied, the image's DB staged from
  the read-only lower and the clone's parked for the existing post-mount merge;
  a seeded generation touches nothing. Proven live for fresh, image move
  (`Cloned(…, Snapshot)`), returning to an older digest, and repairing a
  generation the earlier subvolume-root build had made unusable.
- [Implement the guest upper disk behind `--nix=upper:blk`](tickets/09-implement-nix-upper.md):
  shipped and default - a launch with no `--nix` flag composes `/nix` as
  `lowerdir=/run/cang/nix-lower,upperdir=<build-disk>/nix-overlay/<digest>/upper`.
  Left for later: migrating an existing host-directory upper, and pruning retired
  generations.
- [The acceptance arms - prove upper:blk end to end in live guests](tickets/07-acceptance-arms.md):
  five of six arms green on live guests (generation contract, write path on the
  disk, no inode collision, metadata numbers, failure paths); the migration arm was
  not run because the migration is not implemented. Cost: cold data reads 8932 ms
  on the disk against 5894 ms over virtio-fs - **re-attributed by
  [note 04](notes/04-cold-read-attribution.md)**: that pair came from a metric
  with an uncontrolled host page cache; measured properly the two arms coincide
  cold and the block lower wins warm.
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
  design tickets are unblocked. The 12.5 % cold-re-read regression is **withdrawn**:
  [note 04](notes/04-cold-read-attribution.md) measured both arms at 70-165 MB/s
  cold (one wall: the host's storage), the block arm ahead on warm reads
  (921 vs 583 MB/s) and 6-9x ahead on metadata, with no guest-side knob moving a
  cold read.

## Not yet specified

- **Overlayfs is free; `/nix` is 2.4x slower because it is virtiofs, and the guest cannot
  compose an overlay because the build disk is corrupt** - measured 2026-11-16,
  [note 25](notes/25-overlay-is-free-nix-is-virtiofs-because-vdb-is-corrupt.md): an overlay
  over the block lower with a tmpfs upper walks the same 3 897 files in 442-472 ms against
  the raw lower's 422-559, and issues the same 941 device reads, so the union layer costs
  nothing; `/nix` meanwhile walks in **1 239 ms** because `stat -f /nix` is `fuse` and there
  is no `/nix` mount at all. The guest never composed the overlay because `/dev/vdb`
  (`LABEL=CANG_BUILD`) refuses to mount - `fsconfig() failed: Structure needs cleaning`
  (EUCLEAN, very likely from this session's repeated `kill -9` of microVMs) - so every
  candidate upper was a fuse path, and overlayfs refuses a fuse upper (EINVAL). Nix's own
  `local-overlay` store is overlayfs too and has the same upper requirement, so it cannot
  remove a cost that is already zero. Rebuilding the build disk is the next step and needs an
  explicit go-ahead.

- **A warm host saves 15-20 % of the walk; `/nix` in the harness boots is virtiofs, not the
  guest's overlay** - measured 2026-11-15,
  [note 24](notes/24-warm-host-cache-and-nix-is-virtiofs.md): with the guest's page cache
  dropped every arm, a warm *host* image cache costs 375-380 ms against 450-500 ms cold
  (~98 ms, matching the ~100 us of host service per read) while the guest still issues all
  941 requests - so a second launch gains a fifth, not a different league. The same boot
  exposed a control defect (control11 dropped for every arm, so the first attempt's "warm"
  arms were cold - the proof line `@@ w1 warm_no_drop` must be asserted) and that `/nix` is a
  fuse mount in these boots (`stat -f /nix` = fuse, no `/nix` mount point), so neither the
  guest-composed overlay nor `--nix=upper:blk` has actually been exercised yet.

- **No mount option and no guest-local copy moves the cold walk; the lever left is cache
  lifetime** - measured 2026-11-15, [note 23](notes/23-no-mount-option-and-no-locality-gain.md):
  `noatime` is a no-op on a read-only mount (941 device reads, 451-528 ms either way), a
  guest-local `cp -a` of the 8 GB store walks **1 583-1 769 ms** against the image's
  494-528 ms, and bulk reads are device-bound (639 MB at 444 MB/s with service overlapping
  wall), so libkrun's staging copy is not the lever on a healthy host either. The row of
  refuted candidates is now complete (mount options, uncompressed image, erofs, local copy,
  direct-to-guest, vhost-user, DAX); the only measured win is a **warm cache at 75-101 ms**,
  which makes cache lifetime - not transport - the thing to optimise.

- **Tried the erofs lower end to end: it is 2x the requests and 3-4x slower. Reverted** -
  measured 2026-11-15, [note 22](notes/22-erofs-lower-is-slower.md): a full implementation
  (host `mkfs.erofs -T 0`, `CANG_NIX_LOWER_FSTYPE`, `mount -t erofs`, a labelless-disk
  lookup because an erofs image carries no label, `erofs-utils` in the helper dir) mounted
  and worked, and cost **1 940 device reads / 1 355-2 389 ms** for the 3 897-file cold walk
  against btrfs's **941 reads / 362-500 ms** - erofs metadata blocks are 4 KiB where btrfs
  packs 16 KiB nodes, and per-request round trips are the whole cost here. This **retracts
  note 18's 1.5-1.8x erofs win**, whose erofs arms were reading a loop-mounted copy that
  the cache control never dropped. btrfs stays; the patch is kept at
  `blkread/erofs-lower-switch.patch`; every future lower comparison must record
  reads-per-walk, the column that would have caught both mistakes.

- **Removing the image's compression costs 2.7 s of build and no disk, and buys ~30 % of
  host device service but ~0 end to end** - measured 2026-11-15,
  [note 21](notes/21-compression-cost-and-build-cost.md): the real store builds in 37.3 s
  reflinked (62 898 `encoded` extents) against 40.0 s copied (0 encoded), both 7.8 GB on
  disk - so the earlier "+4.5 GB" here was wrong. In the guest, dense random 16 KiB reads
  drop the host's device service from 84-162 ms to 60-68 ms per 300 reads, while
  end-to-end p50 stays 278-626 us either way: at this read size the round trip through
  virtio-blk and the guest's btrfs path dominates. That is why the metadata walk showed no
  gain, and it leaves **request count** as the only remaining lever (what note 18's
  erofs-plain result looks like).

- **An uncompressed btrfs lower does not speed up the metadata walk; reverted** - measured
  2026-11-15, [note 20](notes/20-uncompressed-lower-no-gain.md): the image was rebuilt
  genuinely uncompressed (303 extents, 0 `encoded`, needs `compression=none` *and* no
  `--reflink`, because a reflink clones the snapshot's compressed extents) and the cold
  `find` came out 411-455 ms against the compressed image's 362-500 ms, with host-side
  16 KiB random reads identical too (mean 388/257 vs 510/288 us). So note 17's 2.0-2.8x
  per-read penalty does not transfer to the real image and must not be quoted for it.
  Dropping `--reflink` also ends extent sharing with the snapshot (~8 GB of its own disk
  per image), so the edit was reverted. What remains unexplained, and the only lead left
  for the metadata cost, is erofs's *layout* (fewer read requests), not compression.

- **The btrfs lower is compressed (71.8 % of extents `encoded`, 12.4 GB -> 7.9 GB), and
  keeping btrfs is compatible with fixing it** - measured 2026-11-15,
  [note 19](notes/19-btrfs-lower-is-compressed.md): compression is a per-file/subvolume
  property, so an uncompressed btrfs image needs no format change, and the same-filesystem
  A/B (note 17) already shows 2.0-2.8x per small cold read. But the disk ranking inverts
  the usual argument - btrfs wins on space only while compressed (7.9 GB) and uncompressed
  btrfs is the largest option (12.4 GB, +4.5 GB per image, three identities), while
  uncompressed erofs is smaller (8.4 GB) *and* 1.5-1.8x faster than compressed btrfs. So
  reflink/snapshot semantics are the real argument for btrfs, not space.

- **(#1) An erofs lower is 1.5-1.8x cheaper than btrfs for the cold metadata walk, and
  lz4 gives it back** - measured 2026-11-15, [note 18](notes/18-erofs-lower-metadata.md):
  four repeats of the same 3897-file cold `find` gave btrfs 362-447 ms, uncompressed
  erofs 240-250 ms, lz4 erofs 416-440 ms - reproducing note 17's compression penalty
  inside a read-only image format. The erofs arms were *penalised* (loop device over a
  file on the host-compressed state root), so 240 ms is a lower bound on the gain.
  Banking it needs a cang change: build the lower with `mkfs.erofs -T 0` and mount it
  read-only in the guest half where the btrfs lower is composed today. Run limitation:
  root-device parsing gave `root`, so this boot has no per-arm device counters.

- **(#2) The per-read cost is the state root's `compress=zstd:1`, not the guest** - measured
  2026-11-15, [note 17](notes/17-per-read-cost-is-fs-compression.md): same disk, same
  subvolume, same 2 GiB of bytes, cold each time - compressed (`encoded` extents, 5802 of
  them) p50 240/215 us per 16 KiB read against uncompressed 116/78 us, with `nodatacow`
  adding nothing. That plausibly accounts for half to two thirds of the 316 ms cold
  metadata penalty (941 reads). The lever is image creation: write the `nix-lower` image
  with `compression=none` or in a subvolume whose property is `none`, and decide per path
  - the measured slice did not compress at all (2.0 GB either way) while the whole image
  saves ~34 %, so every small read currently pays decompression for a partial benefit.
- **Prior art exists for (#1)** - `blkread/erofs/{lz4,zstd}.log` and `fsimg/` from
  2026-11-10 built erofs images of the same store with mkfs.erofs 1.9 (plain/lz4/zstd,
  lz4c64/c256); their payloads were removed for disk space, the logs kept.

- **The queue knobs do not touch cold metadata; host service per read does** - measured
  2026-11-15, [note 16](notes/16-queue-knobs-do-not-move-metadata.md): eight cold `find`
  arms swept `read_ahead_kb` (128-4096) and `max_sectors_kb` (512-4096) and every arm did
  exactly **941 read I/Os and 30 112 sectors**, so neither knob merges or removes a
  metadata read. What moved instead is host service time for that identical work:
  186-498 ms (2.7x) within one boot, 197-529 us per read, wall tracking it
  (`wall = device read_ms + 187-390 ms`). Consequence: the remaining levers are on the
  image/host side - fewer metadata blocks (denser index stream, e.g. erofs/squashfs) or
  cheaper per-read service (the `.raw` sat on a 93 %-full btrfs, and the 2.7x spread
  makes any guest-side number here unstable to ~2x). Recorded defect: the `base*` arms
  set read-ahead 128 while the stock value is 8192, so the baseline label is wrong even
  though the invariance result stands.

- **(a) The cold metadata penalty is 941 serialized device reads (~336 us each), not CPU** -
  measured 2026-11-15, [note 15](notes/15-cold-metadata-is-device-reads.md): a cold
  `find` of 3897 files costs 341-370 ms and moves only ~482 KB in 941 read I/Os, warm is
  54 ms with 0 reads, and forcing a stat of every file adds nothing - so the cost is
  read count and latency, not kernel work (the cold arm spends 8-9 ms of CPU). `perf` at
  999 Hz costs 900-1900 ms on the same arm, which is why the counter comparison, not the
  profile, is the result. Candidate levers in order of cheapness: `read_ahead_kb` on the
  block device (default 128 KB), `max_sectors_kb` for metadata (never swept), or an image
  layout with fewer metadata blocks.
- **(b) DAX is not a lever here** - same note: the binary has no DAX surface at all, and
  the arithmetic is against it - block mode already spends only 8-9 ms of CPU in a
  46-51 ms 128 MiB read with device service as the floor, while DAX is a virtiofs feature
  and that path is 2.1-3.0x slower than block to begin with.

- **Real workload, clean host: the block lower beats virtiofs 2-3x on cold mmap reads** -
  measured 2026-11-15, [note 14](notes/14-block-vs-virtiofs-real.md): identical bytes
  through mmap page faults (the shape exec and nix build use), identical checksums,
  block 46-51 ms per 128 MiB single-reader (2.5-2.8 GB/s) and 190-216 ms for 768 MiB at
  six readers against virtiofs 102-144 ms and 367-758 ms; metadata sweep 309 ms cold vs
  54 ms warm on the block lower. Host gated at 12 GB MemFree with no live VM, and every
  cold arm records its own device reads. This settles the mode question for /nix on a
  clean host (keep block) and leaves the metadata cold penalty as a separate question.

- **CORRECTION: the ~200 MB/s "wall" was host memory pressure** - measured
  2026-11-15, [note 13](notes/13-host-pressure-artifact.md): with the host at 10.9 GB
  free instead of 1.6-2.7 GB, the same cold 128 MiB read takes 38-70 ms (1.8-3.4 GB/s)
  and a four-reader 512 MiB arm 148 ms (3.45 GB/s), with guest device time the same
  order as the wall, and a RAM pre-touch no longer changing anything. Notes 11 and 12
  therefore describe a pressured host, not cang's block lower. The profile taken at
  device speed is dominated by `_copy_to_iter` (21 % at one reader, 45 % at four) with
  btrfs checksum `memcmp` and page allocation behind it - which is both the one item
  DAX would remove and an artefact of the pread instrument, since exec/build mmap and
  fault instead. Rule: every arm records host `MemFree`/`AnonPages` and an arm taken
  under host pressure is invalid, not noisy.

- **The buffered read wall is guest page allocation, not the host** - measured
  2026-11-15, [note 12](notes/12-read-wall-is-page-allocation.md): the backing serves
  130 MB in 210-286 ms (~500 MB/s) while the host state device moves <1 MB and
  `mincore` on the image reads 0 resident pages before and after every arm; cached
  re-reads do zero device reads and are CPU-bound copies at 221-867 MB/s; and writing
  and freeing 3 GiB of guest RAM first (so its pages are already faulted on the host)
  cuts the cold arm 1053 -> 690 -> 494 ms with device service unchanged. Device
  efficiency is thus a ~210-290 ms term of a ~1 s cold read, which is why the
  candidates for this cost are guest-RAM pre-fault (the held `CANG_GUEST_RAM_POPULATE`
  prototype) and /nix DAX, not a different block backend. `O_DIRECT` arms are a
  transport ceiling only: cang cannot impose the guest application's open flags.

- **The depth ceiling is the guest's buffered read path** - measured 2026-11-15 with a
  per-reader instrument and a mincore-verified host drop,
  [note 11](notes/11-guest-read-path-ceiling.md): buffered reads cap at ~200 MB/s and
  do not scale (tmpfs 209 MB/s at one reader, 207 aggregate at four; the block lower
  144 -> 62 MB/s at seven readers with 2.9 -> 65 ms of guest device time per MB),
  while the same bytes under `O_DIRECT` run 426 MB/s at one reader and 3029 MB/s
  aggregate. So the transport is fast and the guest page-cache fill is the wall - which
  is why removing libkrun's staging copy helped one reader and not eight, and why a
  vhost-user backend cannot fix it. Open: the host served up to 3 GB/s while `mincore`
  reported 0 resident pages of every `nix-lower` image and the state device moved
  <50 MB, so host coldness is not yet provable from these arms.

- **Direct-to-guest reads on the device thread** - measured 2026-11-15,
  [note 09](notes/09-direct-to-guest-reads.md): with the pooled staging copy removed
  (reads served on the block worker thread, engagement proved by the read-completion
  trace going to **0** lines per arm), one reader improves 5.1-11.1 s to 3.9-4.2 s
  for 610 MB, and eight concurrent readers do **not** improve (37.7-46.9 s against
  22.3-50.8 s) because one thread then issues every pread - 235-241 s of accumulated
  device service against a 37-46 s wall. The collapse therefore needs *both* no
  staging copy and parallel preads: pool workers writing guest memory directly
  (needs a non-overlap proof) or a vhost-user-blk backend that preads into the
  guest's descriptors. Also recorded: cang forces
  `set_parallel_reads(true)` in `crates/cang-libkrun/src/linked.rs`, so a fork-side
  constructor default is dead code on cang's path.

- **The block lower against virtio-fs for the same cold bytes** - re-tested 2026-11-15
  with the host cache control *verified* rather than assumed,
  [note 08](notes/08-cold-read-route-and-concurrency.md): the block lower delivers
  109-115 MB/s where virtio-fs delivers 176-194 MB/s for the same 610 MB in one boot
  (both host-cache-hot), eight concurrent readers *lower* the block path to 58-74 MB/s
  aggregate with 217-332 % device busy and 24-30 s guest iowait, and the guest is never
  CPU-saturated (5-22 % of 26 vCPUs, `pgsteal` 0). The guest's warm ceiling is 8.7 GB/s,
  so the loss is in delivery, not guest copy - which is also why a vhost-user prototype
  was declined (four virtqueues already measured no better; the transport is a few
  percent of the arm against a 25-37 % spread). Open: *where* in delivery, which needs
  the per-request trace of a HEAD build (`CANG_BLK_TRACE`), absent from the prebuilt
  used for these arms.

- **The cold-read wall's mechanism** - *not* found, 2026-11-10,
  [note 07](notes/07-guest-o-direct.md). Solid facts: a guest `O_DIRECT` read of the
  lower is 8-20x faster than a buffered one at every block size, on both kernel
  lines, with the host cache verifiably cold and identical device bytes; host
  `O_DIRECT` (`BlockDevice::set_direct_io`, wired behind `CANG_BLOCK_DIRECT_IO`) makes
  no difference; buffered arms correlate with host anonymous allocation and
  compaction but pre-filling the guest's own memory (1 GiB, held by the host) does
  not speed the read, so that causal story is retracted. Remaining candidates: the
  guest's readahead request shape (2.6 MiB at 34 ms each vs 0.5 MiB at 0.4 ms), and
  the fork's multi-region descriptor split. **Then answered** (same note, per-request
  instrumentation in the fork's block worker): the stable cost is guest-side - 5.4-6.9 s
  of every buffered arm against 0.24 s of every `O_DIRECT` arm - with the host's own
  read+copy at 0.9-4.9 s of a 6-10 s arm. The wall is the guest kernel's buffered
  readahead path for this device, so no cang launch setting closes it. The last
  mechanical suspect - a single virtqueue (`NUM_QUEUES = 1`) serialising submission -
  was then tested with a four-queue device plus `VIRTIO_BLK_F_MQ` (guest verified
  `mq_dirs=4`, 128 tags each): no change, 5.1-5.9 s guest-side against `O_DIRECT`'s
  0.25 s, and the change was reverted. `MADV_POPULATE_WRITE` on the guest RAM regions
  was the last hypothesis tested (arm proven by worker RSS reaching 4.01 GiB at t=8 s
  against 0.84 GiB lazily): no change either, and the copy-into-guest-memory cost
  survives it, so that cost is the guest's own page allocation, not host faults. No
  product change is justified.
- **The 29 ms-per-request cold-read wall itself** - attacked 2026-11-10 with two
  hypotheses and a third finding, [note 06](notes/06-read-wall-attribution.md):
  `max_sectors_kb` 512/128 makes it worse (48.9 / 51.5 s against 39.5 s at 4096),
  image fragmentation is refuted (the same fragmented image streams at 1.4 GB/s
  `O_DIRECT`, 3.4 GB/s with 8 readers, from inside the guest), and a 22x
  buffered-vs-`O_DIRECT` gap measured in one boot did not reproduce in another
  (ext4 arm: 32 buffered / 58 direct / 85 nocache). Open only because the host
  page cache cannot be dropped from this account; the settling measurement is
  host-side `diskstats`/PSI sampled per arm.
- **A packed (erofs) lower instead of the btrfs `.raw`** - prototyped and then
  settled 2026-11-10, [note 05](notes/05-erofs-packed-lower-prototype.md):
  `mkfs.erofs` from the image's own store packs 7.69 GiB into 4.57 GB (lz4 4 KiB),
  4.20 GB (lz4 64 KiB chunks) or 3.22 GB (zstd), loses no pages, and mounts
  read-only on the shipped kernel for lz4. It is still the wrong trade: on the
  shipped firmware 13 % slower cold than the btrfs lower, and with
  `CONFIG_EROFS_FS_ZIP_ZSTD` + `CONFIG_EROFS_FS_PCPU_KTHREAD` rebuilt into a
  substituted firmware (`bed03226…`, which does mount zstd and does print
  `initialized per-cpu workers successfully`) **1.7-2.2x slower** — zstd 50-69 s
  against the lower's 25-36 s on the same 2.56 GB manifest, with 46 % fewer device
  bytes. Decompression is the limit (~95 MB/s per core), not bandwidth. Not a
  candidate unless the read set stops being a handful of multi-gigabyte files.
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

## Out of scope

- Moving `/workspace` off virtio-fs: settled - the workspace stays a live share,
  and `target/` plus `~/.cargo` already sit on the block disk.
- Re-litigating the read-only lower: shipped default-on, measured at 206-215 ms
  against 864-2033 ms.
- Sharing the host's `/nix/store` into the guest, or changing substitution
  policy and parallelism - a different lever on a different cost.
- The FUSE-level tuning surface for `/workspace`; that has its own map
  (`../workspace-virtiofs-performance`).
