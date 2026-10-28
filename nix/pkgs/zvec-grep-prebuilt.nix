# Prebuilt zg (the zvec-grep Rust rewrite at the pinned revision), downloaded from
# cang's own release assets and reconstructed with autoPatchelfHook.
# `.github/workflows/build-zg.yml` builds it from the same source
# nix/pkgs/zvec-grep.nix compiles, so a container build can skip the
# llama.cpp/bindgen/onnxruntime compile entirely.
#
# Modelled on nix/pkgs/virglrenderer-prebuilt.nix.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  patchelf,
  onnxruntime,
  releaseAsset,
  runtimeDeps,
}:

stdenvNoCC.mkDerivation {
  pname = "zvec-grep";
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
    # The archive carries `lib/libonnxruntime.so.1` as a symlink into the store
    # that built it, because zg loads ONNX Runtime at run time from `../lib`
    # rather than linking it. Rebind it to this evaluation's ONNX Runtime.
    rm -f "$out/lib/libonnxruntime.so.1"
    mkdir -p "$out/lib"
    ln -s ${onnxruntime}/lib/libonnxruntime.so.1 "$out/lib/libonnxruntime.so.1"
    runHook postInstall
  '';

  passthru = {
    sourceRevision = releaseAsset.revision or null;
    inherit (releaseAsset) system;
  };

  meta = {
    description = "Prebuilt zg, the zvec-grep Rust rewrite";
    homepage = "https://github.com/${releaseAsset.owner}/${releaseAsset.repo}/releases/tag/${releaseAsset.tag}";
    license = lib.licenses.asl20;
    platforms = [ releaseAsset.system ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
