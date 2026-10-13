# The host-side render-server environment: where the cang launcher finds mesa
# (GL drivers plus the radeon Vulkan ICD) and the Vulkan loader for the external
# virgl_render_server process.
#
# .#cang is deliberately a raw ELF (see nix/pkgs/cang-rust.nix), so every
# consumer of a *tree-built* cang has to supply these values. They are defined
# here once: .#cang-prebuilt's released wrapper bakes them in, and
# ./render-server-env.sh is the sourceable form the chromium GPU smoke uses so a
# no-argument run can launch a raw-ELF cang.
{ pkgs }:
let
  # cang's mesa, not nixpkgs' plain drop: this is the library the external
  # `virgl_render_server` loads for GL and Vulkan, and on a virtio-gpu host the
  # virtio-gpu DMA-BUF modifier fix (`mesa-headless-virtio-modifiers.patch`) has to
  # be in it - "the same fix cang's host vrend needs on such a host". The image's
  # guest-side mesa is a different thing and stays `pkgs.mesa`; the wrapper
  # contracts pin that separation. See nix/lib/mesa-patched.nix.
  mesa = pkgs.mesaVaApi;
  env = {
    CANG_MESA_LIBDIR = "${mesa}/lib";
    CANG_MESA_ICD = "${mesa}/share/vulkan/icd.d/radeon_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json";
    CANG_VULKAN_LOADER_LIBDIR = "${pkgs.vulkan-loader}/lib";
  };
in
{
  inherit env;

  # Guarded *exports*: sourcing this file fills in only the variables that are
  # not already set (a caller's explicit value wins) and marks them exported,
  # because the consumer is a child process -- the cang launcher itself.
  file = pkgs.writeTextFile {
    name = "cang-render-server-env";
    destination = "/render-server-env.sh";
    text =
      pkgs.lib.concatStringsSep "\n" (
        pkgs.lib.mapAttrsToList (name: value: ''export ${name}="''${${name}:-${value}}"'') env
      )
      + "\n";
  };
}
