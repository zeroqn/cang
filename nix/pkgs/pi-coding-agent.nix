{
  pkgs,
  pins,
  # The toolchain the whole build runs under: `bun install` resolves the
  # dependency tree and `bun run` executes pi's build scripts, with bun's own
  # `bun build --compile` producing the standalone binary. The flake passes
  # nixpkgs-unstable's bun rather than `pkgs.bun`: Magic Context's own build
  # scripts refuse to run below bun 1.4 (`scripts/check-bun.mjs`), and the
  # standalone binary keeps the compiler's dynamic linkage - so this is also
  # what decides which glibc `pi` runs on, and with it whether the ONNX Runtime
  # the ABI shim loads (an unstable build) can be opened in pi's process.
  bun,
  # The ONNX Runtime ABI shim (`libonnxruntime.so.1` / `libonnxrtabi24.so.1`)
  # this package's wrapper exposes, described in nix/pkgs/onnxruntime-shim.nix.
  onnxruntimeShim,
  nativeAddonCxxRuntime,
}:

let
  version = pins.piCodingAgent.version;

  piAiNpmTarball = pkgs.fetchurl {
    url = "https://registry.npmjs.org/@earendil-works/pi-ai/-/pi-ai-${version}.tgz";
    hash = pins.piCodingAgent.aiNpmTarballHash;
  };

  src = pkgs.fetchFromGitHub {
    owner = pins.piCodingAgent.owner;
    repo = pins.piCodingAgent.repo;
    rev = pins.piCodingAgent.rev;
    hash = pins.piCodingAgent.srcHash;
  };

  # What the build sees of the checkout, applied identically to the dependency
  # derivation below and to the build itself:
  #
  # - the generated root manifest narrows the monorepo's `packages/*` workspace
  #   glob to the closure `build:binary` builds (plus `schema`, which `ai`
  #   type-imports), so the install does not drag in the examples, evals and
  #   apps the glob would otherwise collect;
  # - `packages/ai`'s `build` script regenerates its model catalog from the
  #   network, while `build:offline` consumes `src/providers/data`: the
  #   published `pi-ai` tarball's data is unpacked there, which is the same
  #   bytes its own release shipped;
  # - `bun.lock` is the lock `bun install` resolves from (see `bunDeps`).
  postPatch = ''
    cp ${./pi-coding-agent-package.json} package.json
    cp ${./pi-coding-agent-bun.lock} bun.lock
    substituteInPlace packages/coding-agent/package.json \
      --replace-fail 'npm --prefix ../ai run build' \
                     'npm --prefix ../ai run build:offline'

    mkdir -p packages/ai/src/providers/data
    tar -xzf ${piAiNpmTarball} --strip-components=4 \
      -C packages/ai/src/providers/data \
      package/dist/providers/data
  '';

  # The dependency tree, as a fixed-output derivation: `bun install` is the only
  # step that needs the registry, and its result is what `bunDepsHash` in
  # nix/pins.nix pins. The frozen lock makes the resolution strict - a
  # `package.json` that no longer matches `bun.lock` fails here rather than
  # silently resolving something else - and the tree is copied into the build
  # so the workspace symlinks inside it (`node_modules/@earendil-works/pi-ai` ->
  # `../../packages/ai`) resolve against the checkout being built.
  bunDeps = pkgs.stdenvNoCC.mkDerivation {
    pname = "pi-coding-agent-bun-deps";
    inherit version src postPatch;

    nativeBuildInputs = [ bun ];

    dontConfigure = true;
    # Nothing in a dependency tree should be stripped or patched: the JS is
    # copied verbatim and the prebuilt native pieces are not ours to rewrite.
    dontFixup = true;

    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = pins.piCodingAgent.bunDepsHash;

    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      export BUN_INSTALL_CACHE_DIR=$TMPDIR/bun-cache
      bun install --frozen-lockfile --ignore-scripts
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp -a node_modules/. $out/
      runHook postInstall
    '';
  };

  # pi's build scripts are npm scripts (`npm run build`, `npm --prefix ../tui run
  # build`) whose helpers are invoked as `node ../../scripts/*.mjs`, and this
  # package deliberately has no node in its build inputs. bun runs all three
  # forms: a script with bun's own runner, the npm `run` subset these scripts
  # use with `--cwd` standing in for `--prefix`, and the `.mjs`/`.ts` helpers
  # directly. The failure mode is loud - anything outside that subset exits
  # non-zero with the line it did not implement.
  nodeAsBun = pkgs.writeShellScriptBin "node" ''
    exec ${bun}/bin/bun "$@"
  '';
  npmAsBun = pkgs.writeShellScriptBin "npm" ''
    set -eu
    prefix=""
    script=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --prefix)
          prefix="$2"
          shift 2
          ;;
        run)
          script="$2"
          shift 2
          break
          ;;
        *)
          echo "pi-coding-agent: this npm shim only implements 'npm [--prefix <dir>] run <script>', not '$1'" >&2
          exit 1
          ;;
      esac
    done
    if [ -z "$script" ]; then
      echo "pi-coding-agent: this npm shim needs a script to run" >&2
      exit 1
    fi
    if [ -n "$prefix" ]; then
      exec ${bun}/bin/bun run --cwd "$prefix" "$script" "$@"
    fi
    exec ${bun}/bin/bun run "$script" "$@"
  '';

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
  #
  # Being searched ahead of every binary's own run path also makes the one file
  # here a *ceiling* for the whole process tree: an injected libstdc++ older than
  # what some consumer in the image was built against leaves that consumer
  # unstartable, because a `GLIBCXX_3.4.x` version need cannot be satisfied by a
  # file that does not define it (`zg`'s ONNX Runtime is such a consumer). So
  # `nativeAddonCxxRuntime` has to be the newest C++ runtime the image's
  # consumers need - the flake fills it from nixpkgs-unstable's gcc, while `pkgs`
  # here is the 26.05 input whose gcc stops a release earlier. libstdc++ keeps
  # its ABI backward compatible, so the newer file still serves an addon built
  # against an older one.
  # The directory carries exactly the three sonames those consumers lack, and
  # nothing else: `libstdc++.so.6` because the addons NEED it, and the ONNX
  # Runtime ABI shim under both of its names - `libonnxruntime.so.1` for a
  # binding whose `DT_NEEDED` carries that soname, `libonnxrtabi24.so.1` for one
  # built to ask the shim for itself. The shim answers `@VERS_1.24.3` (the
  # binding) and `@VERS_1.27.1` (`zg`, which inherits this search path) from one
  # file; see nix/pkgs/onnxruntime-shim.nix.
  nativeAddonRuntimeDir = pkgs.runCommand "cang-native-addon-runtime" { } ''
    mkdir -p "$out/lib"
    ln -s ${nativeAddonCxxRuntime}/lib/libstdc++.so.6 "$out/lib/libstdc++.so.6"
    ln -s ${onnxruntimeShim}/lib/libonnxruntime.so.1 "$out/lib/libonnxruntime.so.1"
    ln -s ${onnxruntimeShim}/lib/libonnxrtabi24.so.1 "$out/lib/libonnxrtabi24.so.1"
  '';
