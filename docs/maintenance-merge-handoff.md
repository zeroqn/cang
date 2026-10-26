# Merge handoff (2026-10-26)

*Written by the virtio-fs perf session for the next one. Read before touching git state.*

## The merge is done, in the order bob wants

- Worktree branch `wayfinder/workspace-virtiofs-perf` (in `/home/dev/cang/cang-vfio`)
  is **fully merged into local `main`** in the primary checkout
  `/home/dev/cang/cang`: `git merge-base --is-ancestor wayfinder/workspace-virtiofs-perf main`
  is true and `git log main..wayfinder/...` is empty.
- Local `main` = `93bf1b6 chore(nix): add lsof`, whose parent is
  `a7c8ae5 merge main (nixpkgs bump + the libkrun-src pin) into the virtio-fs perf branch`.
  Working tree clean apart from an untracked `.scratch/`.
- Local `main` is **1 ahead / 0 behind** `origin/main` (`a7c8ae5`): the only
  unpublished commit is `93bf1b6`. **bob pushes `main`; sessions do not.**

## The rule this session got wrong once

Integrate into **local** `main` and stop. Do not push a worktree branch's `HEAD`
straight to `origin/main`: bob keeps local `main` as the integration point, wants
it clean, and pushes it himself. (What happened here: `origin/main` was
fast-forwarded `6d6db9d..a7c8ae5` from the worktree before this rule was stated.
It is a valid fast-forward containing exactly the merge that local `main` carries,
so nothing needs undoing - but it is not the workflow.)

## Submodule gotcha, seen immediately after the pull

`git pull` moves a gitlink, never submodule contents, so `deps/libkrun` showed up
as `M deps/libkrun`: `main` records `d00b6d70` while that clone was still at
`7160f48f` and had never fetched it. Fix:

```
cd deps/libkrun && git fetch origin cang
cd ../.. && git submodule update deps/libkrun      # -> d00b6d70, clean status
```

The same equality matters for builds: `flake.lock`'s `libkrun-src` and the
submodule pointer must name the same revision, or `nix build` compiles one libkrun
and in-tree `cargo` compiles another (that is exactly how a smoke gate came to
grade the wrong fork earlier in this session).

## What the branch actually changed

Fork tag `v2.0.0-cang.11` (commit `d00b6d70`, assets published): a worker-robustness
fix, a request thread pool, and zero-message open. Measured on a cold 2401-crate
rustc build: 427.1 s -> 176.0 s (2.42x), the share's cost over the same build on
guest tmpfs from 3.28x to 1.34x. Details: `MAP.md` in the map, note 23, ticket 19.
