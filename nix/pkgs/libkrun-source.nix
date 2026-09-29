# libkrun's source, and the two things cang needs from it besides the crate
# itself: a vendored registry for building that source inside Nix, and the musl
# guest init that `krun-init-blob`'s build script embeds.
#
# `src` defaults to the `deps/libkrun` submodule, so what gets built is the fork
# checkout; the flake sets `inputs.self.submodules = true` so the source tree the
# sandbox receives contains it.
{
  pkgs,
  src ? ../../deps/libkrun,
}:
let
  libkrunSrc = src;

  # The fork's lock vendors ffier twice at one name-version, which
  # `importCargoLock` cannot express, so the sources come from
  # `fetchCargoVendor` - the helper nixpkgs' own libkrun package uses. This hash
  # moves with the submodule pointer: a libkrun bump has to refresh it in the
  # same commit.
  libkrunCargoDeps = pkgs.rustPlatform.fetchCargoVendor {
    src = libkrunSrc;
    hash = "sha256-5Snz7O5nbcg0qVgLPhSzFdGUk5+pqy+Iavt0mE3FLaQ=";
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
    cargoDeps = libkrunCargoDeps;
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
  inherit libkrunSrc libkrunCargoDeps krunInitBinary;
}
