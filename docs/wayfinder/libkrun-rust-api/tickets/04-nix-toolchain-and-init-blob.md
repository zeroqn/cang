---
label: wayfinder:task
title: Nix - build inputs for the Rust-API build, and the musl init blob
status: open
blocked_by: []
claimed_by:
---

## Question

Give both the derivation and the devshell everything libkrun's Rust-API build
needs, without breaking the musl/static outputs.

From the probe (`../notes/02-rust-api-build-probe.md`) and the previous map's
ticket 12:

1. `rustPlatform.bindgenHook` in `nativeBuildInputs` (it populates `LIBCLANG_PATH`
   and `BINDGEN_EXTRA_CLANG_ARGS` - required by `krun-display` and `krun-input`'s
   build scripts), plus `pkg-config`, `virglrenderer` (>= 1.3.0; nixos-26.05 has
   exactly 1.3.0) and `libgbm`/`mesa-libgbm` for `rutabaga_gfx`'s
   `virgl_renderer` feature.
2. The guest init blob: `krun-init-blob/build.rs` either cross-builds
   `init/init-binary` for `-musl` (the host rustc has no musl std, so this
   panics) or embeds a binary given via `KRUN_INIT_BINARY_PATH`. `nix/dev/flake.nix`
   already builds that binary with `pkgs.pkgsStatic`'s rust (`krunInitBinary`,
   with the `timesync` feature) and feeds it in - reuse it (move it somewhere
   shared rather than duplicating it), and confirm it is wired into every
   derivation that compiles `krun-init-blob`, not just the dev one.
3. The devshell (`nix/shell/devshell.nix`) needs the same inputs so a
   contributor's `cargo build`/`cargo test` works, plus whatever vendored
   registry env nixpkgs needs for offline builds in a shell.
4. Keep `cang-musl`/the image build unaffected: libkrun is host-only.

Done when: `nix build .#cang` and `nix develop --command cargo build` both
succeed with libkrun compiled in, and `nix build .#cang-musl` still works.
