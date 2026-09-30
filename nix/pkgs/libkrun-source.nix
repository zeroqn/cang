# libkrun's source, and the two things cang needs from it besides the crate
# itself: a vendored registry for building that source inside Nix, and the musl
# guest init that `krun-init-blob`'s build script embeds.
#
# `src` is the `deps/libkrun` fork checkout the flake passes in as its
# `libkrun-src` input; a flake's own source cannot carry submodule contents, see
# `nix/pkgs/workspace-src.nix`.
{
  pkgs,
  src,
}:
let
  libkrunSrc = src;

  # `init/init-binary` is excluded from libkrun's workspace and tracks its own
  # lock (upstream checks it with `cargo check --locked`), so the guest init
  # vendors from that lock, not the workspace's: a vendor directory only carries
  # the versions its own lock names, and the two locks resolve the same crates
  # (anyhow, nix, ...) to different ones. This hash moves with the fork
  # revision: a libkrun bump (submodule pointer plus `libkrun-src`) has to
  # refresh it in the same commit.
  krunInitCargoDeps = pkgs.rustPlatform.fetchCargoVendor {
    name = "krun-init-cargo-deps";
    src = libkrunSrc;
    cargoRoot = "init/init-binary";
    hash = "sha256-0Qjy3te+nAzRLASyqrhpl9QsSwCZcn0HAN7Ia/3jRjs=";
  };

  muslTarget =
    if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
      "x86_64-unknown-linux-musl"
    else if pkgs.stdenv.hostPlatform.system == "aarch64-linux" then
      "aarch64-unknown-linux-musl"
    else
      throw "cang is Linux-only";

  # The guest init (`init/init-binary`) is a musl binary that the blob's build
  # script would otherwise cross-build itself, which the host toolchain cannot do
  # (no musl rust std in its sysroot). Build it with pkgsStatic's rust - the
  # toolchain `cang-musl` uses - and let `KRUN_INIT_BINARY_PATH` make the blob
  # embed it instead, the same escape hatch upstream's code-quality workflow
  # uses.
  krunInitBinary = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
    pname = "krun-init";
    version = "0.1.0";
    src = libkrunSrc;
    # `cargoRoot` tells the cargo setup hook to compare `cargoDeps`' lock
    # against `init/init-binary/Cargo.lock` instead of the workspace lock.
    cargoRoot = "init/init-binary";
    cargoDeps = krunInitCargoDeps;
    CARGO_BUILD_TARGET = muslTarget;
    cargoBuildFlags = [
      "--manifest-path"
      "init/init-binary/Cargo.toml"
      "--features"
      "timesync"
    ];
    doCheck = false;
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/bin"
      # init/init-binary is not a workspace member, so cargo places the artifact
      # in its own target directory.
      install -m 755 "$(find . -type f -name krun-init -path '*/release/*' | head -n1)" "$out/bin/krun-init"
      runHook postInstall
    '';
  };
in
{
  inherit libkrunSrc krunInitCargoDeps krunInitBinary;
}
