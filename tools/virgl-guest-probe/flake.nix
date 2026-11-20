{
  description = "In-guest virgl venus probe: boot a bare libkrun VM with a venus virtio-gpu and check the guest sees a Vulkan device";

  inputs = {
    # The parent repo: provides the pinned libkrun fork revision (as its
    # `libkrun-src` input, the same commit `deps/libkrun` points at) and, via
    # its nixpkgs, the matching virglrenderer render server for RENDER_SERVER
    # mode.
    repo.url = "path:../..";
  };

  outputs =
    { self, repo, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = repo.inputs.nixpkgs.lib.genAttrs systems;

      # libkrun's C API (`libkrun.so` + `libkrun_init.so`), built from the fork
      # revision the parent repo pins -- not from `packages.<system>.libkrun`,
      # which the repo does not export (cang compiles libkrun's *Rust* API into
      # its own binary through `crates/cang-libkrun`, so it never needs the
      # shared libraries this tool links against).
      #
      # The source is the parent's `libkrun-src` flake input, which
      # `flake.lock` pins to the same commit `.gitmodules` names for
      # `deps/libkrun` (v2.0.0-cang.22 = 2fad450a). libkrun's own Cargo.toml
      # pins `rutabaga_gfx` to `zeroqn/rutabaga_gfx` at the commit the parent
      # pins too, so vendoring the workspace's lock is what makes this
      # reproducible; `cargoDeps` below is the hash of that vendored set
      # (refresh it with `nix build .#libkrun` after a pin bump).
      mkLibkrun =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
          libkrunSrc = repo.inputs.libkrun-src;
          # The parent repo's own recipe for the thing a Nix build of libkrun
          # cannot produce by itself: the musl `krun-init` guest init that
          # `krun-init-blob`'s build script embeds. Imported read-only, so the
          # tool builds the init with the same package set (and the same
          # vendored-registry hash) the product does.
          libkrunSource = import "${repo}/nix/pkgs/libkrun-source.nix" {
            inherit pkgs;
            src = libkrunSrc;
          };
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "libkrun";
          version = "2.0.0";

          src = libkrunSrc;

          cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
            src = libkrunSrc;
            hash = "sha256-cRDdggoYkM+phU9TCk6aBaRu+JaLX+IHrIlUFJxhQ48=";
          };

          # bindgen (krun-display / krun-input), pkg-config + virglrenderer +
          # gbm (rutabaga_gfx's virgl_renderer feature), rustfmt (ffier's client
          # generator shells out to it), patchelf (fixupPhase on the cdylibs).
          nativeBuildInputs = [
            pkgs.rustPlatform.bindgenHook
            pkgs.pkg-config
            pkgs.rustfmt
            pkgs.patchelf
          ];
          buildInputs = [
            pkgs.virglrenderer
            pkgs.libgbm
          ];

          # `krun-init-blob`'s build script would otherwise cross-build the guest
          # init itself (no musl rust std in the host toolchain); pointing it at
          # the prebuilt one is the same escape hatch the product uses.
          env.KRUN_INIT_BINARY_PATH = "${libkrunSource.krunInitBinary}/bin/krun-init";

          # `ffi` is the C ABI bridge this tool calls; `gpu` is the virtio-gpu
          # device. Nothing else (no blk/net/vhost-user) is needed for a probe.
          cargoBuildFlags = [
            "-p"
            "libkrun"
            "--features"
            "ffi,gpu"
          ];
          doCheck = false;

          postBuild = ''
            runHook prePostBuild
            # `libkrun_init.so`: the init-blob cdylib whose `krun_init_*` calls
            # write the guest init and its command line into the VM.
            cargo build --release --offline -p krun-init-blob --features ffi
            runHook postPostBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/lib" "$out/include"
            # The library names upstream's Makefile installs: a full-versioned
            # file plus the ABI soname and the linker name as symlinks, so
            # `-lkrun` records the same DT_NEEDED the C ABI promises.
            # cargoBuildHook passes --target, so the cdylibs land under
            # target/<triple>/release (the init-blob build in postBuild does
            # not); find them rather than depend on which dir each landed in.
            krun_so="$(find target -type f -name libkrun.so | head -n1)"
            init_so="$(find target -type f -name libkrun_init.so | head -n1)"
            echo "libkrun: $krun_so"
            echo "libkrun_init: $init_so"
            install -m755 "$krun_so" "$out/lib/libkrun.so.2.0.0"
            ln -s libkrun.so.2.0.0 "$out/lib/libkrun.so.2"
            ln -s libkrun.so.2 "$out/lib/libkrun.so"
            install -m755 "$init_so" "$out/lib/libkrun_init.so.0.1.0"
            ln -s libkrun_init.so.0.1.0 "$out/lib/libkrun_init.so.0"
            ln -s libkrun_init.so.0 "$out/lib/libkrun_init.so"
            install -m644 include/libkrun.h include/libkrun_display.h \
              include/libkrun_init.h "$out/include/"
            runHook postInstall
          '';

          meta.mainProgram = "libkrun";
        };

      mkGuestProbe =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
        in
        pkgs.stdenv.mkDerivation {
          pname = "virgl-guest-probe";
          version = "0.1.0";
          # Only the probe source, not the whole repo tree (the repo has 100k+
          # files under target/ -- walking them for src causes fd exhaustion).
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = ./guest-probe.c;
          };

          nativeBuildInputs = [ pkgs.gcc ];
          # vulkan-headers only for the Abi/struct layouts; the probe dlopens
          # libvulkan.so.1 at runtime, so libvulkan is deliberately NOT linked.
          buildInputs = [ pkgs.vulkan-headers ];

          buildPhase = ''
            runHook preBuild
            gcc -O2 -Wall -Wno-deprecated-declarations guest-probe.c \
              -I${pkgs.vulkan-headers}/include -ldl \
              -o guest-probe-bin
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            cp guest-probe-bin "$out/bin/guest-probe"
            runHook postInstall
          '';

          meta.mainProgram = "guest-probe";
        };

      mkGuestRootfs =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
          guestProbe = self.packages.${system}.guest-probe;
        in
        import ./guest-rootfs.nix { inherit pkgs guestProbe; };

      mkLauncher =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
          libkrun = self.packages.${system}.libkrun;
        in
        pkgs.stdenv.mkDerivation {
          pname = "virgl-guest-launcher";
          version = "0.1.0";
          # launcher.c is the build input; run.sh is needed below to wrap it.
          # Restrict the source to just those two files so the whole-repo walk
          # (and its fd exhaustion) is avoided.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./abivm.h
              ./launcher.c
            ];
          };

          nativeBuildInputs = [ pkgs.gcc ];
          buildInputs = [ libkrun ];

          buildPhase = ''
            runHook preBuild
            gcc -O2 -Wall launcher.c \
              -I${libkrun}/include -L${libkrun}/lib -lkrun_init -lkrun \
              -Wl,-rpath,${libkrun}/lib \
              -o launcher-bin
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            cp launcher-bin "$out/bin/launcher"
            runHook postInstall
          '';

          meta.mainProgram = "launcher";
        };

      mkLauncherRs =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
          libkrun = self.packages.${system}.libkrun;
        in
        pkgs.stdenv.mkDerivation {
          pname = "virgl-guest-launcher-rs";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./abivm.h
              ./launcher-rs.c
            ];
          };

          nativeBuildInputs = [ pkgs.gcc ];
          buildInputs = [ libkrun ];

          buildPhase = ''
            runHook preBuild
            gcc -O2 -Wall launcher-rs.c \
              -I${libkrun}/include -L${libkrun}/lib -lkrun_init -lkrun \
              -Wl,-rpath,${libkrun}/lib \
              -o launcher-rs-bin
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            cp launcher-rs-bin "$out/bin/launcher-rs"
            runHook postInstall
          '';

          meta.mainProgram = "launcher-rs";
        };

      mkSandboxRs =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
        in
        pkgs.stdenv.mkDerivation {
          pname = "virgl-guest-sandbox-rs";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = ./sandbox-rs.c;
          };

          nativeBuildInputs = [ pkgs.gcc ];
          buildInputs = [ pkgs.libseccomp ];

          buildPhase = ''
            runHook preBuild
            gcc -O2 -Wall sandbox-rs.c \
              -I${pkgs.libseccomp}/include -L${pkgs.libseccomp}/lib \
              -lseccomp -Wl,-rpath,${pkgs.libseccomp}/lib \
              -o sandbox-rs-bin
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            cp sandbox-rs-bin "$out/bin/sandbox-rs"
            runHook postInstall
          '';

          meta.mainProgram = "sandbox-rs";
        };

      mkLandlockRs =
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
        in
        pkgs.stdenv.mkDerivation {
          pname = "virgl-guest-landlock-rs";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = ./landlock-rs.c;
          };

          nativeBuildInputs = [ pkgs.gcc ];
          # linux-headers provides linux/landlock.h for the ABI constants.
          buildInputs = [ pkgs.linuxHeaders ];

          buildPhase = ''
            runHook preBuild
            gcc -O2 -Wall landlock-rs.c \
              -I${pkgs.linuxHeaders}/include \
              -o landlock-rs-bin
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin"
            cp landlock-rs-bin "$out/bin/landlock-rs"
            runHook postInstall
          '';

          meta.mainProgram = "landlock-rs";
        };
    in
    {
      packages = forAllSystems (system: {
        libkrun = mkLibkrun system;
        # The firmware (`libkrunfw.so.5`) libkrun dlopens by soname at payload
        # load: the same pinned one the product boots, so this probe runs the
        # same guest kernel cang does.  Not an input of any package here (the
        # dlopen is by soname, not DT_NEEDED), so the run scripts put its `lib`
        # on LD_LIBRARY_PATH.
        libkrunfw = repo.packages.${system}.libkrunfw;
        guest-probe = mkGuestProbe system;
        guest-rootfs = mkGuestRootfs system;
        launcher = mkLauncher system;
        launcher-rs = mkLauncherRs system;
        sandbox-rs = mkSandboxRs system;
        landlock-rs = mkLandlockRs system;
        default = mkGuestRootfs system;
      });

      apps = forAllSystems (
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
          # `nix run .` runs run.sh out of this flake's own source tree: the
          # script builds `.#launcher`/`.#guest-rootfs` through the flake ref of
          # the directory it lives in, so a copy of it inside a package output
          # would resolve `#launcher` against that output instead.
          runGuestProbe = pkgs.writeShellScriptBin "run-guest-probe" ''
            exec ${pkgs.bash}/bin/bash ${self}/run.sh "$@"
          '';
        in
        {
          default = {
            type = "app";
            program = "${runGuestProbe}/bin/run-guest-probe";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = repo.inputs.nixpkgs.legacyPackages.${system};
          libkrun = self.packages.${system}.libkrun;
        in
        {
          default = pkgs.mkShell {
            buildInputs = [
              pkgs.gcc
              pkgs.vulkan-headers
              libkrun
              pkgs.vulkan-tools
              pkgs.virglrenderer
            ];
            shellHook = ''
              export RENDER_SERVER_EXEC_PATH=${pkgs.virglrenderer}/libexec/virgl_render_server
              export LD_LIBRARY_PATH=${libkrun}/lib:${pkgs.virglrenderer}/lib:"$LD_LIBRARY_PATH"
              echo "virgl guest probe dev shell"
              echo "  RENDER_SERVER_EXEC_PATH=$RENDER_SERVER_EXEC_PATH"
              echo "  Build the rootfs: nix build .#guest-rootfs"
              echo "  Boot + probe:     nix run ."
            '';
          };
        }
      );
    };
}
