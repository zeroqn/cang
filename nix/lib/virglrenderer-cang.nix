# cang's virglrenderer: the prebuilt release asset when cang has published one for
# this system, otherwise the source build with the same patches. Both consumers -
# `pkgs.virglrenderer` in the overlay, which the cang packages link and take the
# render server from, and the flake's `.#virglrenderer` output - go through here,
# so a downstream host that builds its own vrend/libkrun stack can take the fixes
# without a virglrenderer source build.
{ pkgs, pins }:

let
  release = pins.virglrendererPrebuiltRelease or { };
  systems = release.systems or { };
  system = pkgs.stdenv.hostPlatform.system;

  source = import ./virglrenderer-patched.nix {
    inherit pkgs pins;
    virglrenderer = pkgs.virglrenderer;
  };

  # What libvirglrenderer.so.1 and libexec/virgl_render_server are linked against;
  # autoPatchelfHook resolves the prebuilt tarball's DT_NEEDED from these.
  virglrendererRuntimeDeps = with pkgs; [
    libdrm
    libepoxy
    libgbm
    libva
    libx11
    libGLU
    vulkan-loader
  ];
  # `build-virglrenderer.yml` publishes an asset and pins it in nix/pins.nix
  # together, so a `systems` entry is only the current binary while its `revision`
  # is the pinned one and its version carries that revision's stem. Between a
  # source move and that pin landing the entry names the previous build (or an
  # asset that does not exist yet), and the source build is the one that matches -
  # so it is chosen instead of shipping a library built from a revision the
  # `virglrendererSource` pin no longer names.
  prebuiltIsCurrent =
    (release.revision or null) == pins.virglrendererSource.rev
    && pkgs.lib.hasPrefix "${pkgs.virglrenderer.version}-${
      builtins.substring 0 8 pins.virglrendererSource.rev
    }-p" (release.version or "");
in
if prebuiltIsCurrent && builtins.hasAttr system systems then
  let
    asset = builtins.getAttr system systems;
    prebuilt = pkgs.callPackage ../pkgs/virglrenderer-prebuilt.nix {
      runtimeDeps = virglrendererRuntimeDeps;
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
