# The Bun the AOT host was compiled with: an upstream canary, which is the only
# Bun line with `bun build --compile --aot`. zeroqn/pi's release carries the exact
# archive beside the host it compiled (its `tui/nix/runtime.nix` prefers that
# copy over oven-sh's movable `canary` tag, whose assets are replaced daily), so
# cang pins those bytes rather than the moving tag.
#
# A canary is also newer than the 1.4.4 release cang used to pin: the root store
# compiled into this binary is not the one that trusted the compromised
# eMudhra/emSign intermediates (CVE-2026-111943).
#
# `dontStrip`/`dontPatchELF`: a Bun executable is its own runtime - `--compile`
# appends a `.bun_aot` payload to it - so its sections and rpath are not ours to
# rewrite, while autoPatchelfHook still has to move its `/lib64` interpreter into
# the store.
{ pkgs, pins }:

let
  release = pins.piAotRelease;
  system = pkgs.stdenv.hostPlatform.system;
  asset =
    release.systems.${system}.bunAot
      or (throw "no ${release.tag} bun AOT asset is pinned for ${system}");
  releaseUrl = "https://github.com/${release.owner}/${release.repo}/releases/download/${release.tag}/${asset.asset}";
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "bun-aot";
  # The executable's own version, not the archive's canary-date label.
  version = release.bunVersion;

  src = pkgs.fetchurl {
    inherit (asset) hash;
    url = releaseUrl;
  };

  nativeBuildInputs = [
    pkgs.autoPatchelfHook
    pkgs.unzip
  ];

  dontStrip = true;
  dontPatchELF = true;
  appendRunpaths = [ ];

  unpackPhase = ''
    runHook preUnpack
    unzip -q "$src"
    runHook postUnpack
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    install -m755 */bun $out/bin/bun
    runHook postInstall
  '';

  doInstallCheck = true;
  nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
  versionCheckProgram = "${placeholder "out"}/bin/bun";
  versionCheckProgramArg = "--version";

  passthru = {
    inherit releaseUrl;
    inherit (release) bunRevision;
    releaseTag = release.tag;
  };

  meta = {
    description = "Bun ${release.bunVersion} (canary) with the ahead-of-time compiler";
    homepage = "https://github.com/oven-sh/bun";
    license = pkgs.lib.licenses.mit;
    mainProgram = "bun";
    platforms = builtins.attrNames release.systems;
    sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
  };
}
