{ pkgs, pins }:
let
  kachePrebuiltRelease = pins.kachePrebuiltRelease;
  prebuiltSystem = pkgs.stdenv.hostPlatform.system;
in
if builtins.hasAttr prebuiltSystem kachePrebuiltRelease.systems then
  let
    assetInfo = builtins.getAttr prebuiltSystem kachePrebuiltRelease.systems;
    releaseUrl = "https://github.com/${kachePrebuiltRelease.owner}/${kachePrebuiltRelease.repo}/releases/download/${kachePrebuiltRelease.tag}/${assetInfo.asset}";
  in
  pkgs.stdenvNoCC.mkDerivation {
    pname = "kache";
    version = kachePrebuiltRelease.version;
    src = pkgs.fetchurl {
      url = releaseUrl;
      hash = assetInfo.hash;
    };
    # The tarball is flat - `kache`, `kache-engine`, `kache-cl`, `kache-clang-cl`
    # at the root, each with its `.asc` signature beside it - so there is no
    # source root to strip and `dontUnpack` keeps the install phase explicit.
    dontUnpack = true;
    # Upstream ships static-PIE musl ELFs; patchelf would corrupt them, and the
    # image runs them without a dynamic loader of their own.
    dontPatchELF = true;

    installPhase = ''
      runHook preInstall
      mkdir -p "$out/bin"
      tar -xzf "$src" -C "$out/bin" --no-same-owner --exclude='*.asc'
      for binary in kache kache-engine kache-cl kache-clang-cl; do
        chmod 755 "$out/bin/$binary"
      done
      # `kache` resolves the engine beside itself, so both have to ship: the
      # daemon does the cache work, the client is what `RUSTC_WRAPPER` names.
      test -x "$out/bin/kache-engine"
      runHook postInstall
    '';

    passthru = {
      inherit releaseUrl;
      releaseTag = kachePrebuiltRelease.tag;
    };

    meta = {
      description = "Prebuilt kache compiler cache (kache and its kache-engine)";
      homepage = "https://github.com/${kachePrebuiltRelease.owner}/${kachePrebuiltRelease.repo}";
      license = pkgs.lib.licenses.asl20;
      mainProgram = "kache";
      platforms = builtins.attrNames kachePrebuiltRelease.systems;
      sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
    };
  }
else
  # The image wires kache in unconditionally, so a system the pin does not list
  # is a pin bug to report rather than a package to drop.
  throw "kache prebuilt release ${kachePrebuiltRelease.tag} publishes no asset for ${prebuiltSystem}"
