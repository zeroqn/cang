# cang's virglrenderer: the source build with the pinned upstream revision and
# cang's host-side patches.
#
# The revision is a post-1.3.0 upstream main commit, chosen for the vrend
# robustness fixes it brings and for the amdgpu `inttypes` fix that nixpkgs
# otherwise carries as its own patch. It is not chosen for the panfrost
# `res_id_count` fix in it: that is unreachable from cang, which does not support
# ARM/Mali hosts and does not compile the panfrost renderer in (`mesonFlags` in
# nixpkgs builds `-Ddrm-renderers=amdgpu-experimental,asahi,msm`). Two
# consequences of moving off the 1.3.0 tarball:
#  * that revision builds venus-protocol as a meson subproject instead of
#    vendoring it, and a Nix build has no network, so the wrap's checkout is
#    vendored from `pins.virglrendererSource` in `postPatch`;
#  * nixpkgs' own 1.3.0 patch (the amdgpu inttypes fix) is already upstream
#    there, so `patches` is replaced instead of extended.
#
# `virglrenderer` is passed in rather than read from `pkgs`, so the source build
# stays reachable once `pkgs.virglrenderer` is the prebuilt-or-source selector
# (nix/lib/virglrenderer-cang.nix) - and so `.#virglrenderer-release-build` can
# build the source even when a prebuilt asset is pinned.
#
# Consumers:
#  * `nix/lib/virglrenderer-cang.nix` picks this source build when cang's
#    prebuilt virglrenderer has no release asset for the system (otherwise it
#    uses the prebuilt - see `nix/pkgs/virglrenderer-prebuilt.nix` and
#    `.github/workflows/build-virglrenderer.yml`);
#  * `pkgs.virglrenderer` in nix/lib/systems.nix's overlay, which is what the
#    cang packages link and what the render server is symlinked from;
#  * `overlays.default` / `packages.virglrenderer` in flake.nix, for a downstream
#    host that builds its own vrend/libkrun stack.
{
  pkgs,
  pins,
  virglrenderer,
}:
let
  source = pins.virglrendererSource;
  # The wrap's checkout directory (`venus-protocol.wrap`'s `directory`), i.e.
  # `venus-protocol-<tag without v>`.
  venusProtocolDirectory = "venus-protocol-${pkgs.lib.removePrefix "v" source.venusProtocolTag}";
in
virglrenderer.overrideAttrs (old: {
  src = pkgs.fetchFromGitLab {
    domain = "gitlab.freedesktop.org";
    owner = "virgl";
    repo = "virglrenderer";
    inherit (source) rev hash;
  };

  patches = [
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

  # venus-protocol generates its headers with mako, which nixpkgs' pyyaml-only
  # build python does not carry. Put a python with both first on PATH, so the
  # top-level codegen and the vendored subproject (below) resolve the same one.
  nativeBuildInputs = [
    (pkgs.buildPackages.python3.withPackages (ps: [
      ps.pyyaml
      ps.mako
    ]))
  ]
  ++ old.nativeBuildInputs;

  # The venus-protocol subproject the revision builds as a meson wrap. Meson
  # looks for the wrap's `directory` first and uses it as-is, so an unpacked
  # checkout there satisfies the fallback without a download.
  postPatch = (old.postPatch or "") + ''
    cp -r ${
      pkgs.fetchFromGitLab {
        domain = "gitlab.freedesktop.org";
        owner = "virgl";
        repo = "venus-protocol";
        tag = source.venusProtocolTag;
        hash = source.venusProtocolHash;
      }
    } subprojects/${venusProtocolDirectory}
    chmod -R u+w subprojects/${venusProtocolDirectory}
  '';
})
