{
  # cang's workspace tree with the fork checkouts grafted into `deps/` (see
  # nix/pkgs/workspace-src.nix).
  src,
  pkgs,
  pins,
  # The pinned Rust toolchain (`nix/lib/rust-toolchain.nix`): the same rustc,
  # cargo and rustfmt the dev shell and the guest image use.
  rustToolchain,
  krunInitBinary,
  libkrunfw ? null,
  enableCiSccache ? false,
}:
let
  cangVersion = pins.cangVersion;
  ciSccacheNativeBuildInputs = pkgs.lib.optionals enableCiSccache [ pkgs.sccache ];
  ciSccacheEnv = pkgs.lib.optionalAttrs enableCiSccache {
    RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";
    SCCACHE_DIR = "/nix/var/cache/sccache";
    SCCACHE_IGNORE_SERVER_IO_ERROR = "1";
  };
  # cang links libkrun's Rust API (crates/cang-libkrun), so libkrun's crates are
  # part of cang's lock and have to be vendored. The graph's packages that do not
  # come from crates.io are path dependencies on the `deps/` checkouts (libkrun,
  # plus `rutabaga_gfx` and its `magma-gpu` through the workspace `[patch]`), so
  # the vendored set is what is left over from the registry, produced by
  # `fetchCargoVendor` running `cargo vendor` over this tree. A libkrun or
  # rutabaga bump refreshes this hash in the same commit as the submodule
  # pointers and the matching flake inputs.
  #
  # This deliberately stays on nixpkgs' `rustPlatform` and not the pinned
  # toolchain's: the hash is a function of the lock file and the crate sources,
  # not of the compiler, so vendoring with a fixed cargo keeps a toolchain bump
  # (`nix flake update fenix`) from re-hashing it.
  cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
    inherit src;
    hash = "sha256-WxsAGPFoCMs8g7Rb46pNQT4HFLjqNx+fhjyGbe2grrs=";
  };

  # Building libkrun is what needs clang/libclang (krun-display and krun-input
  # run bindgen), pkg-config plus virglrenderer and gbm (rutabaga_gfx's
  # virgl_renderer feature), and rustfmt (ffier's client generator shells out to
  # it). `krunInitBinary` is the musl guest init the embedded init blob needs.
  libkrunNativeBuildInputs = [
    pkgs.rustPlatform.bindgenHook
    pkgs.pkg-config
    # ffier's client generator shells out to `rustfmt`, which has to be the
    # pinned toolchain's (formatting is version-dependent).
    rustToolchain.toolchain
    pkgs.patchelf
  ];
  libkrunBuildInputs = [
    pkgs.virglrenderer
    pkgs.libgbm
  ];
  libkrunEnv = {
    KRUN_INIT_BINARY_PATH = "${krunInitBinary}/bin/krun-init";
  };

  # Keep $out/bin/cang a raw ELF. The release workflow publishes it as the
  # neutral asset and refuses a wrapper script, and a wrapper would also hide
  # the helper/library lookup below behind shell setup. Runtime tools resolve
  # from $out/libexec/cang-helpers, and the firmware (`libkrunfw.so.5`, opened by
  # libkrun itself) from $out/lib/cang.
  rustPackage = rustToolchain.rustPlatform.buildRustPackage (
    {
      pname = "cang";
      version = cangVersion;
      inherit src;

      inherit cargoDeps;

      nativeBuildInputs = ciSccacheNativeBuildInputs ++ libkrunNativeBuildInputs;
      buildInputs = libkrunBuildInputs;
      env = libkrunEnv;

      # `cargoCheckHook` runs the test binaries, and cang links libkrun and
      # therefore `libvirglrenderer.so.1`, which a build shell does not put on
      # the loader path. Scoped to the check phase on purpose: a global
      # `LD_LIBRARY_PATH` changes what `patchelf --shrink-rpath` does in
      # `fixupPhase` and strips the run paths the installed binary needs.
      preCheck = ''
        export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath libkrunBuildInputs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      '';

      postInstall = ''
        mkdir -p "$out/libexec/cang-helpers" "$out/lib/cang"
        install -Dm644 ${src}/crates/cang/assets/seccomp/default.json "$out/share/cang/seccomp/default.json"
        install -Dm644 ${src}/crates/cang/assets/seccomp/render-server.json "$out/share/cang/seccomp/render-server.json"
        ln -s ${pkgs.buildah}/bin/buildah "$out/libexec/cang-helpers/buildah"
        ln -s ${pkgs.btrfs-progs}/bin/btrfs "$out/libexec/cang-helpers/btrfs"
        ln -s ${pkgs.btrfs-progs}/bin/mkfs.btrfs "$out/libexec/cang-helpers/mkfs.btrfs"
        ln -s ${pkgs.util-linux}/bin/blkid "$out/libexec/cang-helpers/blkid"
        ln -s ${pkgs.passt}/bin/pasta "$out/libexec/cang-helpers/pasta"
        ln -s ${pkgs.passt}/bin/passt "$out/libexec/cang-helpers/passt"
        ln -s ${pkgs.strace}/bin/strace "$out/libexec/cang-helpers/strace"
        ln -s ${pkgs.virglrenderer}/libexec/virgl_render_server "$out/libexec/cang-helpers/virgl_render_server"
        ${pkgs.lib.optionalString (libkrunfw != null) ''
          for library in ${pkgs.lib.getLib libkrunfw}/lib/libkrunfw.so*; do
            ln -s "$library" "$out/lib/cang/$(basename "$library")"
          done
        ''}
      '';
      # libkrun opens the firmware (`libkrunfw.so.5`) by soname, from the cang
      # process itself. That used to resolve through the dlopen'd libkrun.so's
      # `$ORIGIN` runpath; now the binary carries a package-relative rpath for it,
      # which also keeps `$out/bin/cang` runnable without a wrapper.
      #
      # The second run path is for `libvirglrenderer.so.1`, which the binary
      # needs because it links libkrun (whose GPU device links vrend). The
      # pinned toolchain's link step does not leave the linker's search
      # directories in the output, so the package states them: without them the
      # loader cannot start `$out/bin/cang` outside a shell that already has the
      # store on `LD_LIBRARY_PATH`. `libvirglrenderer.so.1` carries its own run
      # path for everything below it.
      postFixup = ''
        patchelf --add-rpath '$ORIGIN/../lib/cang' "$out/bin/cang"
        patchelf --add-rpath '${pkgs.lib.makeLibraryPath libkrunBuildInputs}' "$out/bin/cang"
      '';
    }
    // ciSccacheEnv
  );

  muslTarget =
    if pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
      "x86_64-unknown-linux-musl"
    else if pkgs.stdenv.hostPlatform.system == "aarch64-linux" then
      "aarch64-unknown-linux-musl"
    else
      throw "cang-musl is only supported on Linux";

  # The musl guest init stays on `pkgsStatic`'s rustPlatform rather than the
  # pinned toolchain: it builds with `CARGO_BUILD_TARGET` on a musl target, and
  # the static package set is what carries a rustc and std for it. Nothing it
  # compiles is shared with the host crate graph beyond the protocol crates.
  cangMuslPackage = pkgs.pkgsStatic.rustPlatform.buildRustPackage (
    {
      pname = "cang";
      version = cangVersion;
      inherit src;

      # The workspace lock now resolves libkrun's crates too, so the musl build
      # needs the same vendored registry for resolution even though it compiles
      # only `cang-guest-init`.
      inherit cargoDeps;

      nativeBuildInputs = ciSccacheNativeBuildInputs;

      CARGO_BUILD_TARGET = muslTarget;
      cargoBuildFlags = [
        "--package"
        "cang-guest-init"
      ];
      cargoTestFlags = [
        "--package"
        "cang-guest-init"
      ];
    }
    // ciSccacheEnv
  );
in
{
  inherit rustPackage cangMuslPackage;
}
