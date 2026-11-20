---
label: wayfinder:task
title: Remove the --nix CLI axis - a workspace disk means /nix on a block lower with its upper in the tree
status: closed
blocked_by: []
claimed_by: "pi (2026-11-20)"
---

## Question

bob, 2026-11-20, **corrected the same day**: the first shape of this ticket
retired the `/nix` block lower altogether; bob's correction keeps it and makes it
automatic. Verbatim: *"`--nix` cli argument can be removed. workspace raw
automatically mean `/nix` block lower. Otherwise old method virtiofs and overlayfs
in host then mount to `/nix` in cang guest is used."* And on the upper: *"We can
have `/nix` upper in workspace disk, then overlayfs it above `/nix` block lower."*

So the axis collapses into the disk:

| launch | `/nix` |
| --- | --- |
| `--workspace-disk PATH` | guest-composed overlay: read-only btrfs **block lower** built from the image's store, plus `upper`/`work` inside the **workspace btrfs tree** |
| no workspace disk | host-composed overlay over virtio-fs, mounted at `/nix` (today's default) |

`--nix=lower|upper:...` disappears as a surface: no `NixLowerMode`, no
`NixUpperMode`, no `CANG_NIX_LOWER_BLOCK` mode flag deciding *whether*, and the
`--workspace-disk` + `--nix=lower:blk` refusal added in ticket 18 goes away with
the flag it refuses.

Do:

1. **Remove the surface**: the `--nix` flag and its grammar (`cli/mod.rs:2770`),
   `NixLowerMode`/`NixUpperMode`, the parse tests, and the strict-blk refusal
   (`nix_lower.rs:409`). A launch's `/nix` mode is derived from whether it has a
   workspace disk; there is nothing to opt into.
2. **Move the upper into the tree.** Today the block-mode upper is
   `/home/dev/.cache/cang-build/nix-overlay/<generation>/upper` on the build disk
   (`nix_lower.rs:96`, `:225-235`, `:1132`) and reaches the guest as
   `CANG_NIX_LOWER_UPPERDIR`. With a workspace disk it has to live inside the
   workspace btrfs tree, which the host cannot see - so the **guest** creates
   `upper/` and `work/` there after mounting the disk, and the two dirs must be
   plain directories inside the tree, never a subvolume root (copy-up returns
   `EXDEV` otherwise - `docs/wayfinder/nix-upper-on-block`).
3. **Order the guest steps**: mount workspace -> create the upper/work dirs ->
   mount the read-only lower -> compose the overlay at `/nix`. Today the lower
   and overlay come first and the upper is pre-created host-side.
4. **Generation keying.** The lower is per store generation; an upper accumulated
   against an older lower must not be reused blindly. Recommendation: key the
   upper directory by generation inside the tree (`/workspace/.cang/nix/<gen>/`)
   and delete the previous one on switch, the same way the build disk does today.
   **This is the one judgement call in the ticket** - bob may prefer a single
   stable upper that survives image rebuilds.
5. **What a missing lower does**: the image cannot be built (host space) or the
   guest cannot mount it. Keep today's `auto` behaviour under the new shape -
   serve the host-composed overlay and name the reason - because there is no flag
   left to say "fail instead".
6. **Migration**: an existing state root carries `cang-build.raw`, a nix-lower
   image and `nix-overlay.lease.*` files. A default launch must keep using the
   host overlay and the build disk; a workspace-disk launch ignores them. Never
   silently attach a block device the mode did not ask for.
7. **`cang nix gc` still points at the build disk.** It launches a maintenance VM
   with `cang-build.raw` attached and runs the guest's gc there, so with the upper
   in the workspace tree it collects nothing (recorded as a known gap in
   `docs/usage.md`). Retargeting it means attaching the workspace image and
   refusing when the path is not given.
8. **Docs and tests**: `docs/usage.md`, `docs/images-and-storage.md`,
   `README.md` (`--nix=` prose), `crates/cang-repository-tests`, and the
   `nix-upper-on-block` map (whose ticket 09 is already cancelled and whose
   remaining question - upper on a block disk - is now answered by this ticket's
   step 2).

## Resolution

**Built and live-verified 2026-11-20** in bob's corrected shape (the block lower
becomes automatic, the upper moves into the tree), not the original "retire both".

- The `--nix` axis is **gone** (`--nix=lower:blk` is now a plain clap error): no
  `NixLayer`/`NixLayerMode` grammar, no `NixLowerMode`, no pair-wide resolution, no
  `--workspace-disk` + `--nix=lower:blk` refusal.
- `/nix` is **derived from the disk** (`LaunchPlan.nix_block_lower =
  workspace_disk.is_some()`); the guest composes the overlay over the read-only
  block lower.
- The **upper lives at `/workspace/.cang/nix/<digest-key>/`** - a btrfs subvolume
  whose `upper`/`work` are plain directories inside it (the only copy-up-legal
  shape; a subvolume root EXDEVs), seeded by the guest right after it mounts the
  workspace disk and before it composes the overlay, generation-keyed so a lower
  change cannot reuse a stale cache.
- An unavailable lower **keeps the launch** (host overlay, named reason on stderr):
  no flag remains that could ask for a refusal.
- `cang disk reset` cannot reach a workspace image: it offers only `--containers`/
  `--force`, pinned by a CLI test.
- Bonus from the same arm: the launch's `~/.cargo` virtio-fs mount is now
  **unmounted** before the tree's `cargo/` is bound there, so one filesystem sits
  behind the path (live: a single `findmnt` line).

Live, second round (14 GiB free, so the lower was served): the guest composes
`/nix` as `overlay lowerdir=/run/cang/nix-lower,upperdir=/workspace/.cang/nix/<digest>/upper`,
seeds the generation as a btrfs subvolume (`Fresh`, sentinel inside `upper/`), reuses
it on the next boot (`AlreadySeeded`), and the store repair passes. Alternating arms
in one window: `find /nix/store` **1 718 / 776 / 206 ms** (block) against
**6 947 / 2 197 / 627 ms** (host overlay), `du -s` **2 904 / 528 ms** against
**9 310 / 9 341 ms**. Two real bugs surfaced and were fixed: the seed's mount check
still demanded the build disk (so the overlay never composed), and every arm has to
wait for `state=ready` because `/nix` prep is a background step.
[note 60](../notes/60-nix-follows-the-workspace-disk.md).

Carried forward as its own gap: `cang nix gc` still points at `cang-build.raw`, so
generations inside a workspace image are not collected yet (item 7 above,
`docs/usage.md`).
