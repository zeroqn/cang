{ nixpkgs, pins }:
let
  mesaCang = pkgs: import ./mesa-cang.nix { inherit pkgs pins; };
  virglrendererCang = pkgs: import ./virglrenderer-cang.nix { inherit pkgs pins; };
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
                # `overlays.default` / `packages.mesa-cang` in flake.nix.
                mesaVaApi = mesaCang prev;
                # The host half of the same video path: the cang packages link
                # libvirglrenderer and spawn its render server, so they need cang's
                # encode/present patches. The prebuilt release asset when one is
                # published for the system, otherwise the same source build; the
                # flake exports the same override to hosts as `overlays.default` /
                # `packages.virglrenderer`. See nix/lib/virglrenderer-cang.nix.
                virglrenderer = virglrendererCang prev;
              }
            );
      }
    );
in
{
  inherit systems forAllSystems;
}
