{
  nixpkgs,
  fenix,
  pins,
}:
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
      let
        pkgs =
          (import nixpkgs {
            inherit system;
          }).extend
            (
              final: prev: {
                # cang's mesa under the name the image and the render-server
                # environment both use. `pkgs.mesa` stays the plain nixpkgs drop on
                # purpose: the guest's GL/Vulkan/software-renderer stack takes that
                # one, while the guest's VA-API driver and the host render-server
                # environment take this one - the image's wrapper contracts assert
                # the two really are different, so a stock mesa cannot quietly take
                # over the VA path (ticket 08). See nix/lib/mesa-patched.nix.
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
      in
      f {
        inherit system pkgs;
        # fenix's current stable Rust: the dev shell, `nix build` and the guest
        # image's rust layer all take it from here, so they cannot drift apart.
        rustToolchain = import ./rust-toolchain.nix {
          inherit pkgs;
          fenix = fenix.packages.${system};
        };
      }
    );
in
{
  inherit systems forAllSystems;
}
