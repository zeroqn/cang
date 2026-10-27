# The Rust toolchain cang is built with, in one place.
#
# It is fenix's current stable release, not the nixpkgs the flake pins (26.05,
# whose `rustc` is 1.95): one toolchain serves `nix build .#cang`, the dev shell
# (and therefore CI, which runs `nix develop --command cargo ...`) and the guest
# image's rust layer, so the same compiler is used wherever cang is built.
#
# fenix publishes Rust's own release binaries, so this costs a download and not
# a rustc build; the flake's `nixConfig` adds the `fenix.cachix.org` substituter
# that carries the fenix wrapper outputs. Bump it with `nix flake update fenix`.
{
  # nixpkgs instance the toolchain is packaged against.
  pkgs,
  # `fenix.packages.<system>`.
  fenix,
}:
let
  # `rust-std` ships inside fenix's `rustc`, and `rust-src` is not needed to
  # compile anything: the guest image links it separately as `rustSrc`, because
  # a toolchain env and the image's `/share/rust-src` would otherwise carry the
  # same tree twice.
  toolchain = fenix.stable.withComponents [
    "cargo"
    "clippy"
    "rustc"
    "rustfmt"
  ];
in
{
  # rustc, cargo, rustfmt, clippy-driver, ... in one output: what the dev shell
  # puts on PATH and what the image's rust layer installs.
  inherit toolchain;

  # `RUST_SRC_PATH` for the guest and for `rust-analyzer` in it.
  rustSrc = fenix.stable.rust-src;

  # What `buildRustPackage` compiles with: cang, libkrun (compiled in from
  # `deps/libkrun`) and their build scripts. `cargoDeps` in
  # `nix/pkgs/cang-rust.nix` deliberately stays on nixpkgs' `rustPlatform`:
  # the vendor hash is a function of the lock file, so keeping the vendoring
  # cargo fixed means a toolchain bump does not re-hash it.
  rustPlatform = pkgs.makeRustPlatform {
    rustc = toolchain;
    cargo = toolchain;
  };
}
