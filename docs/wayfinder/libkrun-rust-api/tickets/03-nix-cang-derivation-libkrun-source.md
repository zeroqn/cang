---
label: wayfinder:task
title: Nix - compile libkrun inside cang's derivation (source + vendored graph)
status: closed
blocked_by: []
claimed_by: pi session (2026-09-28)
---

## Question

Make `nix build .#cang` compile libkrun from `deps/libkrun` as a cargo path
dependency: get the submodule source into the derivation and get libkrun's
dependency graph vendored into cang's `cargoDeps`.

Known constraints, from charting:

- `nix/pkgs/cang-rust.nix` builds with `src = self` and
  `cargoLock.lockFile = ../../Cargo.lock`. A flake's own source excludes
  submodule contents unless asked (`inputs.self.submodules = true` at the top
  level, or the trick `nix/dev/flake.nix` uses - it declares
  `self.submodules = true` *and* passes `self = root` as a plain path so
  submodule contents are copied).
- The vendoring mechanism has to change. `nix/dev/flake.nix`'s header comment
  records why: *upstream main's lock vendors `ffier` twice at one name-version,
  which `importCargoLock` cannot express, so the fork's source is vendored with
  `rustPlatform.fetchCargoVendor`*. That is exactly the case
  `cargoLock.lockFile` cannot handle once libkrun joins cang's graph.
- `cang-musl` (`pkgsStatic`, `--package cang-guest-init`) shares
  `../../Cargo.lock`, so it needs the *same* enlarged `cargoDeps` for
  resolution even though it compiles none of libkrun.
- Fork-side one-liner worth taking (from ticket 01): make `krun-init-blob`'s
  `ffier` dependency optional (`ffi = ["ffi-client", "dep:ffier"]`) so the
  ffier git dependency is not in cang's graph at all.

## Resolution

**Done; `nix build .#cang` succeeds with libkrun compiled from the submodule.**

- **Source**: `inputs.self.submodules = true` in `flake.nix` is what puts
  `deps/libkrun` into the flake's own source (verified by evaluating
  `packages.<system>.cang.src` and listing `deps/libkrun` in the resulting store
  path). `src = self` stays, so the submodule *pointer* is the libkrun pin - no
  `pins.nix` entry. A fresh checkout needs `git submodule update --init
  --recursive`; that is now in `docs/build.md` and the README build section, and
  `.github/workflows/{test,publish_release}.yml` checkouts use
  `submodules: recursive` (the image workflows already did).
- **Vendoring**: both `rustPackage` and `cangMuslPackage` in
  `nix/pkgs/cang-rust.nix` moved from `cargoLock.lockFile` to
  `cargoDeps = pkgs.rustPlatform.fetchCargoVendor { src = self; hash = ...; }`
  (`sha256-4czB16NnWIrXWxbZDddb8e9/RhVHW3yZ5BA3vfe0GUY=`). The musl build shares
  that vendor for resolution; it still compiles only `cang-guest-init`.
- **A finding worth keeping**: a path dependency inside the workspace directory
  is *auto-enrolled* as a workspace member, which made `cargo test --workspace`
  build libkrun's crates as primary packages - including
  `bindings/libkrun-via-cdylib-weak`, whose build script needs `rustfmt`
  (ffier's generator) and which cang has no use for. `exclude = ["deps"]` in the
  root `Cargo.toml` keeps the dependency a dependency (and lint-capped: measured
  `cargo clippy --all-targets --all-features -- -D warnings` exits 0, with
  libkrun's warnings shown but not fatal).
- **Not taken**: making `krun-init-blob`'s `ffier` dependency optional in the
  fork. `ffier` stays in cang's graph as a build-time dependency of the blob
  crate (unused without the `ffi` feature); it costs build time, not behaviour.
  Left as a possible fork cleanup.
