# cang's waypipe: zeroqn/headless's prebuilt release asset when one is pinned for
# this system, otherwise nixpkgs' own waypipe. Both consumers - the image, which
# ships it as the guest's `waypipe` that guest-init execs for `cang --waypipe`,
# and the flake's `.#waypipe-bin` output, which a downstream host installs as the
# matching client - go through here, so a guest server and a host client can be
# taken from the same asset and speak the same waypipe protocol.
#
# `pkgs.waypipe` is the fallback only because that repository publishes a single
# x86_64-linux asset; a system without one keeps nixpkgs' build rather than
# losing waypipe entirely.
{ pkgs, pins }:

let
  release = pins.waypipePrebuiltRelease or { };
  systems = release.systems or { };
  system = pkgs.stdenv.hostPlatform.system;

  # What bin/waypipe resolves against; autoPatchelfHook rewrites the prebuilt
  # tarball's rpath to these.
  runtimeDeps = with pkgs; [
    ffmpeg
    libgbm
    lz4
    vulkan-loader
    zstd
  ];
in
if builtins.hasAttr system systems then
  let
    asset = builtins.getAttr system systems;
  in
  pkgs.callPackage ../pkgs/waypipe-prebuilt.nix {
    inherit runtimeDeps;
    releaseAsset = asset // {
      inherit system;
      inherit (release)
        owner
        repo
        tag
        version
        ;
      revision = release.revision or null;
      url = "https://github.com/${release.owner}/${release.repo}/releases/download/${release.tag}/${asset.asset}";
    };
  }
else
  pkgs.waypipe
