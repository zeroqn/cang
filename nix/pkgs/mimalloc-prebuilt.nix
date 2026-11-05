# Prebuilt mimalloc, taken from zeroqn/headless's published release asset and
# reconstructed with autoPatchelfHook, so neither the guest image nor the
# launcher's render-server preload has to build mimalloc from source - and both
# preload the same object (`nix/lib/mimalloc-cang.nix`).
#
# Modelled on that repository's `mimalloc/prebuilt-package.nix`, which is what
# builds and publishes the asset; the only difference is the derivation name,
# because this is the image's default allocator rather than a package called
# `mimalloc-bin`.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  releaseAsset,
  runtimeDeps,
}:

stdenvNoCC.mkDerivation {
  pname = "mimalloc";
  inherit (releaseAsset) version;

  src = fetchurl {
    inherit (releaseAsset) url hash;
  };

  dontConfigure = true;
  dontBuild = true;

  nativeBuildInputs = [
    autoPatchelfHook
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
    runHook postInstall
  '';

  meta = {
    description = "Compact, fast, general-purpose memory allocator (prebuilt release asset)";
    homepage = "https://github.com/${releaseAsset.owner}/${releaseAsset.repo}/releases/tag/${releaseAsset.tag}";
    license = lib.licenses.bsd2;
    platforms = [ releaseAsset.system ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
