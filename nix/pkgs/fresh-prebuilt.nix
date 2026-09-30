{ pkgs, pins }:
let
  freshPrebuiltRelease = pins.freshPrebuiltRelease;
  prebuiltSystem = pkgs.stdenv.hostPlatform.system;
  supportedSystems = builtins.attrNames freshPrebuiltRelease.systems;
in
if builtins.hasAttr prebuiltSystem freshPrebuiltRelease.systems then
  let
    assetInfo = builtins.getAttr prebuiltSystem freshPrebuiltRelease.systems;
    releaseUrl = "https://github.com/${freshPrebuiltRelease.owner}/${freshPrebuiltRelease.repo}/releases/download/${freshPrebuiltRelease.tag}/${assetInfo.asset}";
  in
  pkgs.stdenvNoCC.mkDerivation {
    pname = "fresh";
    version = pkgs.lib.removePrefix "v" freshPrebuiltRelease.tag;

    src = pkgs.fetchurl {
      url = releaseUrl;
      hash = assetInfo.hash;
    };

    # Upstream's musl build is a static-PIE ELF (no PT_INTERP), so there is
    # nothing to patch and patchelf would corrupt it.
    dontPatchELF = true;
    dontBuild = true;

    installPhase = ''
      runHook preInstall
      install -Dm755 ./fresh "$out/bin/fresh"
      runHook postInstall
    '';

    passthru = {
      inherit releaseUrl;
      releaseTag = freshPrebuiltRelease.tag;
    };

    meta = {
      description = "Prebuilt fresh terminal text editor binary (static musl)";
      homepage = "https://github.com/${freshPrebuiltRelease.owner}/${freshPrebuiltRelease.repo}";
      license = pkgs.lib.licenses.gpl3Only;
      mainProgram = "fresh";
      platforms = supportedSystems;
      sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
    };
  }
else
  throw ''
    fresh-prebuilt is not pinned for ${prebuiltSystem}.
    Supported systems: ${pkgs.lib.concatStringsSep ", " supportedSystems}
  ''
