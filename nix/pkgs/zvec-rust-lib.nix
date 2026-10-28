# zvec's C API shared library, taken from zvec-ai/zvec-rust's own prebuilt
# release asset. `libzvec_c_api.so` and the jieba dictionary beside it are what
# `zvec-rust-sys` resolves through `ZVEC_LIB_DIR` (its order is env var, sibling
# checkout, vendor dir, GitHub download, clone alibaba/zvec), so pointing that at
# this output keeps the zvec-grep build offline and the C++/Rust ABI matched to
# the crate version the workspace pins. The dictionary is found relative to the
# loaded library (`dladdr`), which is why both ship in `$out/lib`.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  glibc,
  releaseAsset,
}:

stdenvNoCC.mkDerivation {
  pname = "zvec-c-api";
  inherit (releaseAsset) version;

  # `fetchurl`, not `fetchzip`: the recorded hash is the tarball's own, and
  # stdenv's unpackPhase extracts the single archive root for us.
  src = fetchurl {
    inherit (releaseAsset) url hash;
  };

  sourceRoot = ".";
  dontConfigure = true;
  dontBuild = true;

  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [ glibc ];

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/lib"
    cp -a libzvec_c_api.so "$out/lib/"
    # The jieba dictionary ships beside the library while the release still
    # carries it (v0.7.2 and earlier); v0.8.0 dropped the directory, so copy it
    # only when the archive has one.
    if [ -d data ]; then
      cp -a data "$out/lib/"
    fi
    runHook postInstall
  '';

  meta = {
    description = "zvec's C API shared library and jieba dictionary (upstream prebuilt release asset)";
    homepage = "https://github.com/${releaseAsset.owner}/${releaseAsset.repo}";
    license = lib.licenses.asl20;
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
