{
  pkgs,
  rustToolchain,
  cargoDeny,
  wild,
  piCodingAgent,
  herdrPrebuilt,
  montyPrebuilt,
  rmuxPrebuilt,
  rtkPrebuilt,
  zvecGrep,
  zgModels,
  waypipe,
  doltPrebuilt,
  beadsPrebuilt,
  freshPrebuilt,
  containerLibPolicySeccompJson,
  libkrunfw,
  wl-cross-domain-proxy,
  bun,
  cangMuslPackage,
  mimalloc,
  fishConfig,
  starshipConfig,
}:
let
  nixBuilderGroupId = 30000;
  nixBuilderCount = 32;
  nixBuilderUsers = builtins.genList (
    index:
    let
      builderNumber = index + 1;
    in
    {
      name = "nixbld${toString builderNumber}";
      inherit builderNumber;
      uid = nixBuilderGroupId + builderNumber;
    }
  ) nixBuilderCount;
  nixBuilderGroupMembers = pkgs.lib.concatMapStringsSep "," (builder: builder.name) nixBuilderUsers;
  nixBuilderPasswdEntries = pkgs.lib.concatMapStringsSep "\n" (
    builder:
    "${builder.name}:x:${toString builder.uid}:${toString nixBuilderGroupId}:Nix build user ${toString builder.builderNumber}:/var/empty:${pkgs.runtimeShell}"
  ) nixBuilderUsers;
  # The guest's Rust linker, shared with the dev shell so a link in a cang
  # guest and a link in `nix develop` use the same driver and linker.
  clangWildWrapper = import ../lib/clang-wild-wrapper.nix { inherit pkgs wild; };
  grapheneHardenedMalloc = pkgs.graphene-hardened-malloc.overrideAttrs (_old: {
    version = "14";
    src = pkgs.fetchFromGitHub {
      owner = "GrapheneOS";
      repo = "hardened_malloc";
      tag = "14";
      hash = "sha256-QUGDJyTnD5MuBUMlc4PZOZSAfevVUB6QbncVyXIAgb8=";
    };
  });
  emptyLdNixSoPreload = pkgs.writeText "cang-empty-ld-nix-so-preload" "";
  # The default allocator cang preloads into every nix-linked guest process
  # (`nix/lib/mimalloc-cang.nix`: zeroqn/headless's prebuilt asset). It is also
  # the file the launcher preloads into its venus render server, so both sides of
  # the VM boundary run the same allocator.
  mimallocLib = "${mimalloc}/lib/libmimalloc.so";
  hardenedMallocLib = "${grapheneHardenedMalloc}/lib/libhardened_malloc.so";
  hardeningRun = pkgs.writeShellScriptBin "hardening-run" ''
    if [ "$#" -eq 0 ]; then
      echo "usage: hardening-run COMMAND [ARG ...]" >&2
      exit 64
    fi

    allocator_lib=${hardenedMallocLib}
    case ":''${LD_PRELOAD-}:" in
      *":$allocator_lib:"*) ;;
      "::") export LD_PRELOAD="$allocator_lib" ;;
      *) export LD_PRELOAD="$allocator_lib:$LD_PRELOAD" ;;
    esac

    exec "$@"
  '';
  # The Rust tools keep the pinned toolchain's binary on `PATH`, and mask the
  # allocator preload only when the preload is not the one they were verified
  # against.
  #
  # The mask was written for `hardened_malloc` (commit a7dc9344: rustc's
  # allocation behaviour under it). The image's default preload is mimalloc
  # (`nix/lib/mimalloc-cang.nix`), and rustc 1.100.0 was measured in a cang
  # guest compiling a synthetic -O workload, running `cargo build`, running
  # `cargo test` and producing byte-identical output under it - so the default
  # needs no mask. Every other value, including one this does not recognise,
  # keeps the mask: the crash the mask prevents is the failure it must not
  # reintroduce, so the fast path is an allowlist.
  #
  # It also matters beyond the allocator: the mask is a bubblewrap namespace,
  # and a plain container cannot create one (`bwrap: Creating new namespace
  # failed: Operation not permitted`), which is why the image could not compile
  # inside `podman run`. Gating on the preload is what lets it, while
  # `--alloc=hardened` keeps the masking it needs.
  rustToolCommandCompat =
    name: binary:
    pkgs.writeShellScriptBin name ''
      unset LD_PRELOAD NSS_WRAPPER_PASSWD NSS_WRAPPER_GROUP
      preload=$(cat /etc/ld-nix.so.preload 2>/dev/null || true)
      name=''${preload##*/}
      if [ -z "$name" ] || [ "$name" = libmimalloc.so ]; then
        exec ${binary} "$@"
      fi
      exec ${pkgs.bubblewrap}/bin/bwrap \
        --dev-bind / / \
        --ro-bind ${emptyLdNixSoPreload} /etc/ld-nix.so.preload \
        --unsetenv LD_PRELOAD \
        --unsetenv NSS_WRAPPER_PASSWD \
        --unsetenv NSS_WRAPPER_GROUP \
        -- \
        ${binary} "$@"
    '';
  rustcCommandCompat = rustToolCommandCompat "rustc" "${rustToolchain.toolchain}/bin/rustc";
  rustAnalyzerCommandCompat = rustToolCommandCompat "rust-analyzer" "${pkgs.rust-analyzer}/bin/rust-analyzer";
  cangNixCommandCompat = pkgs.writeShellScriptBin "nix" ''
    unset LD_PRELOAD
    unset NSS_WRAPPER_PASSWD
    unset NSS_WRAPPER_GROUP
    if [ "''${CANG_NIX_OVERLAY:-}" = "1" ]; then
      export NIX_REMOTE="''${NIX_REMOTE:-unix:///nix/var/nix/daemon-socket/socket}"
      cang_nix_ready_marker="/tmp/cang-nix-daemon-ready-$(${pkgs.coreutils}/bin/id -u)"
      if [ ! -e "$cang_nix_ready_marker" ]; then
        ${cangMuslPackage}/bin/cang-guest-init internal nix wait
        ${pkgs.nix}/bin/nix store info --store "$NIX_REMOTE" --json >/dev/null
        : > "$cang_nix_ready_marker"
      fi
    fi
    exec ${pkgs.nix}/bin/nix "$@"
  '';
  cangPodmanCommandCompat = pkgs.writeShellScriptBin "podman" ''
    unset LD_PRELOAD
    unset NSS_WRAPPER_PASSWD
    unset NSS_WRAPPER_GROUP
    if [ "''${CANG_CONTAINERS_STORAGE:-}" = "1" ]; then
      ${cangMuslPackage}/bin/cang-guest-init internal podman wait
    fi
    exec ${pkgs.podman}/bin/podman "$@"
  '';
  cangDockerCommandCompat = pkgs.writeShellScriptBin "docker" ''
    unset LD_PRELOAD
    unset NSS_WRAPPER_PASSWD
    unset NSS_WRAPPER_GROUP
    if [ "''${CANG_CONTAINERS_STORAGE:-}" = "1" ]; then
      ${cangMuslPackage}/bin/cang-guest-init internal podman service-wait
    fi
    exec ${pkgs.podman}/bin/podman "$@"
  '';
  cangDockerComposeCommandCompat = pkgs.writeShellScriptBin "docker-compose" ''
    unset LD_PRELOAD
    unset NSS_WRAPPER_PASSWD
    unset NSS_WRAPPER_GROUP
    if [ "''${CANG_CONTAINERS_STORAGE:-}" = "1" ]; then
      ${cangMuslPackage}/bin/cang-guest-init internal podman service-wait
    fi
    exec ${pkgs.docker-compose}/bin/docker-compose "$@"
  '';
  cangAsDevCommandCompat = pkgs.writeShellScriptBin "cang-as-dev" ''
    unset LD_PRELOAD
    unset NSS_WRAPPER_PASSWD
    unset NSS_WRAPPER_GROUP
    exec ${cangMuslPackage}/bin/cang-guest-init as-dev "$@"
  '';

  nixCommandCompat = cangNixCommandCompat;
  podmanCommandCompat = cangPodmanCommandCompat;
  dockerCommandCompat = cangDockerCommandCompat;
  dockerComposeCommandCompat = cangDockerComposeCommandCompat;
  cangOnlyCommandCompat = [ cangAsDevCommandCompat ];
  nixStoreDbCheck = import ./nix-store-db-check.nix { inherit pkgs; };
  nixStoreRepair = import ./nix-store-repair.nix { inherit pkgs; };

  sidecarProxyWrapper = pkgs.writeShellScriptBin "cang-sidecar-proxy" ''
    LISTEN_PORT="$1"
    SOCKET_PATH="$2"

    unset LD_PRELOAD NSS_WRAPPER_PASSWD NSS_WRAPPER_GROUP

    if ! command -v socat >/dev/null 2>&1; then
      echo "cang-sidecar-proxy: socat not found on PATH" >&2
      exit 127
    fi

    echo "cang-sidecar-proxy: starting socat on port $LISTEN_PORT -> $SOCKET_PATH" >&2

    while true; do
      socat "TCP-LISTEN:$LISTEN_PORT,fork,reuseaddr" "UNIX-CONNECT:$SOCKET_PATH" &
      SOCAT_PID=$!

      # Wait for socat to start listening
      for _ in $(seq 1 50); do
        if timeout 1 ${pkgs.bashInteractive}/bin/bash -c "echo >/dev/tcp/127.0.0.1/$LISTEN_PORT" 2>/dev/null; then
          break
        fi
        sleep 0.1
      done

      if ! kill -0 "$SOCAT_PID" 2>/dev/null; then
        echo "cang-sidecar-proxy: socat failed to start, retrying..." >&2
        sleep 0.5
        continue
      fi

      echo "cang-sidecar-proxy: socat listening on port $LISTEN_PORT" >&2
      wait "$SOCAT_PID"
      echo "cang-sidecar-proxy: socat exited, restarting..." >&2
      sleep 0.5
    done
  '';

  sidecarEntrypoint = pkgs.writeShellScriptBin "cang-nix-sidecar-entrypoint" ''
    set -euo pipefail

    mkdir -p /nix/var/nix/daemon-socket
    mkdir -p /nix/var/log/nix
    chmod 0755 /nix/var/nix/daemon-socket

    echo "cang-sidecar: starting nix-daemon"
    if ! command -v nix-daemon >/dev/null 2>&1; then
      echo "cang-sidecar: nix-daemon not found on PATH"
      exit 127
    fi

    unset LD_PRELOAD NSS_WRAPPER_PASSWD NSS_WRAPPER_GROUP

    echo "cang-sidecar: /nix/store has $(ls /nix/store 2>/dev/null | wc -l) entries"
    echo "cang-sidecar: /nix/var/nix/db $(if [ -d /nix/var/nix/db ]; then echo exists; else echo missing; fi)"

    nix-daemon --daemon 2>/tmp/nix-daemon-stderr.log &
    echo "cang-sidecar: nix-daemon spawned"
    sleep 0.5

    if [ -s /tmp/nix-daemon-stderr.log ]; then
      echo "cang-sidecar: nix-daemon stderr:"
      cat /tmp/nix-daemon-stderr.log >&2
    fi

    if pgrep -x nix-daemon >/dev/null 2>&1; then
      echo "cang-sidecar: nix-daemon process is running"
    else
      echo "cang-sidecar: nix-daemon process is NOT running after startup"
    fi

    attempt=0
    while [ ! -S /nix/var/nix/daemon-socket/socket ]; do
      attempt=$((attempt + 1))
      if [ "$attempt" -ge 300 ]; then
        echo "cang-sidecar: daemon socket not created after 30s"
        ls -ald /nix/var/nix /nix/var/nix/daemon-socket || true
        ls -al /nix/var/nix/daemon-socket || true
        ps -ef | grep -E 'nix-daemon' | grep -v grep | grep -v cang || true
        exit 1
      fi
      sleep 0.1
    done

    echo "cang-sidecar: daemon socket ready"
    echo "cang-sidecar: starting nix-proxy socat"
    cang-sidecar-proxy 19876 /nix/var/nix/daemon-socket/socket &
    exec tail -f /dev/null
  '';

  # `RUST_SRC_PATH` for the guest's rust-analyzer: the pinned toolchain's own
  # `rust-src` component, not nixpkgs' `rustPlatform.rustLibSrc`, which unpacks
  # the source tarball of nixpkgs' 1.95 rustc and would disagree with the
  # compiler the image ships.
  rustSourceImage = pkgs.runCommand "cang-rust-source-image" { } ''
    mkdir -p "$out/share"
    ln -s ${rustToolchain.rustSrc}/lib/rustlib/src/rust/library "$out/share/rust-src"
  '';

  # The guest's Rust: the pinned fenix stable toolchain
  # (`nix/lib/rust-toolchain.nix`) - the same rustc/cargo/clippy/rustfmt a
  # `cargo build` in `nix develop` and a `nix build .#cang` use, so a build
  # inside a cang guest cannot fail on a compiler the project does not build
  # with. `rust-analyzer` and `wild` are not part of the Rust release and stay on
  # nixpkgs; `wild` is its unwrapped build, because the bintools wrapper nixpkgs
  # puts around it costs about a second on every link.
  #
  # A compiler cache is deliberately not here: a project that wants one adds it
  # to its own `nix develop` shell, where its `RUSTC_WRAPPER` and C/C++ shims are
  # configured too. The guest still pre-sets the cache's store and runtime
  # directory (`components/kache.rs`), so an image without the binary keeps the
  # plain compiler and a project that adds one finds everything ready.
  stableRustToolchainPackages = [
    rustToolchain.toolchain
    clangWildWrapper
    wild
    pkgs.rust-analyzer
  ];

  # `getconf`, for kache's cache key: on a GNU toolchain it fingerprints the
  # runtime libc with `getconf GNU_LIBC_VERSION` and falls back to `ldd
  # --version`. The image had neither - no `getconf` at all, and its `ldd` is
  # musl's, which answers "musl libc" and so fails kache's family check. Without
  # this every rustc unit is `skipped: uncacheable: ... unable to identify
  # native Linux gnu-libc (tried getconf/ldd)` and the guest's cache never fills
  # (measured in a live guest; `kache explain --all` is the record).
  #
  # One symlink rather than `pkgs.glibc.bin` on PATH: that output would also
  # publish glibc's `ldd` next to musl's and shadow tooling that wants the musl
  # one. A separate directory for a single non-colliding name is the cheaper
  # contract.
  libcGetconf = pkgs.runCommand "cang-libc-getconf" { } ''
    mkdir -p "$out/bin"
    ln -s ${pkgs.glibc.bin}/bin/getconf "$out/bin/getconf"
  '';

  muslBin = pkgs.lib.getBin pkgs.musl;

  cToolchainPathPackages = [
    pkgs.clang
    pkgs.gcc
    muslBin
  ];

  # `virglrenderer` is here for building cang *inside* a cang guest: the Rust
  # workspace links libkrun, whose build scripts probe virglrenderer.pc through
  # rutabaga_gfx. The image already carried the rest of that chain (clang,
  # libclang, gcc, pkg-config, mesa-libgbm, libdrm, rustc, cargo), so without it
  # `cargo build -p cang` in the guest failed at its first build script while the
  # same build works on the host.
  cToolchainImagePackages = cToolchainPathPackages ++ [
    pkgs.libclang.lib
    pkgs.musl
    pkgs.virglrenderer
  ];

  rustToolchainImageLayer = pkgs.buildEnv {
    name = "cang-rust-toolchain-layer";
    paths = stableRustToolchainPackages;
    pathsToLink = [ "/" ];
  };

  pythonToolchain = pkgs.python3.withPackages (ps: [
    ps.pip
    ps.pyyaml
    ps.tree-sitter
    ps.tree-sitter-rust
  ]);

  dynamicToolchainImagePackages = [
    pkgs.nodejs
    pythonToolchain
    pkgs.uv
  ];
  dynamicToolchainImageLayer = pkgs.buildEnv {
    name = "cang-dynamic-toolchain-layer";
    paths = dynamicToolchainImagePackages;
    pathsToLink = [ "/" ];
  };

  toolingImagePackages = [
    bun
    # The guest's early out-of-memory responder, started by `cang-guest-init`'s
    # responsive component: the kernel's own killer acts only after reclaim has
    # thrashed, and on zram that is millions of failed page scans first.
    pkgs.earlyoom
    # nixpkgs-unstable's cargo-deny, the same one the dev shell's lint gate
    # runs and the same input the image's Rust zg comes from.
    cargoDeny
    pkgs.fzf
    pkgs.neovim
    pkgs.nixfmt
  ]
  ++ pkgs.lib.optional (rtkPrebuilt != null) rtkPrebuilt
  ++ [
    # The firmware libkrun opens by soname (`libkrunfw.so.5`). cang links
    # libkrun itself since it binds libkrun's Rust API, so the shared object is
    # no longer in this layer; a cang built or downloaded into the guest still
    # needs the firmware.
    libkrunfw
    pkgs.starship
  ];
  toolingImageLayer = pkgs.buildEnv {
    name = "cang-tooling-layer";
    paths = toolingImagePackages;
    pathsToLink = [ "/" ];
  };

  # The guest-side Chromium launcher: it sets the environment and the flags a
  # guest GPU stack needs (venus Vulkan, EGL through the image's mesa runtime,
  # VA-API video), and refuses to start a software-rendered browser when the
  # guest was booted without --gpu=drm. The script itself lives in the
  # repository (tools/cang-chromium) and is the same file the tool's README
  # documents; `writeShellScriptBin` supplies the interpreter, so drop the
  # `#!/usr/bin/env bash` line the repository copy needs.
  cangChromium = pkgs.writeShellScriptBin "cang-chromium" (
    pkgs.lib.removePrefix "#!/usr/bin/env bash\n" (
      builtins.readFile ../../tools/cang-chromium/cang-chromium.sh
    )
  );

  # Development browser for the cang guest GPU smoke. Wrapped
  # `ungoogled-chromium` (the same package `environment.systemPackages`
  # installs on NixOS) so the wrapper script provides LD_LIBRARY_PATH /
  # XDG_DATA_DIRS itself; use the dedicated layer so Chromium updates stay
  # cache-stable (only this layer's archive changes, not tooling/rust/base).
  browserImageLayer = pkgs.buildEnv {
    name = "cang-browser-layer";
    paths = [
      pkgs.ungoogled-chromium
      cangChromium
    ];
    pathsToLink = [ "/" ];
  };

  agentImagePackages = [
    pkgs.bubblewrap
    # `sqlite3` CLI only: agent tools (e.g. Magic Context) shell out to it to
    # repair a corrupted local database.
    pkgs.sqlite
    piCodingAgent
    zvecGrep
    # The embedding models `zg` resolves from here instead of downloading in the
    # guest (guest-init links them into the task user's cache; the path is in the
    # image's `CANG_ZG_MODELS_DIR`).
    zgModels
    doltPrebuilt
    beadsPrebuilt
    freshPrebuilt
    # A media client that reports its own decoder choice (`Using hardware
    # decoding (vaapi)`), which is the only unambiguous oracle for whether the
    # guest's VA-API path is used: chromium instantiates its VA-API decoder but
    # reports neither success nor failure at default verbosity. mpv only
    # decodes; `ffmpeg` below is the encode half and the client the live smoke
    # scores frame-by-frame.
    pkgs.mpv-unwrapped
    # A VA-API encoder/decoder client and a profile oracle for the guest. The
    # guest's /nix/store is the image's own closure, so a client that is not in
    # the image cannot run there at all (a host store path, and even a host
    # binary patched to the guest's own glibc, is outside that closure); ffmpeg
    # links the same libav* build mpv already pulls in, so this adds the
    # executable and little else. `vainfo` states the profile/entrypoint set the
    # guest's driver advertises, which is how a decode (VLD) / encode (EncSlice)
    # claim is checked without trusting a client's log.
    pkgs.ffmpeg-headless
    pkgs.libva-utils
  ]
  ++ pkgs.lib.optional (herdrPrebuilt != null) herdrPrebuilt
  ++ pkgs.lib.optional (montyPrebuilt != null) montyPrebuilt;
  agentImageLayer = pkgs.buildEnv {
    name = "cang-agent-layer";
    paths = agentImagePackages;
    pathsToLink = [ "/" ];
  };

  rootlessPodmanImagePackages = [
    pkgs.podman
    pkgs.buildah
    pkgs.crun
    pkgs.conmon
    pkgs.netavark
    pkgs.aardvark-dns
    pkgs.passt
    pkgs.shadow
    pkgs.docker-compose
  ];

  baseImagePackages = [
    mimalloc
    grapheneHardenedMalloc
    hardeningRun
    pkgs.bashInteractive
    pkgs.btrfs-progs
    pkgs.cacert
    pkgs.coreutils
    pkgs.curl
    pkgs.openssl
    pkgs.fd
    pkgs.file
    pkgs.fish
    pkgs.ripgrep
    pkgs.socat
    sidecarProxyWrapper
    sidecarEntrypoint
    pkgs.procps
    pkgs.pkg-config
    pkgs.findutils
    pkgs.gitMinimal
    pkgs.gawk
    pkgs.gnugrep
    pkgs.gnused
    pkgs.gnutar
    pkgs.gzip
    pkgs."hostname-debian"
    pkgs.jq
    pkgs.less
    pkgs.lsof
    pkgs.nix
    nixStoreDbCheck
    nixStoreRepair
    pkgs.diffutils
    pkgs.nss_wrapper
    pkgs.tmux
    rmuxPrebuilt
    pkgs.util-linux
    pkgs.which
  ];

  usrBinEnvCompat = pkgs.runCommand "cang-usr-bin-env-compat" { } ''
    mkdir -p "$out/usr/bin"
    ln -s ${pkgs.coreutils}/bin/env "$out/usr/bin/env"
  '';
  # The guest's VA-API driver: libva opens `virtio_gpu_drv_video.so` from
  # LIBVA_DRIVERS_PATH, and that driver is libgallium. The image's mesa is a
  # prebuilt binary drop with no patch able to reach it, so the guest driver
  # comes from pkgs.mesaVaApi (nixpkgs mesa built from source with this repo's
  # guest-side vrend encode patch) instead.
  vaApiRuntime = pkgs.runCommand "cang-va-runtime" { } ''
    mkdir -p "$out/lib" "$out/dri"
    ln -s ${pkgs.mesaVaApi}/lib/libgallium-${pkgs.mesaVaApi.version}.so "$out/lib/libgallium.so"
    ln -s ../lib/libgallium.so "$out/dri/virtio_gpu_drv_video.so"
  '';

  binInterpreterCompat = pkgs.runCommand "cang-bin-interpreter-compat" { } ''
    mkdir -p "$out/bin"
    ln -s ${pkgs.bashInteractive}/bin/sh "$out/bin/sh"
    ln -s ${pkgs.bashInteractive}/bin/bash "$out/bin/bash"
    ln -s ${pythonToolchain}/bin/python "$out/bin/python"
    ln -s ${pythonToolchain}/bin/python3 "$out/bin/python3"
  '';

  imagePackages =
    baseImagePackages
    ++ rootlessPodmanImagePackages
    ++ cToolchainImagePackages
    ++ [
      rustToolchainImageLayer
      dynamicToolchainImageLayer
      toolingImageLayer
      agentImageLayer
    ]
    ++ [
      browserImageLayer
      pkgs.mesa
      # `vulkaninfo`, the guest's own answer to "is there a Vulkan device?".
      # guest-init runs it once when the guest was started with `--gpu=drm` and
      # only then defaults `zg` to the GGUF model on `auto` - a DRM guest whose
      # venus device is not usable keeps the CPU model instead of paying
      # qwen3-on-CPU for every index.
      pkgs.vulkan-tools
      # glvnd's dispatcher (`libEGL.so.1`). The image exposes it at
      # `/usr/lib/cang-gpu-runtime` for the guest's DRM clients, and a client
      # that links the dispatcher directly (rather than dlopening it) also gets
      # it from the merged `/lib` tree this contents entry produces.
      pkgs.libglvnd
      vaApiRuntime
      pkgs.fontconfig.out
      pkgs.perf
      pkgs.strace
      # `scx_bpfland`, the sched_ext scheduler `cang-guest-init` starts by
      # default (`--scx=off` keeps the fair scheduler). The guest only loads it
      # when its firmware was built with CONFIG_SCHED_CLASS_EXT, and records the
      # reason in /run/cang/scx.status otherwise, so an older pinned firmware
      # still boots with this binary present.
      pkgs.scx.full
      waypipe
      wl-cross-domain-proxy
      # Named by the image's PATH (see `libcGetconf`), so the image DB has to
      # carry it as well as the merged tree.
      libcGetconf
    ];
  imagePathPackages =
    baseImagePackages
    ++ rootlessPodmanImagePackages
    ++ cToolchainPathPackages
    ++ [
      rustToolchainImageLayer
      dynamicToolchainImageLayer
      toolingImageLayer
      agentImageLayer
    ]
    ++ [
      browserImageLayer
      pkgs.vulkan-tools
      pkgs.perf
      pkgs.strace
      # `scx_bpfland` is spawned by name, so the scheduler's `bin` has to be on
      # the guest's PATH, not merely present in the image's store.
      pkgs.scx.full
      waypipe
      wl-cross-domain-proxy
      libcGetconf
    ];
  imagePath = pkgs.lib.makeBinPath (
    [
      rustcCommandCompat
      rustAnalyzerCommandCompat
      nixCommandCompat
      podmanCommandCompat
      dockerCommandCompat
      dockerComposeCommandCompat
    ]
    ++ cangOnlyCommandCompat
    ++ imagePathPackages
  );
  realPodmanBin = "${pkgs.podman}/bin/podman";
  # `limit_layers` keeps the first (budget - 1) layer groups and merges every
  # group after them into a single layer, which `reverse` then puts at the
  # bottom of the image. A budget below the number of groups the pipeline
  # emits therefore silences the per-group split: at 9 the rust toolchain, the
  # C/C++ toolchain and the base rest collapsed into one 3.8 GiB layer, which
  # any input change then re-shipped whole. The pipeline emits 16 groups (the
  # bottom layer is the base rest, then the toolchains, the runtime groups and
  # one layer per named split), so this budget keeps every group its own layer;
  # raise it when a `split_paths` is added.
  cangImageMaxLayers = 17;
  cangImageStoreLayers = cangImageMaxLayers - 1;
  imageContents =
    imagePackages
    ++ [
      rustSourceImage
      usrBinEnvCompat
      binInterpreterCompat
      fishConfig
      starshipConfig
      containerLibPolicySeccompJson
      cangMuslPackage
      nixCommandCompat
      podmanCommandCompat
      dockerCommandCompat
      dockerComposeCommandCompat
    ]
    ++ cangOnlyCommandCompat
    ++ [
      rustcCommandCompat
      rustAnalyzerCommandCompat
    ];
  browserLayerPaths = [ (toString browserImageLayer) ];
  cangInitLayerPaths = [ (toString cangMuslPackage) ];
  # The compat wrappers name the guest-init binary, so their store path changes
  # with every cang version bump. As roots of their own layer they only cost
  # that layer plus the guest-init layer a re-pull, instead of rewriting the
  # bottom layer they used to share with the toolchains.
  compatLayerPaths = builtins.map toString [
    nixCommandCompat
    podmanCommandCompat
    dockerCommandCompat
    dockerComposeCommandCompat
    cangAsDevCommandCompat
  ];
  agentLayerPaths = [ (toString agentImageLayer) ];
  toolingLayerPaths = [ (toString toolingImageLayer) ];
  cToolchainLayerPaths = builtins.map toString cToolchainImagePackages;
  rustLayerPaths = [ (toString rustToolchainImageLayer) ];
  dynamicToolchainLayerPaths = [ (toString dynamicToolchainImageLayer) ];

  # dockerTools `over rest` group: the first element becomes its own layer,
  # the remaining text is piped into the following stages. Keep each named
  # group as its own unflattened layer; only the trailing "rest" is flattened.
  toolingAndBelow = [
    [
      "split_paths"
      toolingLayerPaths
    ]
    [
      "over"
      "rest"
      [
        "pipe"
        [
          [
            "split_paths"
            dynamicToolchainLayerPaths
          ]
          [
            "over"
            "rest"
            [
              "pipe"
              [
                [
                  "split_paths"
                  rustLayerPaths
                ]
                [
                  "over"
                  "rest"
                  [
                    "pipe"
                    [
                      [
                        "split_paths"
                        cToolchainLayerPaths
                      ]
                      [ "flatten" ]
                    ]
                  ]
                ]
                [ "flatten" ]
              ]
            ]
          ]
          [ "flatten" ]
        ]
      ]
    ]
    # The tooling group's "rest" is the flattened remainder (c-toolchain
    # + base "rest"). Keeping the flatten here matches the pre-browser
    # pipeline's limit_layers behavior.
    [ "flatten" ]
  ];

  # agent-layer first, then tooling-and-below as its "rest". The dedicated
  # browser layer nests at the same depth as tooling so it stays its own
  # unflattened layer (cache-stable on Chromium updates).
  agentAndBelow = [
    [
      "split_paths"
      browserLayerPaths
    ]
    [
      "over"
      "rest"
      [
        "pipe"
        [
          [
            "split_paths"
            agentLayerPaths
          ]
          [
            "over"
            "rest"
            [
              "pipe"
              toolingAndBelow
            ]
          ]
          [ "flatten" ]
        ]
      ]
    ]
    [ "flatten" ]
  ];

  cangImageLayeringPipeline = [
    [
      "split_paths"
      cangInitLayerPaths
    ]
    [
      "over"
      "rest"
      [
        "pipe"
        [
          [
            "split_paths"
            compatLayerPaths
          ]
          [
            "over"
            "rest"
            [
              "pipe"
              agentAndBelow
            ]
          ]
          [ "flatten" ]
        ]
      ]
    ]
    [
      "flatten"
    ]
    [
      "limit_layers"
      cangImageStoreLayers
    ]
    [
      "reverse"
    ]
  ];
in
{
  inherit
    agentImageLayer
    rustToolchain
    rustToolchainImageLayer
    vaApiRuntime
    cangImageLayeringPipeline
    cangImageMaxLayers
    browserImageLayer
    toolingImageLayer
    imageContents
    imagePath
    realPodmanBin
    rustSourceImage
    clangWildWrapper
    nixCommandCompat
    nixStoreDbCheck
    nixStoreRepair
    podmanCommandCompat
    dockerCommandCompat
    dockerComposeCommandCompat
    cangAsDevCommandCompat
    rustcCommandCompat
    rustAnalyzerCommandCompat
    libcGetconf
    grapheneHardenedMalloc
    hardeningRun
    mimallocLib
    hardenedMallocLib
    zgModels
    nixBuilderGroupId
    nixBuilderGroupMembers
    nixBuilderPasswdEntries
    ;

  montyPackage = montyPrebuilt;
}
