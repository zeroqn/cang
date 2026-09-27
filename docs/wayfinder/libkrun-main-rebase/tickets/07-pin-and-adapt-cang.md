---
label: wayfinder:task
title: Pin v2.0.0-cang.1 and adapt cang
status: open
blocked_by: ["06-publish-v2-release", "01-upstream-main-delta-for-cang"]
claimed_by: unclaimed
---

## Question

Move cang onto the new artifact and fix everything the move breaks:

1. `nix develop --command ./scripts/update-libkrun.sh --tag v2.0.0-cang.1`
   (tag-aware asset derivation), then move the `deps/libkrun` submodule pointer
   to the rebased tip so pointer, tag and pin name the same build.
2. Apply ticket 01's delta list: loader names/soname (`DEFAULT_LIBKRUN_NAMES`),
   `nix/pkgs/libkrun.nix`, and any ABI change in
   `crates/cang/src/runtime/vm/libkrun/`.
3. Update the in-tree documentation: `docs/maintenance.md`'s fork-release
   schemes section (new base version, and the fact that the fork carries three
   **cherry-picked, still-unmerged** upstream PRs plus how to retire each), and
   the repository tests that assert the pin's tag/asset shape.
4. Prove it: `nix build .#cang .#cang-musl .#container`, `cargo test`, and the
   `cang-local-validation-gates` set (fmt, clippy `-D warnings`, `cargo deny`).

## Deliverable

The committed pin + submodule move + cang-side adaptation, with the build and
test output recorded, and `git submodule status deps/libkrun` equal to the tag's
commit.
