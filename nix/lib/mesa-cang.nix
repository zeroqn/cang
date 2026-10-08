# cang's mesa: the prebuilt release asset when cang has published one for this
# system, otherwise the source build with the same patches. Both consumers - the
# guest image's VA driver and the host-side `overlays.default` - go through here, so
# a downstream host can take the fix without a Mesa source build.
{ pkgs, pins }:

let
  applyMesaPatches = import ./mesa-patched.nix;

  release = pins.mesaPrebuiltRelease or { };
  systems = release.systems or { };
  system = pkgs.stdenv.hostPlatform.system;

  source = applyMesaPatches pkgs.mesa;

  mesaRuntimeDeps = with pkgs; [
    libdrm
    libgbm
    libglvnd
    expat
    libx11
    libxcb
    libxext
    libxfixes
    libxrandr
    libxshmfence
    libxxf86vm
    systemd
    wayland
    llvmPackages.libllvm
    xcbutils
    xcbutilkeysyms
    zstd
    elfutils
    lm_sensors
    libdisplay-info
    libpng
    libunwind
    libva-minimal
    vulkan-loader
    gcc-unwrapped
  ];
in
if builtins.hasAttr system systems then
  let
    asset = builtins.getAttr system systems;
    prebuilt = pkgs.callPackage ../pkgs/mesa-prebuilt.nix {
      inherit mesaRuntimeDeps;
      runtimeDeps = mesaRuntimeDeps;
      vulkanLoader = pkgs.vulkan-loader;
      releaseAsset = asset // {
        inherit system;
        inherit (release) owner repo tag version;
        revision = release.revision or null;
      };
    };
  in
  prebuilt.overrideAttrs {
    # Consumers of nixpkgs' mesa expect these sub-outputs to exist; take them from the
    # source build so a prebuilt swap does not break them.
    passthru = (prebuilt.passthru or { }) // {
      inherit (source) driverLink;
      inherit (source) opencl spirv2dxil cross_tools debug;
    };
  }
else
  source
