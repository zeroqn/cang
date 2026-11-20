---
label: wayfinder:grilling
title: Worktrees, target/ and the registry inside the image - and what the image is keyed on
status: closed
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

## Round answers (2026-11-20, bob)

- **Q1 - `target/` sharing: (b), "inside the tree", and cang must not implement
  sharing at all.** Verbatim: *"kache can handle this, so don't do this at cang
  level. Just set up cargo is enough. bind mount cargo to ~/.cargo in the guest"*.
  Consequences: (1) `target/` is cargo's default `./target` inside the workspace
  image - cang stops exporting `CARGO_TARGET_DIR`
  (`launch/.../persistent_disks/build.rs:42`), so a worktree gets its own target
  by construction and cross-worktree target sharing is *someone else's* job
  (kache, sccache, whatever the user runs); (2) the cargo home is a directory on
  the image, bind-mounted at **`/home/dev/.cargo`** in the guest, so the
  conventional path is the real registry path instead of
  `/home/dev/.cache/cang-build/cargo` (`build.rs:44`,
  `guest_init/components/disk/build.rs:28-33`, including the seed-from-image step
  at `:98-100` which this replaces).
- **Q2 - worktree creation: (a) leave it to `git`.** `git worktree add` inside the
  guest just makes a directory on btrfs; no cang command, no new concept.
- **Q3 - image keying: deferred, bob asked for the purpose in plain words** -
  answered below; with Q1=(b) the *sharing* reason for repo-root keying
  disappeared, so the default is to keep the cwd slug
  (`state/mod.rs:48`) unless bob says otherwise.
- **Q4 - creation size and growth: (a) agreed.** Create at 64 GiB,
  auto-grow at launch while free space inside would fall under ~10 % and the host
  keeps >= 2x headroom, `--workspace-size=N` as the override, a named warning
  when the cap or headroom refuses, and the existing manual `cang disk resize`
  as the escape. The mechanism is already there (`build.rs:66-78`).

**What this settles for other tickets:** ticket 18 builds no target-sharing
machinery - only source, registry and target *locations*; ticket 26's retirement
of the build disk now also removes `CARGO_TARGET_DIR`, not just the `/nix` upper;
the word "worktree" stays out of `crates/` entirely.

### Q3 resolved (2026-11-20, bob): neither - the disk *path* is the identity, the
cwd is only where you see it

Verbatim: *"Then it opposite. If we specific a raw disk as workspace, then the
directory we launch from is the reverse view of raw disk workspace. For example
cwd /foo/bar: cang -> mount /foo/bar as /workspace in cang guest; cang work.raw
-> reverse "bind mount" work.raw content to /foo/bar"*.

So the model inverts the old one rather than keying the image off the directory:

- **the workspace is a named raw disk** (`work.raw`, or a `/dev/sdX` later) - that
  file is the identity, and cang creates it if it does not exist;
- **the guest mounts that disk at `/workspace`**;
- **the host's cwd becomes the reverse view of that disk's content** (the reverse
  share, ticket 23, seen at `/foo/bar`), not the source of it;
- with no disk named, a launch keeps today's behaviour (the host directory served
  by virtio-fs), so nothing breaks for existing users.

What this settles: no repo-root lookup, no cwd-derived image name; the cwd slug
(`state/mod.rs:48`) stays only for the per-run task root, logs, leases and the
guest hostname. A launch that *does* name a disk mounts over its own cwd, so the
host tree underneath is hidden rather than copied - which moves the "what happens
to the host copy" question into ticket 25 (seeding) and the mount-time semantics
into ticket 23's contract.

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

Settled 2026-11-20 (bob): **`target/` inside the tree, cang implements no sharing
of it** (kache owns caching, cargo's defaults are enough); the **cargo home is a
directory on the image bind-mounted at `~/.cargo`**; **worktrees are plain
`git worktree add`s in the guest**; **the image is created at 64 GiB and
auto-grows under a headroom rule with `--workspace-size` and the manual command as
overrides**; and **the workspace's identity is the named raw disk, with the
launch directory becoming the reverse view of its content**.

Carried forward: ticket 18 (no target-sharing machinery; source/registry/target
*locations* only; `--workspace-disk` surface and the mount-over-cwd semantics),
ticket 25 (a named empty disk is the norm - seeding an existing host tree is an
explicit act, and the host copy ends up hidden under the mount), ticket 26 (the
build disk's retirement now also removes `CARGO_TARGET_DIR`), ticket 23 (the host
view is the launch directory).
