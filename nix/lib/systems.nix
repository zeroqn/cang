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
                    # NOTE: mesa-virgl-rbsp-bounds.patch is deliberately NOT wired yet. It stops
                    # vl_rbsp_ue()/the slice parser's `while (true)` loops/the packed-header scan
                    # from running past the end of an RBSP - which is genuinely where the guest's
                    # CQP encode stalls (proven host-side, and the markers show the parse then
                    # progress from the SPS through to the IDR slice) - but with it wired the
                    # previously-working bitrate arm hangs too, so it is not a safe fix as written.
                    # See docs/wayfinder/guest-vaapi-video/tickets/08-cqp-encode-hangs.md
                    # NOTE: mesa-virgl-encode-caps.patch is deliberately NOT
                    # wired: with it the guest sees non-zero reference counts
                    # (or, against an unpatched host, whatever sits in the
                    # structure's reserved bits) and real-content encodes hang at
                    # frame 0. Kept in-tree for the next attempt; see
                    # docs/wayfinder/guest-vaapi-video/tickets/04-*.md
                  ];
                });
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
