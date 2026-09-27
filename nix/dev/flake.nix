{
  description = "Local submodule-aware development outputs for cang";

  inputs = {
    self.submodules = true;
    cang.url = "../..";
    nixpkgs.follows = "cang/nixpkgs";
    headless.url = "github:zeroqn/headless";
  };

  outputs =
    {
      nixpkgs,
      headless,
      ...
    }:
    let
      root = ../..;
      systems = import ../../nix/lib/systems.nix {
        inherit nixpkgs headless;
      };
      pins = import ../../nix/pins.nix;
    in
    {
      packages = systems.forAllSystems (
        { pkgs, ... }:
        let
          libkrunfw = pkgs.callPackage ../../nix/pkgs/libkrunfw.nix {
            inherit pins;
            libkrunfwSrc = root + "/deps/libkrunfw";
            useLocalSource = true;
          };
          libkrunSrc = root + "/deps/libkrun";
          # Upstream main's lock vendors ffier twice at one name-version, which
          # importCargoLock cannot express, so the fork's source is vendored with
          # fetchCargoVendor (the helper nixpkgs' own libkrun package uses). The
          # same vendored registry serves both derivations below.
          libkrunCargoDeps = pkgs.rustPlatform.fetchCargoVendor {
            src = libkrunSrc;
            hash = "sha256-SjThWtfmffo38w3ormnO+hSa4H6IugRz2wq4DvWX5Jg=";
          };
          krunMuslTarget =
            if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
              "x86_64-unknown-linux-musl"
            else if pkgs.stdenv.hostPlatform.system == "aarch64-linux" then
              "aarch64-unknown-linux-musl"
            else
              throw "cang dev builds are Linux-only";
          # main builds the guest init (init/init-binary) for musl, and
          # init/init-blob/build.rs refuses to run without the musl rust std,
          # which the host toolchain does not have. Build the blob with
          # pkgsStatic's rust - the same toolchain cang-musl uses - and install
          # it next to the host library.
          # main's guest init is a musl binary (init/init-binary) that the
          # krun-init-blob build script normally cross-builds itself, which the
          # host toolchain cannot do (no musl rust std). Build it here with
          # pkgsStatic's rust - the toolchain cang-musl uses - and point the
          # blob's build script at it with KRUN_INIT_BINARY_PATH: that variable
          # makes it embed the given binary instead of cross-building, the same
          # way upstream's own code-quality workflow does it.
          krunInitBinary = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
            pname = "krun-init";
            version = "0.1.0";
            src = libkrunSrc;
            cargoDeps = libkrunCargoDeps;
            CARGO_BUILD_TARGET = krunMuslTarget;
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
              # init/init-binary is not a workspace member, so cargo puts the
              # artifact in its own target directory.
              install -m 755 "$(find . -type f -name krun-init -path '*/release/*' | head -n1)" "$out/bin/krun-init"
              runHook postInstall
            '';
          };
          libkrun =
            (pkgs.libkrun.override {
              inherit libkrunfw;

              withBlk = true;
              withNet = true;
              withGpu = true;
              withInput = true;
              withTimesync = true;
            }).overrideAttrs
              (old: {
                version = "2.0.0-cang";
                src = libkrunSrc;
                cargoDeps = libkrunCargoDeps;
                # ffier's binding generator shells out to rustfmt.
                nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.rustfmt ];
                env = (old.env or { }) // {
                  KRUN_INIT_BINARY_PATH = "${krunInitBinary}/bin/krun-init";
                };
              });
          rustPackages = import ../../nix/pkgs/cang-rust.nix {
            self = root;
            inherit
              pkgs
              pins
              libkrun
              libkrunfw
              ;
          };
        in
        {
          default = rustPackages.rustPackage;
          cang-dev = rustPackages.rustPackage;
          virglrenderer = pkgs.virglrenderer;
        }
      );
    };
}
