# Prebuilt Mesa carrying cang's VA-API encode fixes and the headless virtio-gpu
# DMA-BUF modifier fix, downloaded from cang's own release assets and reconstructed
# with autoPatchelfHook. `.github/workflows/build-mesa.yml` builds it from the same
# patch set nix/lib/mesa-patched.nix applies to the source build, so a downstream
# host can take the fix without a Mesa source build.
#
# Modelled on the headless flake's `mesa/prebuilt-package.nix`, which does the same
# for its virtio-gpu modifier fix.
{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  autoAddDriverRunpath,
  patchelf,
  releaseAsset,
  runtimeDeps,
  vulkanLoader,
}:

stdenvNoCC.mkDerivation {
  pname = "mesa";
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

  buildInputs = map lib.getLib runtimeDeps;
  runtimeDependencies = map lib.getLib runtimeDeps;
  # libgallium dlopens libvulkan.so.1 (zink), which auto-patchelf only folds into
  # executables' RUNPATH; append the loader's lib dir for the reconstructed
  # libraries, mirroring the source build's patchelf --add-rpath in postFixup.
  appendRunpaths = [ "${lib.getLib vulkanLoader}/lib" ];

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

    # The tarball was produced by a build whose store path differs from this one;
    # rewrite the embedded build-time path to $out.
    icd="$out/share/vulkan/icd.d/radeon_icd.x86_64.json"
    if [ -f "$icd" ]; then
      old_hash="$(sed -n 's|.*/nix/store/\([^-]*\)-mesa-[^/]*/lib.*|\1|p' "$icd")"
      if [ -n "$old_hash" ]; then
        old_path="/nix/store/$old_hash-mesa-${releaseAsset.version}"
        find "$out" -type f -exec sed -i "s|$old_path|$out|g" {} +
      fi
    fi

    if [ -d "$out/bin" ]; then
      patchShebangs --update "$out/bin"
    fi

    patchelf --add-rpath "$out/lib" "$out"/lib/*.so 2>/dev/null || true

    runHook postInstall
  '';

  passthru = {
    sourceRevision = releaseAsset.revision or null;
    inherit (releaseAsset) system;
  };

  meta = {
    description = "Prebuilt Mesa with cang's VA-API encode fixes and the headless virtio-gpu modifier fix";
    homepage = "https://github.com/${releaseAsset.owner}/${releaseAsset.repo}/releases/tag/${releaseAsset.tag}";
    license = lib.licenses.mit;
    platforms = [ releaseAsset.system ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
