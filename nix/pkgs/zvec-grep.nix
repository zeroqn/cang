# The Rust `zg` — zvec-grep's rewrite under `rust/` in the same upstream
# repository — which is what the zvec-grep pi extension drives.
#
# Upstream publishes no Rust binary at all: their workflow packs npm tarballs and
# Actions artifacts only, the platform packages declared in
# `rust/npm/platforms.json` (`@zvec/zvec-grep-linux-x64-gnu`) answer 404 on the
# registry, and the released `@zvec/zvec-grep` is still the TypeScript build. So
# the binary is compiled here (and, once published,
# `.github/workflows/build-zg.yml` reuses this same derivation to build the
# prebuilt asset nix/lib/zvec-grep-cang.nix can prefer). Nothing below needs node
# or npm.
#
# `pkgs` must be a nixpkgs whose rustc satisfies the workspace's
# `rust-version = "1.98"`, and whose ONNX Runtime is recent enough to serve the
# API version the pinned `ort` binding asks for (21 at this revision, loaded at
# run time through `load-dynamic`); the flake passes nixpkgs-unstable for both.
{ pkgs, pins }:

let
  zvecGrep = pins.zvecGrep;
  zvecRustLib = pins.zvecRustLib;

  zvecLib = pkgs.callPackage ./zvec-rust-lib.nix {
    releaseAsset = zvecRustLib // {
      url = "https://github.com/${zvecRustLib.owner}/${zvecRustLib.repo}/releases/download/v${zvecRustLib.version}/${zvecRustLib.asset}";
    };
  };

  # `llama-cpp-sys-2` builds llama.cpp from the source revision named in its own
  # `build_support/source-lock.json` and refuses any tree whose portable SHA-256
  # differs from the pin there. The crate revision Nix vendors carries no
  # `llama.cpp/` directory, so without this the build script would `curl` the
  # archive mid-build; hand it the same archive instead, and let its own
  # verification double-check what Nix already pinned.
  llamaCppSource = pkgs.fetchurl {
    url = "https://codeload.github.com/ggml-org/llama.cpp/tar.gz/5f55650a78f92aff4d48d671423e888fac0469ff";
    hash = "sha256-++BXvv3UkEatu82fGa004EyYFVh0TVXbGHAvQdTQGwc=";
  };
