---
label: wayfinder:task
title: Pin v2.0.0-cang.1 and adapt cang
status: open
blocked_by: ["06-publish-v2-release"]
claimed_by: unclaimed
---

## Question

Move cang onto the new artifact and fix everything the move breaks:

1. `nix develop --command ./scripts/update-libkrun.sh --tag v2.0.0-cang.1`
   (tag-aware asset derivation), then move the `deps/libkrun` submodule pointer
   to the rebased tip so pointer, tag and pin name the same build.
2. Apply ticket 01's delta list: `DEFAULT_LIBKRUN_NAMES` gains `libkrun.so.2`;
   **`nix/pkgs/cang-{prebuilt,rust}.nix` must ship `libkrun_init.so*`** (their
   `libkrun.so*` glob misses it, and cang cannot boot without the init blob);
   `nix/pkgs/libkrun.nix`'s `lib64`->`lib` + `include` copy and `libkrun.so.*`
   fixup should still fit but must be re-checked; `nix/dev`'s feature list needs
   main's reality (`SND` removed as a no-op; `ffi`; the `krun-init-blob` crate
   built with `--features ffi`); `flake.nix`'s libkrun-loadable comment and the
   ABI-change fallout in `crates/cang/src/runtime/vm/libkrun/` (ticket 09).
3. Update the in-tree documentation: `docs/maintenance.md`'s fork-release
   schemes section (new base version **2.0.0 / ABI 2**, the `libkrun.so.2` soname,
   the separate `libkrun_init.so` the prebuilt must carry, and the fact that the
   fork carries three **cherry-picked, still-unmerged** upstream PRs plus how to
   retire each), and the repository tests that assert the pin's tag/asset shape.
4. Prove it: `nix build .#cang .#cang-musl .#container`, `cargo test`, and the
   `cang-local-validation-gates` set (fmt, clippy `-D warnings`, `cargo deny`).
5. Confirm the shipped tree actually contains `libkrun.so.2*` and
   `libkrun_init.so*` (a `ls`/`readelf` on the store path beats trusting the
   globs), since a missing init blob is a boot failure, not a build failure.

## Deliverable

The committed pin + submodule move + cang-side adaptation, with the build and
test output recorded, and `git submodule status deps/libkrun` equal to the tag's
commit.
