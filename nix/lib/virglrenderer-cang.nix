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
in
# The prebuilt asset named in `pins.virglrendererPrebuiltRelease` was cut before
# the decode fix in virglrenderer-decode-slice-params.patch, and an unpatched
# vrend leaves the decode's VA slice parameter buffer uninitialised, which makes
# a guest VA-API decode depend on stack layout (see docs/maintenance.md,
# "The virglrenderer decode parameter buffers"). Build from source until a
# prebuilt carrying the fix is published and pinned, then restore the next line.
# if builtins.hasAttr system systems then
if false then
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