in
pkgs.rustPlatform.buildRustPackage (finalAttrs: {
  pname = "zvec-grep";
  # The Rust workspace still says 0.0.1 (`[workspace.package]` in rust/Cargo.toml,
  # never bumped since the rewrite landed), so the release is stamped here. The
  # extension's argv generation is keyed on `>= 0.2.1` (`styleForVersion` in
  # extensions/packages/zvec-grep/src/cli.ts), and a workspace manifest the node
  # CLI seeded is refused outright, so the stamp says what the binary actually is.
  version = zvecGrep.version;

  src = pkgs.fetchFromGitHub {
    inherit (zvecGrep) owner repo rev;
    hash = zvecGrep.srcHash;
  };
  # The format catalog is an explicit registry, and ArkTS and GLSL are not in
  # it: an unregistered extension is not chunked as text, it is recorded as
  # `SkippedFileReason::Unsupported`, so a HarmonyOS project's `.ets` sources
  # and its `.frag`/`.vert` shaders never reach the index. Registering them as
  # plain-text `[Code]` formats is the path `.sh`, `.css`, `.kt` and `.swift`
  # already take.
  patches = [
    ./patches/zvec-grep-arkts-glsl-formats.patch
  ];

  # The Cargo workspace lives in `rust/`, not at the repository root, and
  # `buildAndTestSubdir` has to be said twice: `cargoLock` reads the lock from
  # `cargoRoot`, while `cargoBuildHook` builds in `buildAndTestSubdir`.
  cargoRoot = "rust";
  buildAndTestSubdir = "rust";
  # `llama-cpp-2`/`llama-cpp-sys-2` come from zvec's llama.cpp fork (a git
  # dependency in the lock), which `importCargoLock` cannot vendor without a hash
  # per locked package; both crates resolve to the same checkout of that revision.
  cargoLock = {
    lockFile = "${finalAttrs.src}/rust/Cargo.lock";
    outputHashes = {
      "llama-cpp-2-0.1.154" = "sha256-l7anKvQBgJVJ3jpn+83Ulp7ZxgbmQt2QH1ytfHx+E5o=";
      "llama-cpp-sys-2-0.1.154" = "sha256-l7anKvQBgJVJ3jpn+83Ulp7ZxgbmQt2QH1ytfHx+E5o=";
    };
  };

  # The extension asks the binary for its version, so stamp the source rather
  # than the manifest: `cargoLock` vendors from the lock shipped in `src`, and
  # cargoSetupPostPatchHook fails the build if the tree's Cargo.lock no longer
  # matches it byte for byte — so bumping `[workspace.package] version` (plus the
  # lock entries for the workspace crates) is not available here. This is every
  # `env!("CARGO_PKG_VERSION")` site: `--version`, `--help`, the daemon handshake
  # version and the MCP server's.
  postPatch = ''
    substituteInPlace \
      rust/crates/zg/src/main.rs \
      rust/crates/zg-cli/src/render.rs \
      rust/crates/zg-daemon/src/runtime.rs \
      rust/crates/zg-transport-mcp/src/lib.rs \
      --replace-fail 'env!("CARGO_PKG_VERSION")' '"${finalAttrs.version}"'
  '';

  # `llama-cpp-sys-2` compiles the llama.cpp it vendors (`zg-engine` takes
  # llama-cpp-2 with default-features = false, so the openmp and common features
  # are off) and drives bindgen off libclang; that is the slow part of this build.
  nativeBuildInputs = [
    pkgs.cmake
    pkgs.autoPatchelfHook
    pkgs.rustPlatform.bindgenHook
  ];

  # In `buildInputs`, not only in `env`, so `autoPatchelfHook` writes the rpath the
  # binary needs to find both later: upstream only emits `$ORIGIN` and
  # `$ORIGIN/../lib`, which is where its own package puts them. It resolves every
  # `NEEDED` entry against these, so the C++ runtime the zvec C API and llama.cpp
  # pull in has to be here too — `libstdc++.so.6` and `libgcc_s.so.1` both come
  # from `cc.cc.lib`, which is what nixpkgs' own C++-linking packages list.
  buildInputs = [
    zvecLib
    pkgs.onnxruntime
    pkgs.stdenv.cc.cc.lib
  ];

  env = {
    ZVEC_LIB_DIR = "${zvecLib}/lib";
    # `ort` is built with its `load-dynamic` feature, so nothing is linked against
    # ONNX Runtime and `ort-sys` never needs `ORT_LIB_LOCATION`. What the setting
    # below does is keep `zg-engine`'s build script from staging (and
    # network-downloading) an upstream 1.21 runtime: it takes any `ORT_DYLIB_PATH`
    # as "an external runtime is configured" and returns early. The runtime the
    # binary actually loads is the one this derivation installs under `$out/lib`.
    ORT_DYLIB_PATH = "${pkgs.onnxruntime}/lib/libonnxruntime.so.1";
    # The archive `llama-cpp-sys-2` verifies, extracts and compiles from.
    LLAMA_CPP_SOURCE_ARCHIVE = "${llamaCppSource}";
  };

  # ... and the extraction cache has to be writable: `llama-cpp-sys-2` unpacks the
  # archive under `CARGO_HOME/llama-cpp-sources` unless `LLAMA_CPP_SOURCE_CACHE`
  # says otherwise, and CARGO_HOME is the read-only vendor directory here.
  preBuild = ''
    export LLAMA_CPP_SOURCE_CACHE="$TMPDIR/llama-cpp-sources"
  '';

  # The suite wants network and downloaded models; this package is the binary.
  doCheck = false;

  # nixpkgs' cargoBuildHook always passes `--target <triple>`, so the artifact is
  # under the target subdirectory of `target/`, not `target/release/` — the same
  # subdirectory its own install hook copies from.
  # zg loads ONNX Runtime at run time (`ort`'s `load-dynamic`): the engine looks
  # for `libonnxruntime.so.1` under `../lib` relative to its own executable before
  # anything on the loader path, which is where upstream's own packaging puts it.
  # So the binary needs the runtime beside it, not linked into it.
  installPhase = ''
    runHook preInstall
    install -Dm755 "target/${pkgs.stdenv.targetPlatform.rust.cargoShortTarget}/$cargoBuildType/zg" $out/bin/zg
    mkdir -p $out/lib
    ln -s ${pkgs.onnxruntime}/lib/libonnxruntime.so.1 $out/lib/libonnxruntime.so.1
    runHook postInstall
  '';

  passthru = {
    inherit zvecLib;
    sourceUrl = "https://github.com/${zvecGrep.owner}/${zvecGrep.repo}/tree/${zvecGrep.rev}";
  };

  meta = {
    description = "Agent-friendly workspace search (Rust rewrite of zvec-grep)";
    homepage = "https://github.com/${zvecGrep.owner}/${zvecGrep.repo}";
    license = pkgs.lib.licenses.asl20;
    platforms = [ "x86_64-linux" ];
    mainProgram = "zg";
  };
})
