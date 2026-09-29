{ pkgs, pins }:

let
  piAiNpmTarball = pkgs.fetchurl {
    url = "https://registry.npmjs.org/@earendil-works/pi-ai/-/pi-ai-${pins.piCodingAgent.version}.tgz";
    hash = pins.piCodingAgent.aiNpmTarballHash;
  };

  # The native addon runtime directory: prebuilt addons that pi extensions
  # install at runtime (magic-context's `sharp` and `onnxruntime-node`, for
  # example) are glibc ELF objects with `DT_NEEDED libstdc++.so.6`. The pi
  # binary itself is a bun standalone artifact whose `DT_NEEDED` set is libc,
  # ld-linux, libpthread, libdl and libm with no `RUNPATH` at all, so nothing in
  # its own chain can satisfy a dependency of a library it opens later - and a
  # Nix environment has no `/lib`, no `/usr/lib` and no `ld.so.cache` to fall
  # back on. This directory exposes exactly the soname those addons are missing
  # and nothing else, because `LD_LIBRARY_PATH` is searched ahead of a binary's
  # own `DT_RUNPATH`.
  nativeAddonRuntimeDir = pkgs.runCommand "cang-native-addon-runtime" { } ''
    mkdir -p "$out/lib"
    ln -s ${pkgs.stdenv.cc.cc.lib}/lib/libstdc++.so.6 "$out/lib/libstdc++.so.6"
  '';
in
pkgs.buildNpmPackage {
  pname = "pi-coding-agent";
  version = pins.piCodingAgent.version;

  src = pkgs.fetchFromGitHub {
    owner = pins.piCodingAgent.owner;
    repo = pins.piCodingAgent.repo;
    rev = pins.piCodingAgent.rev;
    hash = pins.piCodingAgent.srcHash;
  };

  npmDepsHash = pins.piCodingAgent.npmDepsHash;
  npmDepsFetcherVersion = 2;
  npmWorkspace = "packages/coding-agent";
  npmRebuildFlags = [ "--ignore-scripts" ];

  postPatch = ''
    cp ${./pi-coding-agent-package.json} package.json
    cp ${./pi-coding-agent-package-lock.json} package-lock.json
    substituteInPlace packages/coding-agent/package.json \
      --replace-fail 'npm --prefix ../ai run build' \
                     'npm --prefix ../ai run build:offline'

    mkdir -p packages/ai/src/providers/data
    tar -xzf ${piAiNpmTarball} --strip-components=4 \
      -C packages/ai/src/providers/data \
      package/dist/providers/data
  '';

  nativeBuildInputs = [
    pkgs.bun
    pkgs.makeWrapper
  ];
  npmBuildScript = "build:binary";

  installPhase = ''
    runHook preInstall

    mkdir -p $out/lib/pi-coding-agent $out/bin
    cp -R packages/coding-agent/dist/. $out/lib/pi-coding-agent/
    chmod +x $out/lib/pi-coding-agent/pi

    # `bin/pi` is a wrapper so that native addons loaded by extensions can find
    # the C++ runtime they need; `lib/pi-coding-agent/pi` stays the raw binary.
    makeWrapper $out/lib/pi-coding-agent/pi $out/bin/pi \
      --prefix LD_LIBRARY_PATH : ${nativeAddonRuntimeDir}/lib

    install -Dm644 packages/coding-agent/README.md $out/share/doc/pi-coding-agent/README.md
    install -Dm644 packages/coding-agent/CHANGELOG.md $out/share/doc/pi-coding-agent/CHANGELOG.md
    cp -R packages/coding-agent/docs $out/share/doc/pi-coding-agent/docs

    runHook postInstall
  '';

  nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
  doInstallCheck = true;
  versionCheckProgram = "${placeholder "out"}/bin/pi";
  versionCheckProgramArg = "--version";

  passthru = {
    inherit nativeAddonRuntimeDir;
    sourceUrl = "https://github.com/${pins.piCodingAgent.owner}/${pins.piCodingAgent.repo}/tree/${pins.piCodingAgent.rev}/packages/coding-agent";
  };

  meta = {
    description = "Minimal terminal coding harness";
    homepage = "https://github.com/${pins.piCodingAgent.owner}/${pins.piCodingAgent.repo}/tree/main/packages/coding-agent";
    license = pkgs.lib.licenses.mit;
    mainProgram = "pi";
    platforms = [
      "aarch64-linux"
      "x86_64-linux"
    ];
    sourceProvenance = [ pkgs.lib.sourceTypes.fromSource ];
  };
}
