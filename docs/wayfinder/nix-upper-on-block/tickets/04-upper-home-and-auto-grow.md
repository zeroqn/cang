---
label: wayfinder:grilling
title: Where the upper lives on the shared build disk, and how that disk grows
status: closed
blocked_by: ["01-upper-write-cost"]
claimed_by: "pi (bob, 2026-11-10)"
---

## Question

The upper and its `work` dir become a subvolume tree inside `cang-build.raw`
(bob's choice, 2026-11-10: share the disk rather than add one, and make it
auto-grow). That disk is currently a fixed 96 GiB sparse image, one per
workspace, mounted by the guest at `/home/dev/.cache/cang-build`, its filesystem
grown to the device size only when the guest mounts it
(`persistent_disks/build.rs:31-46,65-69`), with a manual
`cang build-disk resize --size` as the only grow path (`cli/mod.rs:366`) and no
lock of its own.

Settled constraints from the overlayfs fact-find
([note 02](../notes/02-overlayfs-on-btrfs-upper.md)) that the layout must satisfy:

- the upper and `work` go on **one mount**, on a filesystem that is **not the
  lower's**, with **the store tree inside one subvolume** - two subvolumes of one
  btrfs are one superblock to overlayfs, which then trusts inode uniqueness and
  ignores `xino`, which is exactly the `257`/`257` wrong-object bug
  `tools/nix-generation-smoke`'s README records;
- `userxattr` stays (no flag change), and no `nodatacow`, no bare `space_cache`,
  no `compress` and no `acl` are wanted on the upper;
- copy-up cannot reflink across two btrfs filesystems (`-EXDEV`), so the upper
  holds **real data**, not shared extents - the disk size must be budgeted as
  such;
- a full raw host file surfaces as `-EIO`, not `-ENOSPC`; btrfs itself returns
  `-ENOSPC` at write and can flip read-only when metadata overcommits, and
  overlayfs translates neither.

1. **Layout inside the filesystem.** `cang-build.raw` currently holds `target/`
   and `cargo/` at its root. Where do the upper generation, its `work`, and the
   parked/staged DB directories go - a dedicated parent subvolume (e.g.
   `nix-overlay/`) with a per-image-generation subvolume inside it, mirroring
   today's `$STATE/cang/<slug>/nix-overlay/{upper,work}` shape? What is the
   guest path the overlay mounts (`upperdir=/home/dev/.cache/cang-build/nix-overlay/<gen>/upper`)?
   Does it stay invisible to the workspace share, and does it survive `cang reset`
   the way the cargo cache does or get wiped?
2. **Auto-grow rule.** Bob's constraint: the disk may not grow into the host's
   own btrfs - headroom must always remain. What rule: extend the sparse file at
   *launch* to `min(needed, host_free - headroom)` with `headroom = max(32 GiB,
   15 %)`, or watch usage during the run? Who grows the filesystem afterwards
   (the guest already grows on mount - does a mid-run extend reach it)? What is
   the cap on the image itself? What happens when the headroom rule forbids a
   grow: refuse the launch, warn, or run and fail at ENOSPC?
3. **What the grow mechanism is.** If launch-time extension is enough, does
   `cang build-disk resize` survive as a manual override (and does it grow to
   "as much as the headroom allows")? Does the extension need a lock - a second
   launch on the same state root extending a file a live VM has open? (The lower
   image solved this with `ImageLock`, `nix_lower.rs:109`.)
4. **`cang reset` and removal.** Which command removes the upper cache now that
   it is inside the disk: `cang reset`, a new subcommand, or "delete the disk"
   with the cargo cache? Deleting the cache the user does not expect to lose is
   worse than keeping it.

Resolve to a concrete layout, a grow rule with its numbers, and the removal
semantics. Cite the measured upper sizes in the map's Notes when choosing
headroom.

## Resolution

(to follow)

## Resolution

Settled with bob, 2026-11-10.

**Layout.** The upper and work are a generation directory inside the build
disk's own btrfs filesystem, one per **image generation**, next to `cargo/` and
`target/`:

    /home/dev/.cache/cang-build/nix-overlay/<digest-key>/upper
    /home/dev/.cache/cang-build/nix-overlay/<digest-key>/work

The guest is the only writer of that tree. `<gen>` is the btrfs subvolume - it is
what a new generation is snapshotted from - and `upper`/`work` are plain
directories inside it, because **a btrfs subvolume root cannot be an overlayfs
upperdir** (measured, note 03: every write returns `EXDEV`). The host only ever writes the path into the env. **The name is the digest
key, not the workspace's constant `nix-overlay` directory**: the first host cut
keyed on `generation_root.file_name()` and so gave every generation one upper,
which the guest's `already seeded` sentinel then turned into an image move that
silently kept the previous image's Nix DB (fixed in 6f27d85, with a test that
compares two digests). A digest key is reduced to one plain path segment before
it is used as a directory name.

**Growing is manual** (bob, 2026-11-10). A mid-run grow is not available and is
not going to be built: libkrun's virtio-blk takes the disk's size once at device
creation (`deps/libkrun/.../block/device.rs:222,347`, `capacity:
disk_properties.nsectors()`), serves config space from a fixed slice
(`read_config`, `:424`) and has neither `set_capacity` nor a config-change
interrupt, so a host-side `ftruncate` mid-run leaves the guest's device at its old
size and `btrfs filesystem resize max` sees nothing new. Making it real would be
a fork change plus a runtime handle to a device `add_device` has already
consumed.

So: **no auto-grow, no refusal, and no new command.** The disk is already a
96 GiB sparse image against a largest-measured upper of 3.0 GB, and the existing
`cang build-disk resize --size 100G` already grows the host image *and* the guest
btrfs filesystem in one narrow maintenance VM (`DiskKind::BuildDisk` ->
`internal resize build` -> `btrfs filesystem resize max`). The workspace state
root's own headroom is the user's problem only if they never run it; a full disk
surfaces as an ordinary ENOSPC in the guest.

**Removal.** The upper cache dies with the disk: `cang reset`/deleting
`cang-build.raw` in the workspace's state root takes `target/`, `cargo/` and
`nix-overlay/` together, which is the existing documented behaviour of that disk.
No new command, and nothing the user keeps elsewhere is touched.

**Docs.** No change is needed in `cang`'s help: `cang build-disk resize` already
says "Grow the raw build disk and guest btrfs filesystem". `docs/images-and-storage.md`
gains one sentence when the guest half lands (the upper shares the build disk and
its capacity, and how to grow it).
