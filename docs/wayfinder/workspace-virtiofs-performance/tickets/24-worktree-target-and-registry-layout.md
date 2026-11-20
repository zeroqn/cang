---
label: wayfinder:grilling
title: Worktrees, target/ and the registry inside the image - and what the image is keyed on
status: open
blocked_by: []
claimed_by: "pi + bob (2026-11-20)"
---

## Facts first (verified 2026-11-20, this checkout - so the round is not guesswork)

- **`target/` is not `/workspace/target` today.** cang redirects cargo onto the
  build disk by env: `CARGO_TARGET_DIR=/home/dev/.cache/cang-build/target` and
  `CARGO_HOME=/home/dev/.cache/cang-build/cargo`
  (`crates/cang/src/runtime/launch/components/persistent_disks/build.rs:42`,
  `:44`, exported at `:90-97`), with the same two paths asserted guest-side
  (`crates/cang-guest-init/src/guest_init/components/disk/build.rs:28-29`). So
  every launch of a state root already shares **one** target dir and **one**
  registry, whatever directory it was launched from.
- **The build disk is created at 96 GiB** (`build.rs:47`, `SIZE_BYTES`), and its
  btrfs is expanded by the guest on mount when the host image grew
  (`build.rs:66-78`, `grow_with_runner` -> `raw_btrfs::grow_existing_at_least_with_runner`).
  In other words **the auto-grow machinery bob wants already exists host-side**;
  what is missing is a rule for *when* to grow and out of whose headroom, not a
  mechanism.
- **The state root is keyed on the cwd**, not the repository:
  `crates/cang/src/state/mod.rs:48` `app_dir.join(derive_workspace_slug(cwd))`,
  slug from `crates/cang/src/naming/workspace.rs:6`; the slug also names the
  guest hostname (`runtime/launch/plan.rs:102-103`).
- **A `git worktree` inside the guest is just a directory today**, on the same
  virtio-fs share as everything else; nothing in cang knows what a worktree is
  (`grep -F worktree crates/` = 0 hits), so this ticket is the first place the
  concept enters the codebase.
- **The guest image ships `btrfs-progs`** (`nix/image/layers.nix:424`), so
  subvolume snapshot/delete needs no new package; `mkfs.btrfs`/`blkid` already
  come from `RuntimeTool` on the host (`raw_btrfs.rs:44`, `:71`).
- The three tenants the build disk carries today - `target/`, `CARGO_HOME` and
  the `/nix` upper - are documented at `crates/cang/src/runtime/maintenance/disk.rs:7`,
  and the third is being retired by ticket 26, which is what makes the placement
  question sharp now.

## Question

The point of putting `/workspace` on btrfs is cheap copies: `btrfs subvolume
snapshot` gives a worktree (or a `target/` checkpoint) in O(1), on a filesystem
whose writes do not cross FUSE. What that means concretely is undecided.

Decide:

1. **Worktree shape.** One subvolume per checkout? A subvolume per `git worktree`
   (created by a cang command, or by `git worktree add` inside the guest)? Nested
   subvolumes are invisible across a snapshot boundary, which is exactly what a
   worktree wants and exactly what a `target/`-sharing scheme does not - say which
   of the two is wanted (`target/` shared between worktrees is a real requirement
   for a compile loop).
2. **`target/` and `CARGO_HOME`.** Inside the tree (snapshotted with the sources)
   or as their own subvolumes (shared across worktrees, deleted as a unit)? bob's
   decision that they live on this image is settled; their *shape* is this
   ticket's.
3. **The registry**: shared subvolume, or per-worktree? A registry is append-mostly
   and wants sharing; a concurrent `cargo` may want isolation.
4. **What the image is keyed on** - the cwd slug (`state/mod.rs:37`
   `resolve_state_layout_from_parts` derives it from the cwd today) or the
   repository root, given that two checkouts of one repo should probably share one
   image and its registry, and that a worktree has no `.git` directory of its own.
5. **Sizing.** Create at N GiB, grow when? `cang disk resize` grows only the host
   file (`raw_btrfs.rs:115`) and refuses a shrink; if `target/` grows the image
   forever, this is where quota/`balance`/subvolume-delete policy gets decided -
   including whether the "no manual grows" goal (map, 2026-11-20) is achievable at
   all.
6. **Visibility through the reverse share** (ticket 23's contract): a nested
   subvolume is a directory to a client that only sees paths - does the host see
   worktrees as ordinary directories, and what does a subvolume *delete* look like
   to a host process holding a file open.

## Resolution

(to follow)
