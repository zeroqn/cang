---
label: wayfinder:task
title: Retire the /nix block lower and block upper - one fewer disk, not one more
status: open
blocked_by: []
claimed_by: ""
---

## Question

bob, 2026-11-20: **"remove `/nix` lower and upper blk support. Manually grow disk
is painful. Fewer blk is better."** `/nix` goes back to the host-composed overlay
over the single virtio-fs connection, and the block-disks it grew disappear.

Do:

1. **Remove the surface**: `--nix=lower:blk` and `--nix=upper:...` (grammar at
   `cli/mod.rs:2770`, parse test `cli/mod.rs:1171`, strict-blk refusal
   `nix_lower.rs:409`), the `CANG_NIX_LOWER_BLOCK` guest contract, the block image
   preparation (`prepare_nix_lower_block`, `persistent_disks/mod.rs:64`), the
   profiler metadata (`session/mod.rs:264-287`), and the guest-side block-lower/upper
   mount steps. Keep the host overlay path (`runtime/session/nix_overlay.rs`) as
   the only one.
2. **What remains of `--nix`**: `lower:auto|virtiofs` and `upper:` modes - or does
   the axis collapse to nothing? Decide and say so; `--nix-lower` is already retired
   by the `nix-upper-on-block` map's ticket 10.
3. **Migration**: an existing state root carries `cang-build.raw`, a nix-lower
   image and lease files (`nix-overlay.lease.*` seen in a real root). What removal
   does to a launch that has them, and what the user is told - never a silent
   fallback to the block disk.
4. **The number that could reopen it**: measure a store-read-heavy guest on the
   host overlay (the `1500 /nix reads` profile from the map's cache-symlinks work)
   and record it. If the store is measurably slower, that is a *share* question for
   the map's fog, not a reason to keep the disk.
5. **Docs and tests**: `docs/usage.md`, `docs/images-and-storage.md`,
   `crates/cang-repository-tests` (which pins the btrfs helper symlinks and the
   image layer list), and the `nix-upper-on-block` map, which is withdrawn in part
   by this.

## Resolution

(to follow)
