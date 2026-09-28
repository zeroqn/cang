---
label: wayfinder:task
title: Nix - compile libkrun inside cang's derivation (source + vendored graph)
status: open
blocked_by: []
claimed_by:
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
  `cargoLock.lockFile` cannot handle once libkrun joins cang's graph, so cang's
  derivation likely moves to `fetchCargoVendor` + an explicit `cargoDeps` hash
  (the dev sub-flake's `libkrunCargoDeps` is a starting point, but cang's hash
  differs because the graph is bigger).
- `cang-musl` (`pkgsStatic`, `--package cang-guest-init`) shares
  `../../Cargo.lock`, so it needs the *same* enlarged `cargoDeps` for
  resolution even though it compiles none of libkrun. Confirm that, and that
  the musl build does not start compiling libkrun.
- Fork-side one-liner worth taking (from ticket 01): make `krun-init-blob`'s
  `ffier` dependency optional (`ffi = ["ffi-client", "dep:ffier"]`) so the
  ffier git dependency is not in cang's graph at all.

Done when: a clean `nix build .#cang` in the sandbox compiles libkrun from the
submodule (bindgen and virglrenderer included - ticket 04) and the binary links,
with no `/nix/store` path leaked into the build inputs by accident and the
submodule revision recorded in `nix/pins.nix` (or stated as "whatever the
submodule pointer is", a decision to make here).
