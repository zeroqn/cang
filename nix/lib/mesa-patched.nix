# cang's mesa override, shared by every consumer that needs the VA-API encode fix:
#
#  * the guest image (`mesaVaApi` in nix/lib/systems.nix), whose libgallium is the
#    guest's VA-API driver;
#  * the host-side overlay (`overlays.default` in flake.nix), so a downstream host
#    that runs its own mesa VA-API driver (a virtio-gpu host, radeonsi, ...) gets the
#    same fix - cang cannot reach the host's VA driver by configuration, because the
#    VM worker runs in glibc secure-execution mode and libva's secure_getenv()
#    ignores LIBVA_DRIVERS_PATH there.
#
# Both patches are applied together: `mesa-virgl-encode-raw-headers.patch` carries
# the guest half of the vrend video wire extension, and
# `mesa-virgl-rbsp-bounds.patch` stops the packed-header RBSP readers looping for
# ever on a truncated header. See
# docs/wayfinder/guest-vaapi-video/tickets/08-cqp-encode-hangs.md.
mesa:
mesa.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    ../pkgs/patches/mesa-virgl-encode-raw-headers.patch
    ../pkgs/patches/mesa-virgl-rbsp-bounds.patch
  ];
})
