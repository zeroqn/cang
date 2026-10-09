# cang's mesa override: the source-built variant with cang's VA-API encode fixes and
# the headless virtio-gpu DMA-BUF modifier fix.
#
# Consumers:
#  * `nix/lib/mesa-cang.nix` picks this source build when cang's prebuilt mesa has no
#    release asset for the system (otherwise it uses the prebuilt - see
#    `nix/pkgs/mesa-prebuilt.nix` and `.github/workflows/build-mesa.yml`);
#  * the guest image's VA-API driver (`mesaVaApi` in nix/lib/systems.nix);
#  * `overlays.default` / `packages.mesa-cang` in flake.nix, which a downstream
#    host applies to its own nixpkgs - a host whose VA driver is mesa (virtio-gpu,
#    radeonsi, ...) hits the same hang in vl_rbsp_ue() when vrend hands it a packed
#    header, and cang cannot reach that driver by configuration (the VM worker runs in
#    glibc secure-execution mode, where libva's secure_getenv() ignores
#    LIBVA_DRIVERS_PATH).
#
# See docs/wayfinder/guest-vaapi-video/tickets/08-cqp-encode-hangs.md.
mesa:
mesa.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    # the guest half of the vrend video wire extension
    ../pkgs/patches/mesa-virgl-encode-raw-headers.patch
    # stop the packed-header RBSP readers looping for ever on a truncated header
    ../pkgs/patches/mesa-virgl-rbsp-bounds.patch
    # host-side: report DMA-BUF modifiers on a virtio-gpu render node (shared with the
    # headless flake; same fix cang's host vrend needs on such a host)
    ../pkgs/patches/mesa-headless-virtio-modifiers.patch
    # forward the host driver's encoder attributes (past/future reference counts)
    # so a VA client can build a B-frame GOP - the host half is
    # virglrenderer-encode-caps.patch
    ../pkgs/patches/mesa-virgl-encode-caps.patch
  ];
})
