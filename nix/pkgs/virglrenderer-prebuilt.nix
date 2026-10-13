# Prebuilt virglrenderer carrying cang's host-side VA-API encode fixes at the
# pinned upstream revision, downloaded from cang's own release assets and
# reconstructed with autoPatchelfHook. `.github/workflows/build-virglrenderer.yml` builds it from
# the same source nix/lib/virglrenderer-patched.nix applies, so a downstream host
# can take the fixes without a virglrenderer source build.
#
# Modelled on nix/pkgs/mesa-prebuilt.nix.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  patchelf,
  releaseAsset,
  runtimeDeps,
}:

stdenvNoCC.mkDerivation {
  pname = "virglrenderer";
  inherit (releaseAsset) version;

  src = fetchurl {
    inherit (releaseAsset) url hash;
  };

  dontConfigure = true;
  dontBuild = true;

  nativeBuildInputs = [
    autoPatchelfHook
    patchelf
  ];

  buildInputs = map lib.getLib runtimeDeps;
  runtimeDependencies = map lib.getLib runtimeDeps;

  unpackPhase = ''
    runHook preUnpack
    mkdir source
    tar --extract --gzip --file "$src" --directory source
    runHook postUnpack
  '';

  sourceRoot = "source";

  installPhase = ''
    runHook preInstall

    mkdir -p "$out"
    cp -a . "$out"/
    chmod -R u+w "$out"

    # Only the pkg-config file embeds the build store path in a way cang depends
    # on. The library also carries the build's `libexec/virgl_render_server` as
    # virglrenderer's own default server path, but cang never uses it: the
    # launcher resolves the helper from its own package root and passes it as
    # RENDER_SERVER_EXEC_PATH, and the server itself connects back over a
    # socketpair fd rather than a path.
    #
    # The tarball was produced by a build whose store path differs from this one;
    # rewrite every embedded build path in the pkg-config file to $out, because
    # cang's build resolves virglrenderer through it (meson writes `prefix` and,
    # for this package, an absolute `includedir`).
    pc="$out/lib/pkgconfig/virglrenderer.pc"
    if [ -f "$pc" ]; then
      built_prefix="$(sed -n 's|^prefix=\(.*\)$|\1|p' "$pc")"
      if [ -n "$built_prefix" ]; then
        sed -i "s|$built_prefix|$out|g" "$pc"
      fi
    fi

    runHook postInstall
  '';

  passthru = {
    sourceRevision = releaseAsset.revision or null;
    inherit (releaseAsset) system;
  };

  meta = {
    description = "Prebuilt virglrenderer with cang's host-side VA-API encode fixes";
    homepage = "https://github.com/${releaseAsset.owner}/${releaseAsset.repo}/releases/tag/${releaseAsset.tag}";
    license = lib.licenses.mit;
    platforms = [ releaseAsset.system ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
