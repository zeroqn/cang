# `pi`, compiled ahead of time: zeroqn/pi's `tui/flake.nix` builds stock upstream pi
# with Bun's `bun build --compile --aot` and publishes the result as the
# per-system asset of a `pi-v<version>` release, together with the Bun canary it
# compiled it with (nix/pkgs/bun-aot.nix). This derivation installs that release
# artifact and adds the two pieces cang's image needs on top of it:
#
# - `PI_PACKAGE_DIR`, pi's own escape hatch for where its package directory is
#   (config.ts), because the executable and the assets beside it live in two
#   different outputs here;
# - the native addon runtime directory and the ONNX Runtime shim the wrapper
#   hands every process pi starts. Prebuilt extension addons (magic-context's
#   `sharp` and `onnxruntime-node`) are glibc ELF objects with
#   `DT_NEEDED libstdc++.so.6`, while the AOT host's own `DT_NEEDED` set is libc,
#   ld-linux, libpthread, libdl and libm with no RUNPATH - and a Nix environment
#   has no `/lib`, no `/usr/lib` and no `ld.so.cache` to fall back on. See ADR
#   0009.
#
# This is built with the flake's nixpkgs-unstable set: the shim it exposes is
# linked against that glibc, so pi's own interpreter has to come from the same
# set or the process cannot resolve the shim's symbol versions.
{
  pkgs,
  pins,
  onnxruntimeShim,
  nativeAddonCxxRuntime,
}:

let
  release = pins.piAotRelease;
  system = pkgs.stdenv.hostPlatform.system;
  asset =
    release.systems.${system}.piAot or (throw "no ${release.tag} pi AOT asset is pinned for ${system}");
  releaseUrl = "https://github.com/${release.owner}/${release.repo}/releases/download/${release.tag}/${asset.asset}";

  # The directory the wrapper exposes to prebuilt extension addons; see ADR 0009
  # and the map under docs/wayfinder/pi-native-addons/.
  #
  # `LD_LIBRARY_PATH` is searched ahead of a binary's own `DT_RUNPATH`, so the
  # one `libstdc++.so.6` here is a ceiling for the whole process tree: it has to
  # be the newest C++ runtime the image's consumers need (`zg`'s ONNX Runtime
  # wants `GLIBCXX_3.4.35/36`, which the 26.05 input's gcc stops short of), while
  # libstdc++'s backward-compatible ABI still serves an addon built against an
  # older one. The three sonames are exactly what those consumers lack:
  # `libstdc++.so.6`, and the ONNX Runtime ABI shim under both of its names -
  # `libonnxruntime.so.1` for a binding whose `DT_NEEDED` carries that soname,
  # `libonnxrtabi24.so.1` for one built to ask the shim for itself (see
  # nix/pkgs/onnxruntime-shim.nix).
  nativeAddonRuntimeDir = pkgs.runCommand "cang-native-addon-runtime" { } ''
    mkdir -p "$out/lib"
    ln -s ${nativeAddonCxxRuntime}/lib/libstdc++.so.6 "$out/lib/libstdc++.so.6"
    ln -s ${onnxruntimeShim}/lib/libonnxruntime.so.1 "$out/lib/libonnxruntime.so.1"
    ln -s ${onnxruntimeShim}/lib/libonnxrtabi24.so.1 "$out/lib/libonnxrtabi24.so.1"
  '';
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "pi-coding-agent";
  version = release.version;

  src = pkgs.fetchurl {
    inherit (asset) hash;
    url = releaseUrl;
  };

  # The tarball holds the executable plus the assets it reads from its own
  # directory (`package.json`, `theme`, `assets`, `export-html`, `docs`,
  # `examples`, `native`, `photon_rs_bg.wasm`), so it is unpacked by hand rather
  # than by `unpackPhase`, which wants one directory.
  dontUnpack = true;

  nativeBuildInputs = [
    pkgs.autoPatchelfHook
    pkgs.makeWrapper
  ];

  # The released executable arrives with the interpreter and rpath of the nixpkgs
  # it was built against; re-patch it against this store. `libxcb` is what
  # `native/linux/prebuilds/linux-x64/linux-platform-x11.node` (the X11 platform
  # addon) NEEDs, and glibc is what the executable itself does.
  buildInputs = [
    pkgs.glibc
    pkgs.libxcb
  ];

  # An AOT-compiled Bun executable is its own runtime: the payload and the
  # compiled code must not be touched.
  dontStrip = true;
  dontPatchShebangs = true;

  installPhase = ''
    runHook preInstall

    mkdir -p unpacked $out/libexec/pi $out/share/pi
    tar -xf "$src" -C unpacked
    install -Dm755 unpacked/pi $out/libexec/pi/pi
    cp -R unpacked/. $out/share/pi/
    rm $out/share/pi/pi

    # `bin/pi` is the wrapper, `libexec/pi/pi` the released executable. Both
    # variables are defaults: the image exports ONNXRUNTIME_LIB_PATH too, and a
    # wrapper may not override a value pi's user set.
    makeWrapper $out/libexec/pi/pi $out/bin/pi       --set-default PI_PACKAGE_DIR $out/share/pi       --prefix LD_LIBRARY_PATH : ${nativeAddonRuntimeDir}/lib       --set-default ONNXRUNTIME_LIB_PATH ${onnxruntimeShim.onnxruntimeLibDir}

    runHook postInstall
  '';

  nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
  doInstallCheck = true;
  versionCheckProgram = "${placeholder "out"}/bin/pi";
  versionCheckProgramArg = "--version";

  passthru = {
    inherit nativeAddonRuntimeDir releaseUrl;
    releaseTag = release.tag;
    sourceUrl = "https://github.com/${release.owner}/${release.repo}/tree/${release.tag}";
  };

  meta = {
    description = "pi ${release.version}, compiled ahead of time";
    homepage = "https://github.com/${release.owner}/${release.repo}";
    license = pkgs.lib.licenses.mit;
    mainProgram = "pi";
    platforms = builtins.attrNames release.systems;
    sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
  };
}
