{
  description = "Rust CLI for launching direct-libkrun microVM task environments";

  # The Rust toolchain cang builds with is fenix's stable release (see
  # `nix/lib/rust-toolchain.nix`). fenix's cached toolchain outputs are served by
  # `nix-community.cachix.org` (measured: a fenix revision's own toolchain path
  # is there, the same revision's path built against this flake's nixpkgs is
  # not); `fenix.cachix.org` is fenix's own cache and is listed too. Adding both
  # as extra substituters is what makes the toolchain a download rather than a
  # local build on a fresh machine; `extra-*` settings are only honoured for a
  # user who trusts this flake.
  nixConfig = {
    extra-substituters = [
      "https://fenix.cachix.org"
      "https://nix-community.cachix.org"
    ];
    extra-trusted-public-keys = [
      "fenix.cachix.org-1:ecJhr+RdYEdcVgUkjruiYhjbBloIEGov7bos90cZi0Q="
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";

    # The Rust toolchain: fenix's current stable (nixpkgs 26.05's rustc is
    # 1.95). `nix/lib/rust-toolchain.nix` turns it into the `rustPlatform` the
    # cang packages build with, the dev shell's `rustc`/`cargo` and the guest
    # image's rust layer.
    #
    # The input deliberately does not follow this flake's `nixpkgs`: fenix's CI
    # builds its toolchain derivations against fenix's own locked nixpkgs and
    # publishes them to the `nix-community.cachix.org` substituter, so following
    # this flake's nixpkgs changes those derivations and every one of them
    # becomes a cache miss (measured: the followed build's toolchain paths 404 on
    # the substituter while a fenix revision's own paths are served). The extra
    # nixpkgs costs an evaluation, not a build; nothing cang compiles is built
    # against it - the compiler is a tool that runs under this flake's stdenv.
    fenix.url = "github:nix-community/fenix";

    # `cang-libkrun` depends on libkrun's source at `deps/libkrun` by path (cang
    # links libkrun's Rust API, so libkrun has to be compiled by cang's own
    # rustc). A flake's own source cannot carry submodule contents - neither from
    # a `git` checkout nor with `inputs.self.submodules = true`, which only
    # writes `submodules = true` into the ref a downstream `flake.lock` records
    # and the `github:` scheme then rejects (NixOS/nix#13571), while the flake's
    # own tree stays submodule-less - so the fork revisions are inputs, grafted
    # into the workspace source by `nix/pkgs/workspace-src.nix`. Keep them equal
    # to the pointers in `.gitmodules`; a local fork edit is
    # `--override-input libkrun-src "git+file://$PWD/deps/libkrun"`.
    libkrun-src = {
      url = "github:zeroqn/libkrun/cang";
      flake = false;
    };
    libkrunfw-src = {
      url = "github:zeroqn/libkrunfw/cang";
      flake = false;
    };

    # libkrun's GPU device depends on `rutabaga_gfx`, which libkrun pins to this
    # fork: it carries `VIRGL_RENDERER_USE_VIDEO` (bit 11), the flag cang's
    # `--gpu=drm` asks for and upstream's `VirglRendererFlags` does not have.
    # cang compiles the checkout at `deps/rutabaga_gfx` through a `[patch]` in the
    # workspace manifest, so the same submodule/input split as libkrun applies:
    # keep this revision equal to the pointer in `.gitmodules`, and use
    # `--override-input rutabaga-gfx-src "git+file://$PWD/deps/rutabaga_gfx"` for
    # a local patch.
    rutabaga-gfx-src = {
      url = "github:zeroqn/rutabaga_gfx/cang";
      flake = false;
    };
  };

  outputs =
    {
      self,
      fenix,
      nixpkgs,
      nixpkgs-unstable,
      libkrun-src,
      libkrunfw-src,
      rutabaga-gfx-src,

    }:
    let
      pins = import ./nix/pins.nix;
      systems = import ./nix/lib/systems.nix {
        inherit fenix nixpkgs pins;
      };
      # cang's mesa (the VA-API encode fixes + the headless virtio-gpu modifier fix):
      # the prebuilt release asset when one is published for the system, otherwise
      # the same patches built from source. Shared with the guest image's `mesaVaApi`.
      mesaCang = pkgs: import ./nix/lib/mesa-cang.nix { inherit pkgs pins; };
      # cang's host-side virglrenderer (the VA-API encode fixes, at the pinned
      # post-1.3.0 upstream revision): the prebuilt release asset when one is
      # published for the system, otherwise the same source build. This is
      # what the cang packages link and what their render server comes from, and
      # it is exported for downstream hosts as `packages.virglrenderer` /
      # `overlays.default`.
      virglrendererCang = pkgs: import ./nix/lib/virglrenderer-cang.nix { inherit pkgs pins; };
      # nixpkgs-unstable's bun moved to the release whose embedded root store
      # rejects the compromised emSign intermediates (CVE-2026-111943): the root
      # store lives inside the binary, so the image's `bun` and the pi standalone
      # binary have to be built from it. Exported for downstream hosts as
      # `packages.bun`. See nix/lib/bun-cang.nix.
      bunCang = pkgs: import ./nix/lib/bun-cang.nix { inherit pkgs pins; };
    in
    {
      # Apply to a host's nixpkgs to give *its* VA-API driver the same fix the guest
      # image gets: `nixpkgs.overlays = [ cang.overlays.default ];`. Needed because a
      # host whose own vadriver is mesa (virtio-gpu, radeonsi, ...) hits the same hang
      # in vl_rbsp_ue() when vrend hands it a packed header, and the VM worker runs in
      # glibc secure-execution mode, where libva's secure_getenv() ignores
      # LIBVA_DRIVERS_PATH - so pointing it at the patched build by environment does
      # not work. See docs/wayfinder/guest-vaapi-video/tickets/08-cqp-encode-hangs.md.
      overlays.default = final: prev: {
        mesa = mesaCang prev;
        mesaVaApi = mesaCang prev;
        virglrenderer = virglrendererCang prev;
      };

      packages = systems.forAllSystems (
        {
          pkgs,
          rustToolchain,
          system,
          ...
        }:
        let
          # The image's `bun` and the toolchain that compiles pi: both from
          # nixpkgs-unstable and both moved to the release with the fixed root
          # store, because each of them carries that store. See
          # nix/lib/bun-cang.nix.
          bun = bunCang (import nixpkgs-unstable { inherit system; });
          # The un-overridden nixpkgs. `nix/lib/systems.nix` replaces
          # `virglrenderer` with the prebuilt-or-source selector, which is what
          # consumers want but not what CI can build the asset from.
          rawPkgs = import nixpkgs { inherit system; };
          # zg's Rust workspace pins `rust-version = "1.98"` and a 1.27 ONNX
          # Runtime API the 26.05 input does not carry (its rustc is 1.95 and its
          # onnxruntime 1.24). Only zg builds against unstable, so the image stays
          # on 26.05; see nix/pkgs/zvec-grep.nix.
          zgPkgs = import nixpkgs-unstable { inherit system; };
          # The ONNX Runtime ABI shim the pi wrapper exposes as
          # `libonnxruntime.so.1`/`libonnxrtabi24.so.1`: the vendored
          # `onnxruntime-node` binding NEEDs that soname and imports
          # `OrtGetApiBase@VERS_1.24.3`, while `zg` NEEDs it too and imports
          # `@VERS_1.27.1` - and no nixpkgs ONNX Runtime exports both nodes, so
          # one file has to carry both and forward them. It is built *and*
          # targeted by this input (unstable, the same ONNX Runtime `zg` links
          # and the image already carries): the 1.27.1 library is what the
          # `@VERS_1.27.1` caller's `OrtApi` needs, and it loads in a guest,
          # measured - the preloaded-malloc failure of 1.27.1 that the earlier
          # 26.05-input attempt hit belongs to this *#host* shell, not the guest.
          # See nix/pkgs/onnxruntime-shim.nix.
          onnxruntimeShim = import ./nix/pkgs/onnxruntime-shim.nix {
            pkgs = zgPkgs;
            onnxruntime = zgPkgs.onnxruntime;
          };
          # The single `libstdc++.so.6` the pi wrapper injects is a ceiling for
          # every process pi launches, because `LD_LIBRARY_PATH` is searched
          # ahead of each binary's own run path. It therefore has to be the
          # newest C++ runtime any consumer in the image needs, and that is
          # nixpkgs-unstable's gcc: `zg` links an ONNX Runtime built against
          # `GLIBCXX_3.4.35/36`, while the 26.05 input's gcc-15 stops at 3.4.34.
          piCodingAgent = import ./nix/pkgs/pi-coding-agent.nix {
            inherit
              pkgs
              pins
              bun
              onnxruntimeShim
              ;
            nativeAddonCxxRuntime = zgPkgs.stdenv.cc.cc.lib;
          };
          herdrPrebuilt = import ./nix/pkgs/herdr-prebuilt.nix {
            inherit pkgs pins;
          };
          montyPrebuilt = import ./nix/pkgs/monty-prebuilt.nix {
            inherit pkgs pins;
          };
          rmuxPrebuilt = import ./nix/pkgs/rmux-prebuilt.nix {
            inherit pkgs pins;
          };
          symposium = import ./nix/pkgs/symposium.nix {
            inherit pkgs;
          };
          rtkPrebuilt = import ./nix/pkgs/rtk-prebuilt.nix {
            inherit pkgs pins;
          };
          zvecGrep = import ./nix/lib/zvec-grep-cang.nix {
            pkgs = zgPkgs;
            inherit pins;
          };
          # The guest's waypipe and the matching host client: zeroqn/headless's
          # prebuilt release asset where one is pinned for the system, otherwise
          # nixpkgs' own waypipe. See nix/lib/waypipe-cang.nix.
          waypipe = import ./nix/lib/waypipe-cang.nix {
            inherit pkgs pins;
          };
          doltPrebuilt = import ./nix/pkgs/dolt-prebuilt.nix {
            inherit pkgs pins;
          };
          beadsPrebuilt = import ./nix/pkgs/beads-prebuilt.nix {
            inherit pkgs pins;
          };
          freshPrebuilt = import ./nix/pkgs/fresh-prebuilt.nix {
            inherit pkgs pins;
          };
          containerLibPolicySeccompJson = import ./nix/pkgs/container-lib-policy-seccomp-json.nix {
            inherit pkgs pins;
          };
          libkrunfw = pkgs.callPackage ./nix/pkgs/libkrunfw.nix {
            inherit pins;
            libkrunfwSrc = libkrunfw-src;
          };
          # The same firmware built from the fork checkout instead of the pinned
          # asset. Only `.#cang-dev` uses it, for local kernel-configuration
          # experiments.
          libkrunfwLocal = pkgs.callPackage ./nix/pkgs/libkrunfw.nix {
            inherit pins;
            libkrunfwSrc = libkrunfw-src;
            useLocalSource = true;
          };
          # libkrun's source tree plus what building it inside Nix needs (the
          # vendored registry and the musl guest init).
          libkrunSource = import ./nix/pkgs/libkrun-source.nix {
            inherit pkgs;
            src = libkrun-src;
          };
          # cang's own tree with the fork checkouts grafted into `deps/`: what a
          # build actually compiles.
          workspaceSrc = import ./nix/pkgs/workspace-src.nix {
            inherit pkgs;
            src = self;
            libkrunSrc = libkrun-src;
            libkrunfwSrc = libkrunfw-src;
            rutabagaGfxSrc = rutabaga-gfx-src;
          };
          wl-cross-domain-proxy = pkgs.callPackage ./nix/wl-cross-domain-proxy.nix { };
          renderServerEnv = import ./nix/lib/render-server-env.nix {
            inherit pkgs;
          };
          prebuiltCang = import ./nix/pkgs/cang-prebuilt.nix {
            inherit
              pkgs
              pins
              libkrunfw
              ;
            renderServerEnv = renderServerEnv.env;
          };
          rustPackages = import ./nix/pkgs/cang-rust.nix {
            inherit
              pkgs
              pins
              rustToolchain
              libkrunfw
              ;
            src = workspaceSrc;
            krunInitBinary = libkrunSource.krunInitBinary;
          };
          rustPackagesCiSccache = import ./nix/pkgs/cang-rust.nix {
            inherit
              pkgs
              pins
              rustToolchain
              libkrunfw
              ;
            src = workspaceSrc;
            krunInitBinary = libkrunSource.krunInitBinary;
            enableCiSccache = true;
          };
          # Local development: the firmware is built from the fork checkout
          # (kernel and all) instead of the pinned asset, so kernel work is an
          # `--override-input libkrunfw-src "git+file://$PWD/deps/libkrunfw"`
          # away from this target.
          rustPackagesDev = import ./nix/pkgs/cang-rust.nix {
            inherit
              pkgs
              pins
              rustToolchain
              ;
            src = workspaceSrc;
            libkrunfw = libkrunfwLocal;
            krunInitBinary = libkrunSource.krunInitBinary;
          };
          mkImage =
            cangMuslPackage:
            import ./nix/image/container.nix {
              inherit
                pkgs
                rustToolchain
                cargoDeny
                wild

                piCodingAgent
                herdrPrebuilt
                montyPrebuilt
                rmuxPrebuilt
                rtkPrebuilt
                zvecGrep
                waypipe
                doltPrebuilt
                beadsPrebuilt
                freshPrebuilt
                containerLibPolicySeccompJson
                libkrunfw
                wl-cross-domain-proxy
                onnxruntimeShim
                bun
                ;
              inherit cangMuslPackage;
            };
          # The image ships the same `cargo deny` the dev shell's lint gate runs
          # (nixpkgs-unstable's), next to the Rust zg and ONNX Runtime shim it
          # already takes from that input.
          cargoDeny = zgPkgs.cargo-deny;
          # The guest's Rust linker (`clang_wild_wrapper`), from the same input
          # as the Rust zg and the ONNX Runtime shim.
          wild = zgPkgs.wild;
          cangImage = mkImage rustPackages.cangMuslPackage;
          cangImageCiSccache = mkImage rustPackagesCiSccache.cangMuslPackage;
        in
        {
          default = rustPackages.rustPackage;
          pi-coding-agent = piCodingAgent;
          onnxruntime-shim = onnxruntimeShim;
          rmux-prebuilt = rmuxPrebuilt;
          symposium = symposium;
          cang = rustPackages.rustPackage;
          cang-ci-sccache = rustPackagesCiSccache.rustPackage;
          cang-dev = rustPackagesDev.rustPackage;
          cang-prebuilt = prebuiltCang;
          cang-render-server-env = renderServerEnv.file;
          cang-musl = rustPackages.cangMuslPackage;
          cang-musl-ci-sccache = rustPackagesCiSccache.cangMuslPackage;
          libkrunfw = libkrunfw;
          virglrenderer = pkgs.virglrenderer;
          wl-cross-domain-proxy = wl-cross-domain-proxy;
          podman = pkgs.podman;
          container = cangImage;
          container-ci-sccache = cangImageCiSccache;
          container-lib-policy-seccomp-json = containerLibPolicySeccompJson;
          zvec-grep = zvecGrep;
          waypipe-bin = waypipe;
          dolt-prebuilt = doltPrebuilt;
          beads-prebuilt = beadsPrebuilt;
          fresh-prebuilt = freshPrebuilt;
        }
        // pkgs.lib.optionalAttrs (herdrPrebuilt != null) {
          herdr-prebuilt = herdrPrebuilt;
        }
        // pkgs.lib.optionalAttrs (montyPrebuilt != null) {
          monty-prebuilt = montyPrebuilt;
        }
        // pkgs.lib.optionalAttrs (rtkPrebuilt != null) {
          rtk-prebuilt = rtkPrebuilt;
        }
        // {
          # nixpkgs' mesa with cang's VA-API encode fixes, for consumers that cannot
          # apply the overlay (the guest image uses the same override as `mesaVaApi`):
          # the prebuilt release asset when one is pinned for the system, otherwise
          # the same patches built from source.
          mesa-cang = mesaCang pkgs;
          # The same bun the image ships and compiles pi with, for a downstream
          # host whose own bun users need the fixed embedded root store before its
          # nixpkgs carries it (NixOS/nixpkgs#577354).
          bun = bun;
          # The source build of the patches `.#virglrenderer` carries: what
          # `.github/workflows/build-virglrenderer.yml` builds, tars and publishes
          # as the per-system release assets that `virglrenderer-cang.nix` then
          # consumes as a prebuilt.
          virglrenderer-release-build = (import ./nix/lib/virglrenderer-patched.nix) {
            inherit pins;
            pkgs = rawPkgs;
            virglrenderer = rawPkgs.virglrenderer;
          };
          # The source build of the same patches: what `.github/workflows/build-mesa.yml`
          # builds, tars and publishes as the per-system release assets that
          # `mesa-cang` / `overlays.default` then consume as prebuilts.
          mesa-release-build = (import ./nix/lib/mesa-patched.nix) pkgs.mesa;
          # The source build of the Rust zg: what `.github/workflows/build-zg.yml`
          # builds, tars and publishes as the per-system release asset that
          # nix/lib/zvec-grep-cang.nix then consumes as a prebuilt.
          zvec-grep-release-build = import ./nix/pkgs/zvec-grep.nix {
            pkgs = zgPkgs;
            inherit pins;
          };
        }
      );

      checks = systems.forAllSystems (
        {
          pkgs,

          rustToolchain,
          system,
          ...
        }:
        let
          unstablePkgs = import nixpkgs-unstable { inherit system; };
          packages = self.packages.${system};
          # The checks must see the same bun the image does, patched root store
          # included; taking it from the packages output is what keeps them in
          # step.
          bun = packages.bun;
          # The image ships nixpkgs-unstable's cargo-deny and its wild linker,
          # so these contracts resolve the same packages.
          cargoDeny = unstablePkgs.cargo-deny;
          wild = unstablePkgs.wild;
          cangImageChecks = import ./nix/image/checks.nix {
            inherit
              pkgs
              rustToolchain
              cargoDeny
              wild
              ;
            bun = bun;
            piCodingAgent = packages.pi-coding-agent;
            herdrPrebuilt = packages.herdr-prebuilt or null;
            montyPrebuilt = packages.monty-prebuilt or null;
            rmuxPrebuilt = packages.rmux-prebuilt;
            rtkPrebuilt = packages.rtk-prebuilt or null;
            zvecGrep = packages.zvec-grep;
            onnxruntimeShim = packages.onnxruntime-shim;
            waypipe = packages.waypipe-bin;
            doltPrebuilt = packages.dolt-prebuilt;
            beadsPrebuilt = packages.beads-prebuilt;
            freshPrebuilt = packages.fresh-prebuilt;
            containerLibPolicySeccompJson = packages.container-lib-policy-seccomp-json;
            libkrunfw = packages.libkrunfw;
            wl-cross-domain-proxy = packages.wl-cross-domain-proxy;
            cangMuslPackage = packages.cang-musl;
          };
        in
        {
          container-nix-db-metadata = cangImageChecks.imageConfigNixDbRefs;
          # The image's bun (and the toolchain behind pi) has to be the pinned
          # release: the fix is a root store compiled into the binary, so which
          # release the binary came from is the only thing a build can assert
          # about it.
          bun-cang-version =
            pkgs.runCommand "bun-cang-version"
              {
                bun = packages.bun;
              }
              ''
                $bun/bin/bun --version | grep -Fx '${pins.bun.version}'
                touch $out
              '';
          # The shim's own loader check: builds the three consumer shapes (the
          # binding's node and name, `zg`'s node, and a patched binding's name)
          # and runs each against the real ONNX Runtime, including the
          # ONNXRUNTIME_LIB_PATH and ORT_SHIM_REAL_LIB overrides.
          onnxruntime-shim-probe = packages.onnxruntime-shim.probes;
          container-codex-absent = cangImageChecks.codexAbsent;
          container-omx-absent = cangImageChecks.omxAbsent;
          container-omp-absent = cangImageChecks.ompAbsent;
          container-dirge-absent = cangImageChecks.dirgeAbsent;
          container-gh-absent = cangImageChecks.ghAbsent;
          container-root-cargo-absent = cangImageChecks.rootCargoAbsent;
          container-wrapper-contracts = cangImageChecks.wrapperContracts;
          # The render-server environment is defined once
          # (nix/lib/render-server-env.nix) and consumed from two directions:
          # .#cang-prebuilt's released wrapper bakes the values in, and the
          # chromium GPU smoke sources the file form to launch a raw-ELF .#cang.
          # Assert that file still sources with all three variables and still
          # lets a caller's exported value win, so a no-argument smoke run keeps
          # working.
          cang-render-server-env-sources =
            pkgs.runCommand "cang-render-server-env-sources"
              {
                envFile = packages.cang-render-server-env;
              }
              ''
                set -eu
                . "$envFile/render-server-env.sh"
                [ -n "''${CANG_MESA_LIBDIR:-}" ] || { echo "CANG_MESA_LIBDIR is unset after sourcing" >&2; exit 1; }
                # The consumer is a child process (cang), so sourcing has to
                # *export* the variables: a shell variable set with VAR:= would
                # still leave cang aborting with "mesa library directory is not
                # set".
                child="$(bash -c 'printf "%s:%s:%s" "$CANG_MESA_LIBDIR" "$CANG_MESA_ICD" "$CANG_VULKAN_LOADER_LIBDIR"')"
                [ "$child" = "$CANG_MESA_LIBDIR:$CANG_MESA_ICD:$CANG_VULKAN_LOADER_LIBDIR" ] || { echo "the render-server variables are not exported to child processes (child saw: $child)" >&2; exit 1; }
                [ -d "$CANG_MESA_LIBDIR" ] || { echo "CANG_MESA_LIBDIR is not a directory: $CANG_MESA_LIBDIR" >&2; exit 1; }
                [ -r "$CANG_MESA_ICD" ] || { echo "CANG_MESA_ICD is not readable: $CANG_MESA_ICD" >&2; exit 1; }
                [ -d "$CANG_VULKAN_LOADER_LIBDIR" ] || { echo "CANG_VULKAN_LOADER_LIBDIR is not a directory: $CANG_VULKAN_LOADER_LIBDIR" >&2; exit 1; }
                exported="$(export CANG_MESA_LIBDIR=/custom; . "$envFile/render-server-env.sh"; printf '%s' "$CANG_MESA_LIBDIR")"
                [ "$exported" = /custom ] || { echo "sourcing overrode an exported CANG_MESA_LIBDIR (got $exported)" >&2; exit 1; }
                touch "$out"
              '';
          # The exported `virglrenderer` is the host-side patched build the cang
          # packages ship (libkrun links libvirglrenderer and the render-server
          # helper is symlinked from it), so downstream consumers cannot pick up
          # an unpatched vrend by consuming this output.
          virglrenderer-is-patched =
            pkgs.runCommand "virglrenderer-is-patched"
              {
                # Plain strings: comparing the two derivations must not pull
                # either virglrenderer build into this check's closure.
                patched = builtins.unsafeDiscardStringContext packages.virglrenderer.drvPath;
                plain =
                  builtins.unsafeDiscardStringContext
                    (import nixpkgs { inherit system; }).virglrenderer.drvPath;
              }
              ''
                if [ "$patched" = "$plain" ]; then
                  echo "packages.virglrenderer is the plain nixpkgs build; the host vrend patches in nix/lib/virglrenderer-patched.nix are missing" >&2
                  exit 1
                fi
                touch "$out"
              '';

        }
      );

      devShells = systems.forAllSystems (
        {
          pkgs,
          rustToolchain,
          system,
          ...
        }:
        {
          default =
            let
              unstablePkgs = import nixpkgs-unstable { inherit system; };
            in
            import ./nix/shell/devshell.nix {
              inherit pkgs rustToolchain;
              # `cargo deny` follows the advisory database and its own lock-file
              # parser, so the lint gate uses nixpkgs-unstable's cargo-deny
              # instead of the 26.05 input's (0.19.6 -> 0.20.2). The Rust linker
              # is the image's `wild`, from the same input.
              cargoDeny = unstablePkgs.cargo-deny;
              wild = unstablePkgs.wild;
              libkrunfw = pkgs.callPackage ./nix/pkgs/libkrunfw.nix {
                inherit pins;
                libkrunfwSrc = libkrunfw-src;
              };
              krunInitBinary =
                (import ./nix/pkgs/libkrun-source.nix {
                  inherit pkgs;
                  src = libkrun-src;
                }).krunInitBinary;
            };
        }
      );

      apps = systems.forAllSystems (
        { pkgs, ... }:
        import ./nix/apps/default.nix {
          inherit self pkgs;
        }
      );
    };
}
