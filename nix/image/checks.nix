{
  pkgs,
  piCodingAgent,
  rioBin,
  herdrPrebuilt,
  montyPrebuilt,
  rmuxPrebuilt,
  rtkPrebuilt,
  zvecGrep,
  doltPrebuilt,
  beadsPrebuilt,
  freshPrebuilt,
  containerLibPolicySeccompJson,
  libkrunfw,
  wl-cross-domain-proxy,
  bun,
  cangMuslPackage,
}:

let
  configPayloads = import ./config-payloads.nix { inherit pkgs; };
  layers = import ./layers.nix {
    inherit
      pkgs
      piCodingAgent
      rioBin
      herdrPrebuilt
      montyPrebuilt
      rmuxPrebuilt
      rtkPrebuilt
      zvecGrep
      doltPrebuilt
      beadsPrebuilt
      freshPrebuilt
      containerLibPolicySeccompJson
      libkrunfw
      wl-cross-domain-proxy
      bun
      cangMuslPackage
      ;
    fishConfig = configPayloads.fishConfig;
    starshipConfig = configPayloads.starshipConfig;
  };
  imageConfig = import ./config.nix {
    inherit
      pkgs
      cangMuslPackage
      configPayloads
      layers
      ;
  };

  storePathPattern = "[0-9a-df-np-sv-z]{32}-[^/\":, ]+";
  storeRefDelimiters = [
    "/"
    "\""
    ":"
    ","
    " "
    "\\"
    "\n"
    "\t"
  ];
  takeStoreRefToken =
    segment:
    let
      takeStoreRefChars =
        chars:
        if chars == [ ] then
          [ ]
        else
          let
            head = builtins.head chars;
            tail = builtins.tail chars;
          in
          if builtins.elem head storeRefDelimiters then [ ] else [ head ] ++ takeStoreRefChars tail;
    in
    pkgs.lib.concatStrings (takeStoreRefChars (pkgs.lib.stringToCharacters segment));
  sortStoreRefs = refs: pkgs.lib.unique (pkgs.lib.sort (left: right: left < right) refs);
  storeRefsIn =
    text:
    let
      segments = builtins.tail (pkgs.lib.splitString "/nix/store/" text);
      tokens = map takeStoreRefToken segments;
      refs = map (token: "/nix/store/${token}") (
        builtins.filter (token: builtins.match storePathPattern token != null) tokens
      );
    in
    sortStoreRefs refs;

  imageConfigText = builtins.unsafeDiscardStringContext (builtins.toJSON imageConfig);
  imageConfigRefs = storeRefsIn imageConfigText;
  imageNixDbClosureInfo = pkgs.closureInfo {
    rootPaths = layers.imageContents;
  };
  imageNixDbStorePathsText = builtins.readFile "${imageNixDbClosureInfo}/store-paths";
  imageNixDbStorePaths = storeRefsIn imageNixDbStorePathsText;
  missingImageConfigNixDbRefs = builtins.filter (
    ref: !(builtins.elem ref imageNixDbStorePaths)
  ) imageConfigRefs;

  refsText = refs: builtins.concatStringsSep "\n" refs;
  indentedRefsText = refs: builtins.concatStringsSep "\n" (map (ref: "  ${ref}") refs);
  missingRefsMessage = ''
    cang image config references store paths outside the generated image Nix DB metadata.
    These paths can be pulled in by Docker config/env references without being registered in /nix/var/nix/db.

    Missing from pkgs.closureInfo { rootPaths = layers.imageContents; }:
    ${indentedRefsText missingImageConfigNixDbRefs}

    This check uses the same root path closure that dockerTools.includeNixDB loads into the image DB.
    It does not inspect, repair, or mutate the host Nix DB.
  '';

  imageConfigFile = pkgs.writeText "cang-image-config.json" imageConfigText;
  imageConfigRefsFile = pkgs.writeText "cang-image-config-refs.txt" (refsText imageConfigRefs);
  imageNixDbStorePathsFile = pkgs.writeText "cang-image-nix-db-store-paths.txt" (
    builtins.unsafeDiscardStringContext imageNixDbStorePathsText
  );
  missingRefsFile = pkgs.writeText "cang-image-config-missing-refs.txt" (
    builtins.unsafeDiscardStringContext (refsText missingImageConfigNixDbRefs)
  );
  missingRefsMessageFile = pkgs.writeText "cang-image-config-missing-refs-message.txt" (
    builtins.unsafeDiscardStringContext missingRefsMessage
  );
  containerSourceFile = pkgs.writeText "cang-container-nix-source.txt" (
    builtins.readFile ./container.nix
  );
  configSourceFile = pkgs.writeText "cang-config-nix-source.txt" (builtins.readFile ./config.nix);
  layersSourceFile = pkgs.writeText "cang-layers-nix-source.txt" (builtins.readFile ./layers.nix);
  openglSourceFile = pkgs.writeText "cang-guest-init-opengl-source.txt" (
    builtins.readFile ../../crates/cang-guest-init/src/guest_init/components/opengl.rs
  );
  piSourceFile = pkgs.writeText "cang-pi-coding-agent-nix-source.txt" (
    builtins.readFile ../pkgs/pi-coding-agent.nix
  );

  # The directory the pi wrapper exposes to prebuilt extension addons, and the
  # wrapper's own path into it; see ADR 0009 and the map under
  # docs/wayfinder/pi-native-addons/.
  nativeAddonRuntimeDir = piCodingAgent.passthru.nativeAddonRuntimeDir;
  allocatorContracts = ''
    grep -F 'mimallocLib = ' ${layersSourceFile}
    grep -F 'pkgs.mimalloc' ${layersSourceFile}
    grep -F './etc/ld-nix.so.preload' ${containerSourceFile}
    grep -F 'cat > ./etc/nix-allocator-libs <<EOF_NIX_ALLOCATOR_LIBS' ${containerSourceFile}
    grep -F 'mimalloc=' ${containerSourceFile}
    grep -F 'hardened=' ${containerSourceFile}
    grep -F 'CANG_MIMALLOC_LIB=' ${configSourceFile}
    ! grep -F 'LD_PRELOAD=' ${containerSourceFile}
  '';
  terminalMultiplexerContracts = ''
    grep -F 'rmuxPrebuilt' ${layersSourceFile}
    grep -F 'pkgs.tmux' ${layersSourceFile}
    grep -F 'rmuxPrebuilt' ${containerSourceFile}
    grep -F './etc/rmux.conf' ${containerSourceFile}
    grep -F 'set -g mouse off' ${containerSourceFile}
    grep -F "bind T if-shell -F '#{mouse}' 'set -g mouse off ; display-message \"mouse OFF: native terminal selection enabled\"' 'set -g mouse on ; display-message \"mouse ON: pane mouse mode enabled\"'" ${containerSourceFile}
    grep -F 'set -g history-limit 100000' ${containerSourceFile}
    grep -F 'set -g renumber-windows on' ${containerSourceFile}
    grep -F 'set -g base-index 1' ${containerSourceFile}
    grep -F 'setw -g pane-base-index 1' ${containerSourceFile}
    grep -F 'setw -g mode-keys vi' ${containerSourceFile}
    grep -F 'set -g status-keys vi' ${containerSourceFile}
    grep -F 'bind | split-window -h -c "#{pane_current_path}"' ${containerSourceFile}
    grep -F 'bind - split-window -v -c "#{pane_current_path}"' ${containerSourceFile}
    grep -F 'bind c new-window -c "#{pane_current_path}"' ${containerSourceFile}
    test "$(grep -Fc 'bind h select-pane -L' ${containerSourceFile})" -eq 1
    test "$(grep -Fc 'bind j select-pane -D' ${containerSourceFile})" -eq 1
    test "$(grep -Fc 'bind k select-pane -U' ${containerSourceFile})" -eq 1
    test "$(grep -Fc 'bind l select-pane -R' ${containerSourceFile})" -eq 1
    ! grep -F 'rmuxTmuxCommandCompat' ${layersSourceFile}
    ! grep -F 'rmux-tmux-command-compat' ${layersSourceFile}
    ! grep -F './etc/tmux.conf' ${containerSourceFile}
    test -x ${rmuxPrebuilt}/bin/rmux
    test -x ${pkgs.tmux}/bin/tmux
    ${rmuxPrebuilt}/bin/rmux -V
    ${pkgs.tmux}/bin/tmux -V
  '';
  ghosttyTerminfoContracts = ''
    ! grep -F 'pkgs.ghostty' ${layersSourceFile}
    grep -F './home/dev/.terminfo/x' ${containerSourceFile}
    grep -F 'pkgs.ghostty.terminfo' ${containerSourceFile}
    grep -F 'xterm-ghostty' ${containerSourceFile}
    test -f ${pkgs.ghostty.terminfo}/share/terminfo/x/xterm-ghostty
    ${pkgs.lib.optionalString (rioBin != null) ''
      test -x ${rioBin}/bin/rio
      test -f ${rioBin}/share/terminfo/r/rio
      test -f ${rioBin}/share/terminfo/x/xterm-rio
    ''}
  '';

  browserContracts = ''
    grep -F 'browserImageLayer' ${layersSourceFile}
    grep -F 'pkgs.ungoogled-chromium' ${layersSourceFile}
    test -x ${pkgs.ungoogled-chromium}/bin/chromium
  '';

  zvecGrepContracts = ''
    grep -F 'zvecGrep' ${layersSourceFile}
    test -x ${layers.agentImageLayer}/bin/zg
    ${layers.agentImageLayer}/bin/zg --help >/dev/null
    HOME="$TMPDIR" ${pkgs.nodejs}/bin/node ${./zvec-grep-native-load-check.mjs} ${layers.agentImageLayer}/lib/zvec-grep
    case ":${layers.imagePath}:" in
      *":${layers.agentImageLayer}/bin:"*) ;;
      *) exit 1 ;;
    esac
  '';

  herdrContracts = ''
    grep -F 'herdrPrebuilt' ${layersSourceFile}
    ${pkgs.lib.optionalString (herdrPrebuilt != null) ''
      test -x ${layers.agentImageLayer}/bin/herdr
      ${herdrPrebuilt}/bin/herdr --version >/dev/null
    ''}
  '';

  doltContracts = ''
    grep -F 'doltPrebuilt' ${layersSourceFile}
    test -x ${layers.agentImageLayer}/bin/dolt
    HOME="$TMPDIR" ${layers.agentImageLayer}/bin/dolt version | grep -F 'dolt version ${doltPrebuilt.version}'
    case ":${layers.imagePath}:" in
      *":${layers.agentImageLayer}/bin:"*) ;;
      *) exit 1 ;;
    esac
  '';

  beadsContracts = ''
    grep -F 'beadsPrebuilt' ${layersSourceFile}
    test -x ${layers.agentImageLayer}/bin/bd
    HOME="$TMPDIR" ${layers.agentImageLayer}/bin/bd version | grep -F 'bd version ${beadsPrebuilt.version}'
    case ":${layers.imagePath}:" in
      *":${layers.agentImageLayer}/bin:"*) ;;
      *) exit 1 ;;
    esac
  '';
  freshContracts = ''
    grep -F 'freshPrebuilt' ${layersSourceFile}
    test -x ${layers.agentImageLayer}/bin/fresh
    HOME="$TMPDIR" ${layers.agentImageLayer}/bin/fresh --version | grep -F 'fresh ${freshPrebuilt.version}'
    case ":${layers.imagePath}:" in
      *":${layers.agentImageLayer}/bin:"*) ;;
      *) exit 1 ;;
    esac
  '';
  sqliteContracts = ''
    grep -F 'pkgs.sqlite' ${layersSourceFile}
    test -x ${layers.agentImageLayer}/bin/sqlite3
    HOME="$TMPDIR" ${layers.agentImageLayer}/bin/sqlite3 --version
  '';
  montyContracts = ''
    grep -F 'montyPrebuilt' ${layersSourceFile}
    grep -F 'MONTY_BIN=' ${configSourceFile}
    ${pkgs.lib.optionalString (montyPrebuilt != null) ''
      test -x ${layers.agentImageLayer}/bin/monty
      HOME="$TMPDIR" ${layers.agentImageLayer}/bin/monty --version | grep -F 'monty-runtime ${montyPrebuilt.releaseVersion}'
      case ":${layers.imagePath}:" in
        *":${layers.agentImageLayer}/bin:"*) ;;
        *) exit 1 ;;
      esac
      grep -F 'MONTY_BIN=${layers.montyPackage}/bin/monty' ${imageConfigFile}
    ''}
  '';

  # Prebuilt addons that pi extensions install at runtime (magic-context's
  # `sharp` and `onnxruntime-node`) are glibc ELF objects with
  # `DT_NEEDED libstdc++.so.6`, while the pi binary is a bun standalone artifact
  # with no RUNPATH and no libstdc++ in its own DT_NEEDED - and the guest has no
  # default library path that supplies it. The wrapper is what carries the
  # soname into the process, so assert both halves of that wiring and that the
  # wrapper really hands the variable to its child.
  nativeAddonRuntimeContracts = ''
    grep -F 'nativeAddonRuntimeDir' ${piSourceFile}
    grep -F 'makeWrapper $out/lib/pi-coding-agent/pi $out/bin/pi' ${piSourceFile}
    test -e ${nativeAddonRuntimeDir}/lib/libstdc++.so.6
    test -x ${piCodingAgent}/lib/pi-coding-agent/pi
    # The wrapper, not a symlink to the raw bun binary, is what the image runs.
    test ! -L ${piCodingAgent}/bin/pi
    grep -F '${nativeAddonRuntimeDir}/lib' ${piCodingAgent}/bin/pi
    test -x ${layers.agentImageLayer}/bin/pi
    grep -F '${nativeAddonRuntimeDir}/lib' ${layers.agentImageLayer}/bin/pi
    ${piCodingAgent}/bin/pi --version | grep -F '${piCodingAgent.version}'
    # Behavioural: mentioning the directory is not enough - a child process
    # started through the wrapper must actually see it in LD_LIBRARY_PATH.
    sed -e 's|^exec .*|exec env|' ${piCodingAgent}/bin/pi > native-addon-wrapper-env
    if cmp -s native-addon-wrapper-env ${piCodingAgent}/bin/pi; then
      echo "native addon runtime: could not rewrite the pi wrapper's exec line" >&2
      exit 1
    fi
    chmod +x native-addon-wrapper-env
    ./native-addon-wrapper-env | grep -F "LD_LIBRARY_PATH=${nativeAddonRuntimeDir}/lib"
  '';

  rootCargoAbsent = pkgs.runCommand "cang-image-root-cargo-absent-check" { } ''
    set -euo pipefail

    test ! -e ${layers.rustSourceImage}/.cargo

    touch "$out"
  '';

  omxAbsent =
    pkgs.runCommand "cang-image-omx-absent-check"
      {
        nativeBuildInputs = [ pkgs.gnugrep ];
      }
      ''
        set -euo pipefail

        test ! -e ${layers.agentImageLayer}/bin/omx
        test ! -e ${layers.agentImageLayer}/bin/omx-api
        test ! -e ${layers.agentImageLayer}/bin/omx-runtime
        test ! -e ${layers.agentImageLayer}/bin/omx-sparkshell

        ! grep -F 'OMX_API_BIN=' ${imageConfigFile}
        ! grep -F 'OMX_RUNTIME_BINARY=' ${imageConfigFile}
        ! grep -F 'OMX_SPARKSHELL_BIN=' ${imageConfigFile}
        ! grep -F 'oh-my-codex' ${imageConfigFile}
        ! grep -F 'oh-my-codex' ${imageNixDbStorePathsFile}

        mkdir -p "$out"
        touch "$out/passed"
      '';

  codexAbsent = pkgs.runCommand "cang-image-codex-absent-check" { } ''
    set -euo pipefail

    test ! -e ${layers.agentImageLayer}/bin/codex
    test ! -e ${layers.agentImageLayer}/bin/codex-code-mode-host

    mkdir -p "$out"
    touch "$out/passed"
  '';

  ompAbsent = pkgs.runCommand "cang-image-omp-absent-check" { } ''
    set -euo pipefail

    test ! -e ${layers.agentImageLayer}/bin/omp

    mkdir -p "$out"
    touch "$out/passed"
  '';

  dirgeAbsent = pkgs.runCommand "cang-image-dirge-absent-check" { } ''
    set -euo pipefail

    test ! -e ${layers.agentImageLayer}/bin/dirge
    test ! -e ${layers.agentImageLayer}/bin/dirge-microvm-runner

    mkdir -p "$out"
    touch "$out/passed"
  '';

  # `gh` ships in the shared tooling layer, so its absence is asserted against
  # the realized image PATH rather than one layer's bin directory.
  ghAbsent = pkgs.runCommand "cang-image-gh-absent-check" { } ''
    set -euo pipefail

    for binDir in $(printf '%s' "${layers.imagePath}" | tr ':' '\n'); do
      test ! -e "$binDir/gh"
    done

    mkdir -p "$out"
    touch "$out/passed"
  '';

  wrapperContracts =
    pkgs.runCommand "cang-image-wrapper-contracts-check"
      {
        nativeBuildInputs = [ pkgs.gnugrep ];
      }
      ''
        set -euo pipefail

        ${allocatorContracts}
        ${terminalMultiplexerContracts}
        ${ghosttyTerminfoContracts}
        ${zvecGrepContracts}
        ${herdrContracts}
        ${doltContracts}
        ${beadsContracts}
        ${freshContracts}
        ${sqliteContracts}
        ${montyContracts}
        ${nativeAddonRuntimeContracts}

        grep -F 'CANG_NIX_OVERLAY' ${layers.nixCommandCompat}/bin/nix
        grep -F 'cang-guest-init internal nix wait' ${layers.nixCommandCompat}/bin/nix
        grep -F 'CANG_CONTAINERS_STORAGE' ${layers.podmanCommandCompat}/bin/podman
        grep -F 'cang-guest-init internal podman wait' ${layers.podmanCommandCompat}/bin/podman
        grep -F 'cang-guest-init internal podman service-wait' ${layers.dockerCommandCompat}/bin/docker
        grep -F 'cang-guest-init internal podman service-wait' ${layers.dockerComposeCommandCompat}/bin/docker-compose
        grep -F 'cang-nix-store-db-check' ${layers.nixStoreDbCheck}/bin/cang-nix-store-db-check
        grep -F '/run/cang/nix-disk/upper' ${layers.nixStoreDbCheck}/bin/cang-nix-store-db-check
        test -x ${pkgs.perf}/bin/perf
        test -x ${pkgs.strace}/bin/strace
        test -f ${pkgs.mesa}/lib/dri/swrast_dri.so
        test -f ${pkgs.mesa}/lib/dri/virtio_gpu_dri.so
        test -f ${pkgs.mesa}/lib/libvulkan_lvp.so
        test -f ${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json
        test -f ${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json
        test -f ${pkgs.mesa}/share/vulkan/icd.d/virtio_icd.x86_64.json
        grep -F 'pkgs.mesa' ${layersSourceFile}
        grep -F './usr/lib/cang-mesa-runtime' ${containerSourceFile}
        grep -F 'ln -s ${"$"}{pkgs.mesa} ./usr/lib/cang-mesa-runtime' ${containerSourceFile}
        # Native EGL needs glvnd's dispatcher (`libEGL.so.1`) on a loader path:
        # mesa's output only has the vendor library the dispatcher loads, so the
        # image exposes libglvnd at a stable path and guest-init exports
        # LD_LIBRARY_PATH for it.
        test -f ${pkgs.libglvnd}/lib/libEGL.so.1
        grep -F './usr/lib/cang-gpu-runtime' ${containerSourceFile}
        grep -F 'ln -s ${"$"}{pkgs.libglvnd} ./usr/lib/cang-gpu-runtime' ${containerSourceFile}
        # The guest's VA-API driver has to be the patched libgallium, not the
        # prebuilt mesa's: libva opens the drv_video.so from LIBVA_DRIVERS_PATH.
        test -f ${layers.vaApiRuntime}/dri/virtio_gpu_drv_video.so
        test -f ${layers.vaApiRuntime}/lib/libgallium.so
        grep -F 'pkgs.mesaVaApi' ${layersSourceFile}
        # guest-init builds the NixOS-conventional `/run/opengl-driver` symlink
        # farm out of these published runtime dirs (component `opengl.rs`).
        # `lib/dri/virtio_gpu_drv_video.so` has to be the PATCHED driver: libva
        # searches that directory by default, and the mesa runtime also ships a
        # `virtio_gpu_drv_video.so`, so pointing the farm at mesa would silently
        # reintroduce ticket 08's constant-QP encode stall. Assert the two
        # drivers really differ, that the patched one is the VA runtime's
        # libgallium, and that the builder links the VA runtime while excluding
        # mesa's copy.
        test -f ${pkgs.mesa}/lib/dri/virtio_gpu_drv_video.so
        patched_va="$(readlink -f ${layers.vaApiRuntime}/dri/virtio_gpu_drv_video.so)"
        mesa_va="$(readlink -f ${pkgs.mesa}/lib/dri/virtio_gpu_drv_video.so)"
        test "$patched_va" = "${pkgs.mesaVaApi}/lib/libgallium-${pkgs.mesaVaApi.version}.so"
        test "$patched_va" != "$mesa_va"
        grep -F '"/run/opengl-driver"' ${openglSourceFile}
        grep -F '"/usr/lib/cang-gpu-runtime/lib"' ${openglSourceFile}
        grep -F '"/usr/lib/cang-mesa-runtime/lib"' ${openglSourceFile}
        grep -F '"/usr/lib/cang-mesa-runtime/share"' ${openglSourceFile}
        grep -F '"/usr/lib/cang-va-runtime/dri"' ${openglSourceFile}
        grep -F '"virtio_gpu_drv_video.so"' ${openglSourceFile}
        grep -F 'name != VA_DRIVER_FILE' ${openglSourceFile}
        grep -F './usr/lib/cang-va-runtime' ${containerSourceFile}
        grep -F 'ln -s ${"$"}{layers.vaApiRuntime} ./usr/lib/cang-va-runtime' ${containerSourceFile}
        grep -F './usr/lib/cang-software-renderer' ${containerSourceFile}
        grep -F 'ln -s ${"$"}{pkgs.mesa} ./usr/lib/cang-software-renderer' ${containerSourceFile}
        grep -F 'pkgs.fontconfig' ${layersSourceFile}
        grep -F './usr/lib/cang-fontconfig' ${containerSourceFile}
        grep -F 'ln -s ${"$"}{pkgs.fontconfig.out} ./usr/lib/cang-fontconfig' ${containerSourceFile}
        test -f ${pkgs.fontconfig.out}/etc/fonts/fonts.conf
        ${pkgs.lib.optionalString (rioBin != null) ''
          test -x ${rioBin}/bin/rio
          grep -F './home/dev/.terminfo/r' ${containerSourceFile}
          grep -F '${"$"}{rioBin}/share/terminfo/r/rio' ${containerSourceFile}
          grep -F '${"$"}{rioBin}/share/terminfo/x/xterm-rio' ${containerSourceFile}
          case ":${layers.imagePath}:" in
            *":${rioBin}/bin:"*) ;;
            *) exit 1 ;;
          esac
        ''}
        test -x ${pkgs.waypipe}/bin/waypipe
        ${browserContracts}
        case ":${layers.imagePath}:" in
          *":${layers.browserImageLayer}/bin:"*) ;;
          *) exit 1 ;;
        esac
        case ":${layers.imagePath}:" in
          *":${pkgs.perf}/bin:"*) ;;
          *) exit 1 ;;
        esac
        case ":${layers.imagePath}:" in
          *":${pkgs.strace}/bin:"*) ;;
          *) exit 1 ;;
        esac
        case ":${layers.imagePath}:" in
          *":${pkgs.waypipe}/bin:"*) ;;
          *) exit 1 ;;
        esac

        mkdir -p "$out"
        touch "$out/passed"
      '';
in
{
  inherit
    imageConfigRefs
    imageNixDbClosureInfo
    imageNixDbStorePaths
    missingImageConfigNixDbRefs
    missingRefsMessage
    codexAbsent
    omxAbsent
    ompAbsent
    dirgeAbsent
    ghAbsent
    rootCargoAbsent
    wrapperContracts
    ;

  imageConfigNixDbRefs =
    pkgs.runCommand "cang-image-config-nix-db-refs-check"
      {
        nativeBuildInputs = [
          pkgs.coreutils
        ];
      }
      ''
        set -euo pipefail

        ${allocatorContracts}
        ${terminalMultiplexerContracts}
        ${ghosttyTerminfoContracts}

        cp ${imageConfigRefsFile} image-config-refs
        cp ${imageNixDbStorePathsFile} image-nix-db-valid-paths
        cp ${missingRefsFile} missing-refs

        if [ -s missing-refs ]; then
          cat ${missingRefsMessageFile} >&2
          exit 1
        fi

        mkdir -p "$out"
        cp image-config-refs "$out/image-config-refs"
        cp image-nix-db-valid-paths "$out/image-nix-db-valid-paths"
        touch "$out/passed"
      '';
}
