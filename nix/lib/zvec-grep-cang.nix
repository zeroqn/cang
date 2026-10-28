# cang's zg: the prebuilt release asset when cang has published one for this
# system, otherwise the Rust source build with the same revision. Both consumers —
# the image's agent layer and the flake's `.#zvec-grep` output, which downstream
# flakes install — go through here, so a downstream host can take the binary
# without compiling llama.cpp, and the container build can too.
#
# `pkgs` must be a nixpkgs whose rustc satisfies the workspace's
# `rust-version = "1.98"` and whose ONNX Runtime is recent enough for the API
# version `ort` compiles against (21 as of the pinned revision); the flake passes
# nixpkgs-unstable (the 26.05 input has rustc 1.95). See nix/pkgs/zvec-grep.nix.
{ pkgs, pins }:

let
  release = pins.zgPrebuiltRelease or { };
  systems = release.systems or { };
  system = pkgs.stdenv.hostPlatform.system;

  zvecRustLib = pins.zvecRustLib;
  zvecLib = pkgs.callPackage ../pkgs/zvec-rust-lib.nix {
    releaseAsset = zvecRustLib // {
      url = "https://github.com/${zvecRustLib.owner}/${zvecRustLib.repo}/releases/download/v${zvecRustLib.version}/${zvecRustLib.asset}";
    };
  };

  source = import ../pkgs/zvec-grep.nix { inherit pkgs pins; };

  # What the prebuilt binary's DT_NEEDED set resolves against; autoPatchelfHook
  # rewrites its rpath to these.
  runtimeDeps = [
    zvecLib
    pkgs.onnxruntime
    pkgs.stdenv.cc.cc.lib
  ];
  # `build-zg.yml` publishes an asset and pins it in nix/pins.nix together, so a
  # pinned asset is only the current binary while both its revision and its stamp
  # match the source pin - the asset name carries both. Between either bump and
  # that pin landing, the pinned asset is the previous zg (or an asset name that
  # does not exist yet) and the source build is the one that matches.
  prebuiltIsCurrent =
    (release.revision or null) == pins.zvecGrep.rev
    && (release.version or null) == pins.zvecGrep.version;
in
if prebuiltIsCurrent && builtins.hasAttr system systems then
  let
    asset = builtins.getAttr system systems;
    prebuilt = pkgs.callPackage ../pkgs/zvec-grep-prebuilt.nix {
      inherit runtimeDeps;
      inherit (pkgs) onnxruntime;
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
    };
  in
  prebuilt
else
  source
