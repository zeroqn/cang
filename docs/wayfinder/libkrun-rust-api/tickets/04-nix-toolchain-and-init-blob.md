---
label: wayfinder:task
title: Nix - build inputs for the Rust-API build, and the musl init blob
status: closed
blocked_by: []
claimed_by: pi session (2026-09-28)
---

## Question

Give both the derivation and the devshell everything libkrun's Rust-API build
needs, without breaking the musl/static outputs.

## Resolution

**Done; `nix build .#cang`, `nix build .#cang-musl` and
`nix develop --command cargo build` all succeed.**

- New shared module `nix/pkgs/libkrun-source.nix` holds the three things the
  source needs: `libkrunSrc` (`deps/libkrun` by default), `libkrunCargoDeps`
  (the fork's own vendored registry, needed by the musl init) and
  `krunInitBinary` (the guest init, built with `pkgs.pkgsStatic`'s rust and the
  `timesync` feature, reusing the previously private version from
  `nix/dev/flake.nix`). The main flake, `nix/dev` and the devshell all import it,
  so there is one definition instead of three.
- `nix/pkgs/cang-rust.nix` gained `rustPlatform.bindgenHook` (populates
  `LIBCLANG_PATH` and `BINDGEN_EXTRA_CLANG_ARGS` for `krun-display` and
  `krun-input`), `pkg-config`, `rustfmt` (ffier's generator shells out to it) and
  `patchelf`, plus `virglrenderer`/`libgbm` as `buildInputs` for `rutabaga_gfx`'s
  `virgl_renderer` probe - nixos-26.05 ships virglrenderer 1.3.0, exactly
  `atleast_version("1.3.0")` - and `KRUN_INIT_BINARY_PATH` pointing at
  `krunInitBinary`.
- The devshell gained the same inputs; `libkrunfw` is a `buildInput` so a
  dev-built binary can open the firmware by soname.
- The package's `postFixup` adds `--add-rpath '$ORIGIN/../lib/cang'` to
  `bin/cang`: the firmware is opened by soname from the cang process itself now,
  and libkrun's old `$ORIGIN` runpath is gone with the shared object.
- `nix/dev/flake.nix` collapsed: with cang compiling libkrun itself, the local
  `.so` override (makeFlags `FFI=1`, the `pkgs.libkrun.override`) had no
  consumer left. It now imports the shared module and keeps only its
  local-source `libkrunfw`.
