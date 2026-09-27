---
label: wayfinder:task
title: Make nix/dev build main's libkrun (musl guest init)
status: closed
blocked_by: []
claimed_by: pi session (2026-09-27)
---

## Question

`nix build ./nix/dev#cang-dev` fails on the rebased branch before compiling
anything of ours: upstream `main` builds the guest init (`init/init-binary`) as a
**musl** binary, and `init/init-blob/build.rs` panics unless the
`<arch>-unknown-linux-musl` rust **std** is available. Our `nix/dev` libkrun
override uses the host toolchain, whose sysroot has no musl std (details and the
exact error in `../notes/04-build-check.md`).

Decide and implement the local-source build path:

1. **Split build** - `make` the host `libkrun.so.2` and build `krun-init-blob`
   separately with `pkgs.pkgsStatic`'s rust (the toolchain `cang-musl` already
   uses), then install both (plus `libkrun.pc`/`libkrun_init.pc` and both
   headers) into one output.
2. **Host library only** - build `libkrun.so.2` without the init blob and record
   that local builds cannot boot a guest; all live booting then happens against
   the published prebuilt (tickets 06/07/08) and tickets 09/10 verify compilation
   only.
3. Something else (e.g. a multi-target toolchain for the derivation).

Whatever is chosen must also keep `nix/dev`'s stale settings honest for a 2.0.0
base: `withSound = true` (main removed the feature) and
`version = "1.19.5-cang-profile"`.

## Deliverable

`nix build ./nix/dev#cang-dev` green on the rebased branch, with either a
runnable (host library + musl init) or explicitly compile-only result recorded,
and the outcome noted in `../notes/04-build-check.md`.

## Resolution

**Bob chose the split build (2026-09-27); implemented and verified.**

The fix turned out cleaner than the three sketched options, because upstream
already supports it: `init/init-blob/build.rs` honours **`KRUN_INIT_BINARY_PATH`**
and then embeds the given binary instead of cross-building it - the same escape
hatch upstream's own `code-quality.yml:65` uses. So:

1. `krunInitBinary` - a new derivation in `nix/dev/flake.nix` builds
   `init/init-binary` (package `krun-init`, not a workspace member) with
   `pkgs.pkgsStatic`'s rust - the toolchain `cang-musl` uses - for
   `x86_64-unknown-linux-musl`, with the `timesync` feature (upstream PR 840's
   guest half), and installs `$out/bin/krun-init`. It reuses the workspace's
   vendored registry (`libkrunCargoDeps`).
2. The `libkrun` override now sets `env.KRUN_INIT_BINARY_PATH` to that binary, so
   the ordinary Makefile flow builds the host `krun-init-blob` cdylib around the
   prebuilt musl init and installs `libkrun_init.so*` itself - no Makefile
   patching, no second install path.
3. Also corrected while there: `withSound = true` dropped (main removed the
   feature), `withTimesync = true` added, `version = "2.0.0-cang"`, and
   `pkgs.rustfmt` added to `nativeBuildInputs` because ffier's binding generator
   shells out to it.

**Verified:** `nix build ./nix/dev#cang-dev` exits 0 on `cang-main-rebase`. The
libkrun output carries `libkrun.so.2.0.0` (+ `.so.2`, `.so`),
`libkrun_init.so.0.1.0` (+ `.so.0`, `.so`), the four headers and both `.pc`
files; `cang-dev`'s `lib/cang` links the freshly built `libkrun-2.0.0-cang` and
the local `libkrunfw`; the cang cargo test suite ran and passed.