in
pkgs.stdenv.mkDerivation {
  pname = "pi-coding-agent";
  inherit version src postPatch;

  nativeBuildInputs = [
    bun
    nodeAsBun
    npmAsBun
    pkgs.makeWrapper
  ];

  buildPhase = ''
    runHook preBuild

    cp -a ${bunDeps} ./node_modules
    # npm-installed entry points carry `#!/usr/bin/env node`, which a Nix build
    # has no `/usr/bin/env` for: point them at the `node` on PATH, which is bun.
    patchShebangs --build node_modules
    export HOME=$TMPDIR
    export BUN_INSTALL_CACHE_DIR=$TMPDIR/bun-cache

    # `packages/ai` imports `@earendil-works/pi-schema` (a workspace member
    # since v1.5.0) and `build:binary` does not build it, while upstream's own
    # build list runs `schema` before `ai`; without this `tsc` cannot resolve
    # the module's declarations.
    bun run --cwd packages/schema build
    bun run --cwd packages/coding-agent build:binary

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out/lib/pi-coding-agent $out/bin
    cp -R packages/coding-agent/dist/. $out/lib/pi-coding-agent/
    chmod +x $out/lib/pi-coding-agent/pi

    # `bin/pi` is a wrapper so that native addons loaded by extensions can find
    # the C++ runtime and the ONNX Runtime shim they need;
    # `lib/pi-coding-agent/pi` stays the raw binary. `ONNXRUNTIME_LIB_PATH` is
    # the directory the shim loads the real library from, and a *default*: the
    # image exports it too, and an exported value has to win.
    makeWrapper $out/lib/pi-coding-agent/pi $out/bin/pi \
      --prefix LD_LIBRARY_PATH : ${nativeAddonRuntimeDir}/lib \
      --set-default ONNXRUNTIME_LIB_PATH ${onnxruntimeShim.onnxruntimeLibDir}

    install -Dm644 packages/coding-agent/README.md $out/share/doc/pi-coding-agent/README.md
    install -Dm644 packages/coding-agent/CHANGELOG.md $out/share/doc/pi-coding-agent/CHANGELOG.md
    cp -R packages/coding-agent/docs $out/share/doc/pi-coding-agent/docs

    runHook postInstall
  '';

  # The bun standalone is already stripped and is not ours to rewrite.
  dontStrip = true;

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
