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
          libkrun =
            (pkgs.libkrun.override {
              inherit libkrunfw;

              withBlk = true;
              withNet = true;
              withGpu = true;
              withSound = true;
              withInput = true;
            }).overrideAttrs
              (_oldAttrs: {
                version = "1.19.5-cang-profile";
                src = libkrunSrc;
                # Upstream main's lock vendors ffier twice (tags 0.2.0rc1 and
                # v0.2.0-rc2) at the same name-version, which importCargoLock
                # cannot express (it keys outputHashes by name-version), so the
                # fork's source uses fetchCargoVendor instead.
                cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
                  src = libkrunSrc;
                  hash = "sha256-SjThWtfmffo38w3ormnO+hSa4H6IugRz2wq4DvWX5Jg=";
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
