---
label: wayfinder:grilling
title: Worktrees, target/ and the registry inside the image - and what the image is keyed on
status: open
blocked_by: []
claimed_by: ""
---

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
