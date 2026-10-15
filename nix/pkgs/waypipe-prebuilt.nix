# Prebuilt waypipe, taken from zeroqn/headless's published release asset and
# reconstructed with autoPatchelfHook, so the cang guest does not have to build
# waypipe (and its Rust/ffmpeg dependency set) from source.
#
# Modelled on that repository's `waypipe/prebuilt-package.nix`, which is what
# builds and publishes the asset; the only difference is the derivation name,
# because this is the guest image's `waypipe` rather than a package called
# `waypipe-bin`.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  autoAddDriverRunpath,
  patchelf,
  releaseAsset,
  runtimeDeps,
}:

stdenvNoCC.mkDerivation {
  pname = "waypipe";
  inherit (releaseAsset) version;

  src = fetchurl {
    inherit (releaseAsset) url hash;
  };

  dontConfigure = true;
  dontBuild = true;

  nativeBuildInputs = [
    autoPatchelfHook
    autoAddDriverRunpath
    patchelf
  ];

  # waypipe dlopens its compressor (`lz4`/`zstd`) and, for a dmabuf transfer, the
  # GBM/ffmpeg/Vulkan libraries, so the runtime deps have to be on its RPATH even
  # though only zstd/lz4/libgcc/libc are DT_NEEDED. Same list as the headless
  # flake's `waypipeRuntimeDeps`.
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

  passthru = {
    sourceRevision = releaseAsset.revision or null;
    inherit (releaseAsset) system;
  };

  meta = {
    description = "Prebuilt waypipe (zeroqn/headless release asset)";
    homepage = "https://github.com/${releaseAsset.owner}/${releaseAsset.repo}/releases/tag/${releaseAsset.tag}";
    license = lib.licenses.mit;
    mainProgram = "waypipe";
    platforms = [ releaseAsset.system ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
