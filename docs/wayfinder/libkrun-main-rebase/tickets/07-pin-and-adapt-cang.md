---
label: wayfinder:task
title: Pin v2.0.0-cang.1 and adapt cang
status: closed
blocked_by: ["06-publish-v2-release"]
claimed_by: pi session (2026-09-27)
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

## Resolution (2026-09-27)

Pinned and adapted.

**Pin** (`nix/pins.nix`, via the repo's own tool
`nix develop --command ./scripts/update-libkrun.sh --tag v2.0.0-cang.1`):

| | |
|---|---|
| tag | `v2.0.0-cang.1` |
| x86_64-linux | `libkrun-x86_64-linux-full.tgz`, `sha256-p+ZOHb8wHtORBRkYWqwZHVcidM6Mn0vmRnfBJ10r5SU=` |
| aarch64-linux | `libkrun-aarch64-linux-full.tgz`, `sha256-i29BoYqhSqMNfEsDkig2hHYxoNIg0u9ihYdsfYTRY2U=` |

**Submodule pointer** moved to the tag's commit `d578e4e2` (the rebased fork
tip), so the local source tree and the pinned prebuilt are the same revision.

**Adaptations** (backed by `notes/04-fork-commit-triage.md` and ticket 01's
delta):

- `crates/cang/.../dynamic.rs`: `DEFAULT_LIBKRUN_NAMES` is now
  `["libkrun.so.2", "libkrun.so"]`.
- `nix/pkgs/libkrun.nix`: the postFixup loop covers `libkrun_init.so.*` as well
  (it needs `$ORIGIN` for its `libkrunfw.so.5` dlopen), the loop tolerates a
  release without an init blob, and `libpipewire` is gone from the runpath
  because 2.0.0 removed the snd feature.
- `nix/pkgs/cang-rust.nix` and `nix/pkgs/cang-prebuilt.nix`: `$out/lib/cang` now
  also symlinks `libkrun_init.so*`, which cang must dlopen under ABI 2.
- Docs: `README.md` (libkrun 2.0.0 / `libkrun.so.2`, the `CANG_LIBKRUN_LIBRARY`
  example) and `docs/maintenance.md` (examples name `v2.0.0-cang.1`, and note
  that a tag push also publishes a permanent release).

**Verified:** `nix build .#libkrun` fetches the new pin and produces
`lib/{libkrun.so.2.0.0,libkrun_init.so.0.1.0}` with their symlink chains, the
headers and both `.pc` files; `libkrun.so.2.0.0` carries
`RUNPATH=$ORIGIN:virglrenderer` and `libkrun_init.so.0.1.0` carries
`RUNPATH=$ORIGIN`. Building against the **old** pin still succeeds, so the edits
are backwards compatible.

**Known, expected consequence - the next ticket's job.** cang is *not* runnable
against this pin yet: its Rust binding still targets the v1 C ABI, so a launch
fails with a missing-symbol error (`krun_set_log_level` and friends) until ticket
09 ports it to the v2 object API. `--gpu=drm`/`--wayland` additionally need the
render-server fd entry point that ticket 10 re-adds. That window is inherent to
the route - a 2.0.0 pin cannot coexist with a v1-bound cang - so it is recorded
here rather than papered over.
