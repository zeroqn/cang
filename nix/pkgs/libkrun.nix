{
  pkgs,
  pins,
  ...
}:

let
  lib = pkgs.lib;
  system = pkgs.stdenv.hostPlatform.system;
  release = pins.libkrunRelease;
  systemPins = release.systems.${system} or (throw "unsupported libkrun system: ${system}");
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "libkrun";
  version = "${release.tag}";

  nativeBuildInputs = [ pkgs.patchelf ];

  src = pkgs.fetchurl {
    url = "https://github.com/${release.owner}/${release.repo}/releases/download/${release.tag}/${systemPins.asset}";
    hash = systemPins.hash;
  };

  sourceRoot = ".";

  installPhase = ''
    runHook preInstall

    mkdir -p "$out/lib" "$out/include" "$out/lib/pkgconfig"
    cp -a lib64/. "$out/lib/"
    cp -a include/. "$out/include/"

    runHook postInstall
  '';

  postFixup = ''
    # The cang prebuilt libkrun has DT_NEEDED on libvirglrenderer.so.1 but ships
    # no DT_RUNPATH to locate it, so a dlopen of libkrun fails unless the search
    # path covers it. A consumer's own RUNPATH cannot cover it because
    # DT_RUNPATH is not transitive across DT_NEEDED children.
    #
    # libkrun 2.0.0 dropped the snd feature, so libpipewire is no longer a
    # DT_NEEDED edge and is deliberately absent from the runpath.
    #
    # $ORIGIN covers the firmware: libkrun and libkrun_init both carry a plain
    # soname DT_NEEDED on libkrunfw.so.5, so the loader searches the directory
    # of the caller (libkrun itself) rather than the executable's. Packages
    # that expose libkrun, libkrun_init and libkrunfw as siblings under
    # "$out/lib/cang" (cang-rust.nix, cang-prebuilt.nix) then resolve the
    # firmware from that directory and the bare cang ELF needs no wrapper
    # LD_LIBRARY_PATH; consumers that load libkrun from a directory without
    # libkrunfw are unaffected because the lookup falls through to
    # LD_LIBRARY_PATH.
    for so in "$out"/lib/libkrun.so.*; do
      [ -e "$so" ] || continue
      if [ -L "$so" ]; then
        continue
      fi
      ${pkgs.patchelf}/bin/patchelf \
        --add-rpath '$ORIGIN:${pkgs.virglrenderer}/lib' \
        "$so"
    done

    # libkrun 2.0.0 moved the guest init out of libkrun.so into its own shared
    # library, which cang dlopens; it needs only $ORIGIN for libkrunfw.so.5.
    for so in "$out"/lib/libkrun_init.so.*; do
      [ -e "$so" ] || continue
      if [ -L "$so" ]; then
        continue
      fi
      ${pkgs.patchelf}/bin/patchelf --add-rpath '$ORIGIN' "$so"
    done
  '';

  meta = {
    description = "Pinned prebuilt libkrun shared library for cang";
    homepage = "https://github.com/${release.owner}/${release.repo}";
    license = with lib.licenses; [ asl20 ];
    platforms = lib.attrNames release.systems;
  };
}
