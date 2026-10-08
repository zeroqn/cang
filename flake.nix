{
  description = "Rust CLI for launching direct-libkrun microVM task environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";

    headless.url = "github:zeroqn/headless";

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
      nixpkgs,
      nixpkgs-unstable,
      headless,
      libkrun-src,
      libkrunfw-src,
      rutabaga-gfx-src,

    }:
    let
      pins = import ./nix/pins.nix;
      systems = import ./nix/lib/systems.nix {
        inherit nixpkgs headless pins;
      };
      # cang's mesa (the VA-API encode fixes + the headless virtio-gpu modifier fix):
      # the prebuilt release asset when one is published for the system, otherwise
      # the same patches built from source. Shared with the guest image's `mesaVaApi`.
      mesaCang = pkgs: import ./nix/lib/mesa-cang.nix { inherit pkgs pins; };
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
      };

      packages = systems.forAllSystems (
        { pkgs, system, ... }:
        let
          bun = (import nixpkgs-unstable { inherit system; }).bun;
          rioBin = headless.packages.${system}.rio-bin or null;
          piCodingAgent = import ./nix/pkgs/pi-coding-agent.nix {
            inherit pkgs pins;
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
          zvecGrep = import ./nix/pkgs/zvec-grep.nix {
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
              libkrunfw
              ;
            src = workspaceSrc;
            krunInitBinary = libkrunSource.krunInitBinary;
          };
          rustPackagesCiSccache = import ./nix/pkgs/cang-rust.nix {
            inherit
              pkgs
              pins
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
                ;
              inherit cangMuslPackage;
            };
          cangImage = mkImage rustPackages.cangMuslPackage;
          cangImageCiSccache = mkImage rustPackagesCiSccache.cangMuslPackage;
        in
        {
          default = rustPackages.rustPackage;
          pi-coding-agent = piCodingAgent;
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
        // pkgs.lib.optionalAttrs (rioBin != null) {
          rio-bin = rioBin;
        }
        // pkgs.lib.optionalAttrs (rtkPrebuilt != null) {
          rtk-prebuilt = rtkPrebuilt;
        }
        // {
          # nixpkgs' mesa with cang's VA-API encode fixes, for consumers that cannot
          # apply the overlay (the guest image uses the same override as `mesaVaApi`).
          mesa-rbsp-bounds = mesaCang pkgs;
          # The source build of the same patches: what `.github/workflows/build-mesa.yml`
          # builds, tars and publishes as the `mesa-rbsp-bounds` release assets that
          # `mesa-rbsp-bounds` / `overlays.default` then consume as prebuilts.
          mesa-release-build = (import ./nix/lib/mesa-patched.nix) pkgs.mesa;
        }
      );

      checks = systems.forAllSystems (
        {
          pkgs,

          system,
          ...
        }:
        let
          bun = (import nixpkgs-unstable { inherit system; }).bun;
          packages = self.packages.${system};
          cangImageChecks = import ./nix/image/checks.nix {
            inherit pkgs;
            bun = bun;
            piCodingAgent = packages.pi-coding-agent;
            rioBin = packages.rio-bin or null;
            herdrPrebuilt = packages.herdr-prebuilt or null;
            montyPrebuilt = packages.monty-prebuilt or null;
            rmuxPrebuilt = packages.rmux-prebuilt;
            rtkPrebuilt = packages.rtk-prebuilt or null;
            zvecGrep = packages.zvec-grep;
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
                  echo "packages.virglrenderer is the plain nixpkgs build; the host vrend patches in nix/lib/systems.nix are missing" >&2
                  exit 1
                fi
                touch "$out"
              '';

        }
      );

      devShells = systems.forAllSystems (
        { pkgs, ... }:
        {
          default = import ./nix/shell/devshell.nix {
            inherit pkgs;
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
