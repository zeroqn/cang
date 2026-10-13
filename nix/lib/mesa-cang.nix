# cang's mesa: the prebuilt release asset when cang has published one for this
# system, otherwise the source build with the same patches. Every consumer goes
# through here - the guest image's VA driver (`pkgs.mesaVaApi`), the render-server
# environment the cang packages carry (`CANG_MESA_LIBDIR` / `CANG_MESA_ICD`: the mesa
# the external virgl_render_server loads for GL and Vulkan, via
# nix/lib/render-server-env.nix), and the host-side `overlays.default` - so a
# downstream host can take the fix, and the cang packages do too, without a Mesa
# source build. The image's *guest-side* GL/Vulkan/software-renderer mesa is the
# plain `pkgs.mesa` on purpose, and the image's wrapper contracts assert the two
# differ.
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
      runtimeDeps = mesaRuntimeDeps;
      vulkanLoader = pkgs.vulkan-loader;
      releaseAsset = asset // {
        inherit system;
        inherit (release) owner repo tag version;
        revision = release.revision or null;
        url = "https://github.com/${release.owner}/${release.repo}/releases/download/${release.tag}/${asset.asset}";
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
