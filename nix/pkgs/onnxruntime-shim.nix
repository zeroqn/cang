# The ONNX Runtime ABI shim (`libonnxruntime.so.1` / `libonnxrtabi24.so.1`) that
# the vendored `onnxruntime-node` binding and `zg` are both loaded against.
#
# Two consumers need `OrtGetApiBase` from a library named `libonnxruntime.so.1`,
# and they import it under different version nodes: a vendored binding asks for
# `@VERS_1.24.3` (and NEEDs the soname while its own `$ORIGIN` holds no library),
# `zg` asks for `@VERS_1.27.1` (nixpkgs-unstable's, which its `RUNPATH` names).
# No nixpkgs ONNX Runtime serves both - each release exports its own node
# (`@@VERS_1.24.4`, `@@VERS_1.27.1`), and the loader checks the node before ONNX
# Runtime's own append-only `OrtApi` negotiation can happen - so this file defines
# the symbol under both nodes and forwards both to one real library.
#
# The forwarded library is the *newest* one any consumer asks for
# (nixpkgs-unstable's 1.27.1, the same build `zg` links and the image already
# carries): a `@VERS_1.27.1` caller wants the 1.27 `OrtApi`, while a
# `@VERS_1.24.3` caller only wants the 1.24 table, which a 1.27 library still
# serves because ONNX Runtime only appends to that vtable. `ONNXRUNTIME_LIB_PATH`
# overrides the directory it loads from at run time (`ORT_SHIM_REAL_LIB` names an
# exact file, for tests), so another ONNX Runtime can be swapped in without
# rebuilding this file, and `onnxruntimeLibDir` below is both the compiled-in
# fallback and the value the image exports under that variable.
#
# The file is installed under both of its names and its SONAME is the soname the
# consumers' `DT_NEEDED` carries. glibc matches a lookup against the *file* name
# in the search path, so `libonnxrtabi24.so.1` serves a binding patched with
# `patchelf --replace-needed` while `libonnxruntime.so.1` serves the unpatched
# one, and because the shim answers *both* version nodes, `zg` - which runs as a
# child of `pi` and therefore sees the same `LD_LIBRARY_PATH` - keeps working
# instead of failing on the binding's node. `nix/pkgs/onnxruntime-shim.c` carries
# the same reasoning next to the code.
#
# One consequence is worth stating where the wiring lives: because the shim's
# directory precedes every binary's own `RUNPATH` (that is what `LD_LIBRARY_PATH`
# means), *this* file is the ONNX Runtime of everything on that path. A consumer
# importing a node the shim does not define - a vendored binding built against a
# newer ONNX Runtime than 1.27 - will fail to load until its node is added to
# `versionScript` below.
{ pkgs, onnxruntime }:

