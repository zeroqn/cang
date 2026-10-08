{ nixpkgs, headless, pins }:
let
  mesaCang = pkgs: import ./mesa-cang.nix { inherit pkgs pins; };
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
                # cang image only: the guest's VA-API driver is libgallium and the
                # image's mesa is a prebuilt binary drop, so the guest half of the
                # vrend video fix is carried by nixpkgs' mesa built from source with
                # cang's patches. The same override is exported for hosts as
                # `overlays.default` / `packages.mesa-rbsp-bounds` in flake.nix.
                mesaVaApi = mesaCang prev;
                virglrenderer = prev.virglrenderer.overrideAttrs (old: {
                  patches = (old.patches or [ ]) ++ [
                    ../pkgs/patches/virglrenderer-enum-26.patch
                    ../pkgs/patches/virglrenderer-gbm-layout-linear-modifier.patch
                    ../pkgs/patches/virglrenderer-encode-raw-headers.patch
                    ../pkgs/patches/virglrenderer-encode-upload-fence.patch
                    ../pkgs/patches/virglrenderer-linear-surface.patch
                    # NOTE: virglrenderer-encode-caps.patch and
                    # virglrenderer-encode-reference-frames.patch are deliberately
                    # NOT wired: together with the guest half they hang
                    # real-content encodes at frame 0 (and the DPB fill hangs even
                    # a 6-frame testsrc encode). Kept in-tree for the next
                    # attempt; see tickets/04-*.md
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
