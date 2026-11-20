---
label: wayfinder:task
title: Remove the --nix CLI axis - a workspace disk means /nix on a block lower with its upper in the tree
status: open
blocked_by: []
claimed_by: ""
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
7. **Docs and tests**: `docs/usage.md`, `docs/images-and-storage.md`,
   `README.md` (`--nix=` prose), `crates/cang-repository-tests`, and the
   `nix-upper-on-block` map (whose ticket 09 is already cancelled and whose
   remaining question - upper on a block disk - is now answered by this ticket's
   step 2).

## Resolution

(to follow)
