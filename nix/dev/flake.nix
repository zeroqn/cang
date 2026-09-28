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
          # cang compiles libkrun's Rust API itself now, so this is just the
          # shared source/derivation module pointed at the local fork checkout -
          # there is no separately built libkrun.so to wire in any more.
          libkrunSource = import ../../nix/pkgs/libkrun-source.nix {
            inherit pkgs;
            src = root + "/deps/libkrun";
          };
          rustPackages = import ../../nix/pkgs/cang-rust.nix {
            self = root;
            inherit
              pkgs
              pins
              libkrunfw
              ;
            krunInitBinary = libkrunSource.krunInitBinary;
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
