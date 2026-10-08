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
                    # Encoder-attribute forwarding (the guest half is
                    # mesa-virgl-encode-caps.patch) and the reference/DPB fill
                    # that makes the forwarded B-frames honour their references.
                    # Both are needed together: the caps alone advertise future
                    # references the host DPB cannot keep. See tickets/04-*.md.
                    ../pkgs/patches/virglrenderer-encode-caps.patch
                    ../pkgs/patches/virglrenderer-encode-reference-frames.patch
                    # Guest rate control. The wire already carries the client's
                    # RC mode with the picture description, but the host driver
                    # used to ignore it: virgl_video_create_codec() asked for
                    # only VAConfigAttribRTFormat, so the context stayed in
                    # PIPE_H2645_ENC_RATE_CONTROL_METHOD_DISABLE. The -config
                    # patch creates the VA config/context lazily from the first
                    # picture's rate_ctrl_method (recreating on a mid-stream
                    # change), and the target_percentage patch recovers the
                    # client's target percentage correctly - VBR needs both. See
                    # tickets/04-*.md.
                    ../pkgs/patches/virglrenderer-encode-rate-control.patch
                    ../pkgs/patches/virglrenderer-encode-rate-control-config.patch
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