let
  # Exporting the symbol under a named version node is what a consumer's
  # versioned import is checked against. `OrtGetApiBase` under `VERS_1.24.3` is
  # the default node (what a linker binds), the `@VERS_1.27.1` alias is added by
  # the `.symver` directive in the source, and `local:` keeps the alias's
  # implementation out of the dynamic symbol table.
  versionScript = pkgs.writeText "onnxruntime-abi-shim.map" ''
    VERS_1.24.3 {
      global:
        OrtGetApiBase;
      local:
        ort_get_api_base_v1271;
    };
    VERS_1.27.1 { };
  '';

  shim = pkgs.stdenv.mkDerivation {
    pname = "onnxruntime-abi-shim";
    # The ONNX Runtime ABI the shim's default node presents - what its binding
    # consumer imports - not the release it forwards to.
    version = "1.24.3";

    src = ./onnxruntime-shim.c;

    # One translation unit, compiled below; there is nothing to unpack.
    dontUnpack = true;

    buildPhase = ''
      runHook preBuild
      $CC -shared -fPIC -O2 -Wall -Wextra \
        -DONNXRUNTIME_DEFAULT_LIB_DIR=${onnxruntime}/lib \
        -Wl,--version-script=${versionScript} \
        -Wl,-soname,libonnxruntime.so.1 \
        -o libonnxruntime.so.1 $src
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      install -Dm755 libonnxruntime.so.1 $out/lib/libonnxruntime.so.1
      # The name a binding patched to ask for the shim itself carries, and what
      # `-l` wants so a consumer (or the probes below) can link against the same
      # file the loader will open.
      ln -s libonnxruntime.so.1 $out/lib/libonnxrtabi24.so.1
      ln -s libonnxruntime.so.1 $out/lib/libonnxruntime.so
      ln -s libonnxruntime.so.1 $out/lib/libonnxrtabi24.so
      runHook postInstall
    '';

    passthru = {
      # The directory the shim falls back to, and what the image exports as
      # ONNXRUNTIME_LIB_PATH.
      onnxruntimeLibDir = "${onnxruntime}/lib";
    };

    meta = {
      description = "ONNX Runtime ABI shim for the vendored onnxruntime-node binding and zg";
      homepage = "https://onnxruntime.ai";
      license = pkgs.lib.licenses.mit;
      platforms = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      sourceProvenance = [ pkgs.lib.sourceTypes.fromSource ];
    };
  };

  # The consumers, in the three shapes described in onnxruntime-shim-probe.c.
  # They link the shim by store path but carry no rpath: the shim is found
  # through `LD_LIBRARY_PATH`, which is exactly the wiring the image's pi wrapper
  # provides, so the same binaries are the live guest evidence for it.
  probes = pkgs.stdenv.mkDerivation {
    pname = "onnxruntime-abi-probe";
    inherit (shim) version;

    src = ./onnxruntime-shim-probe.c;

    dontUnpack = true;
    nativeBuildInputs = [ pkgs.patchelf ];

    buildPhase = ''
      runHook preBuild
      build_probe() {
        $CC -O2 -Wall -Wextra \
          -DPROBE_ORT_API_VERSION="$2" \
          ''${3:+-DPROBE_SYMVER="$3"} \
          -I${onnxruntime.dev}/include \
          -L${shim}/lib -l:libonnxruntime.so.1 \
          -o "$1" $src
      }

      # A vendored binding: the soname, the 1.24 node, the 1.24 API table.
      build_probe probe-binding 24

      # `zg`: the same soname, the 1.27 node, the 1.27 API table.
      build_probe probe-zg 27 '".symver OrtGetApiBase,OrtGetApiBase@VERS_1.27.1"'

      # A binding built to ask the shim for itself.
      cp probe-binding probe-patched-name
      patchelf --replace-needed libonnxruntime.so.1 libonnxrtabi24.so.1 probe-patched-name
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      install -Dm755 probe-binding $out/bin/onnxruntime-abi-probe
      install -Dm755 probe-zg $out/bin/onnxruntime-abi-probe-zg
      install -Dm755 probe-patched-name $out/bin/onnxruntime-abi-probe-patched-name
      runHook postInstall
    '';

    doInstallCheck = true;
    installCheckPhase = ''
      runHook preInstallCheck

      export LD_LIBRARY_PATH=${shim}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}

      # Each consumer shape, answered by the node it imports.
      $out/bin/onnxruntime-abi-probe
      $out/bin/onnxruntime-abi-probe-zg
      $out/bin/onnxruntime-abi-probe-patched-name

      # ... and the variable is really consulted, not just carried: a directory
      # without a libonnxruntime.so.1 in it must fail rather than fall back to the
      # compiled-in one.
      if ONNXRUNTIME_LIB_PATH="$TMPDIR" $out/bin/onnxruntime-abi-probe 2>/dev/null; then
        echo "onnxruntime ABI shim: ONNXRUNTIME_LIB_PATH was ignored" >&2
        exit 1
      fi

      # An exact file wins over the directory, which is the test knob.
      ORT_SHIM_REAL_LIB=${onnxruntime}/lib/libonnxruntime.so.1 \
        ONNXRUNTIME_LIB_PATH="$TMPDIR" \
        $out/bin/onnxruntime-abi-probe

      runHook postInstallCheck
    '';

    meta = {
      description = "Loader-contract probes for the ONNX Runtime ABI shim";
      inherit (shim.meta) homepage license platforms;
      sourceProvenance = [ pkgs.lib.sourceTypes.fromSource ];
    };
  };
in
shim.overrideAttrs (old: {
  passthru = (old.passthru or { }) // {
    inherit probes;
  };
})
