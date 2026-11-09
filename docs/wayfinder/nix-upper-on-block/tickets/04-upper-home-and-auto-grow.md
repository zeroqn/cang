---
label: wayfinder:grilling
title: Where the upper lives on the shared build disk, and how that disk grows
status: open
blocked_by: ["01-upper-write-cost"]
claimed_by: ""
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
