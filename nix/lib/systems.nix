{ nixpkgs, headless }:
let
  systems = [
    "x86_64-linux"
    "aarch64-linux"
  ];

  forAllSystems =
    f:
    nixpkgs.lib.genAttrs systems (
      system:
      f {
        inherit system;
        pkgs =
          (import nixpkgs {
            inherit system;
          }).extend
            (
              final: prev: {
                # cang image only: the guest's VA-API driver is libgallium and
                # the image's mesa is a prebuilt binary drop that cannot be
                # patched, so the guest-side half of the vrend encode fix is
                # carried by nixpkgs' mesa built from source with cang's patch.
                # (Its driver list stays nixpkgs' own: trimming it leaves the
                # `opencl`/`spirv2dxil`/`cross_tools` outputs empty, which nix
                # rejects.) Nothing outside `nix/image` uses this attribute.
                mesaVaApi = prev.mesa.overrideAttrs (old: {
                  patches = (old.patches or [ ]) ++ [
                    ../pkgs/patches/mesa-virgl-encode-raw-headers.patch
                  ];
                });
                virglrenderer = prev.virglrenderer.overrideAttrs (old: {
                  patches = (old.patches or [ ]) ++ [
                    ../pkgs/patches/virglrenderer-enum-26.patch
                    ../pkgs/patches/virglrenderer-gbm-layout-linear-modifier.patch
                    ../pkgs/patches/virglrenderer-encode-raw-headers.patch
                    ../pkgs/patches/virglrenderer-encode-upload-fence.patch
                    ../pkgs/patches/virglrenderer-linear-surface.patch
                  ];
                });
              }
            );
      }
    );
in
{
  inherit systems forAllSystems;
}
