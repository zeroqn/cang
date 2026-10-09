{
  # cang's workspace tree with the fork checkouts grafted into `deps/` (see
  # nix/pkgs/workspace-src.nix).
  src,
  pkgs,
  pins,
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
  cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
    inherit src;
    hash = "sha256-goNlKHgpdohF0Mr3E3ZepFIS2tdR2woy+2dA+/rxcVE=";
  };

  # Building libkrun is what needs clang/libclang (krun-display and krun-input
  # run bindgen), pkg-config plus virglrenderer and gbm (rutabaga_gfx's
  # virgl_renderer feature), and rustfmt (ffier's client generator shells out to
  # it). `krunInitBinary` is the musl guest init the embedded init blob needs.
  libkrunNativeBuildInputs = [
    pkgs.rustPlatform.bindgenHook
    pkgs.pkg-config
    pkgs.rustfmt
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
  rustPackage = pkgs.rustPlatform.buildRustPackage (
    {
      pname = "cang";
      version = cangVersion;
      inherit src;

      inherit cargoDeps;

      nativeBuildInputs = ciSccacheNativeBuildInputs ++ libkrunNativeBuildInputs;
      buildInputs = libkrunBuildInputs;
      env = libkrunEnv;

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
      postFixup = ''
        patchelf --add-rpath '$ORIGIN/../lib/cang' "$out/bin/cang"
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
