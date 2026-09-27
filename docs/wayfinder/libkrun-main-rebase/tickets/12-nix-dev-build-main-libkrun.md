---
label: wayfinder:task
title: Make nix/dev build main's libkrun (musl guest init)
status: open
blocked_by: []
claimed_by: unclaimed
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
