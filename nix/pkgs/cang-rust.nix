{
  self,
  pkgs,
  pins,
  libkrun ? null,
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
  # Keep $out/bin/cang a raw ELF. The release workflow publishes it as the
  # neutral asset and refuses a wrapper script, and a wrapper would also hide
  # the helper/library lookup below behind shell setup. Runtime tools resolve
  # from $out/libexec/cang-helpers and libkrun from $out/lib/cang.
  rustPackage = pkgs.rustPlatform.buildRustPackage (
    {
      pname = "cang";
      version = cangVersion;
      src = self;

      nativeBuildInputs = ciSccacheNativeBuildInputs;

      cargoLock = {
        lockFile = ../../Cargo.lock;
      };

      postInstall = ''
        mkdir -p "$out/libexec/cang-helpers" "$out/lib/cang"
        install -Dm644 ${self}/crates/cang/assets/seccomp/default.json "$out/share/cang/seccomp/default.json"
        install -Dm644 ${self}/crates/cang/assets/seccomp/render-server.json "$out/share/cang/seccomp/render-server.json"
        ln -s ${pkgs.buildah}/bin/buildah "$out/libexec/cang-helpers/buildah"
        ln -s ${pkgs.btrfs-progs}/bin/btrfs "$out/libexec/cang-helpers/btrfs"
        ln -s ${pkgs.btrfs-progs}/bin/mkfs.btrfs "$out/libexec/cang-helpers/mkfs.btrfs"
        ln -s ${pkgs.util-linux}/bin/blkid "$out/libexec/cang-helpers/blkid"
        ln -s ${pkgs.passt}/bin/pasta "$out/libexec/cang-helpers/pasta"
        ln -s ${pkgs.passt}/bin/passt "$out/libexec/cang-helpers/passt"
        ln -s ${pkgs.strace}/bin/strace "$out/libexec/cang-helpers/strace"
        ln -s ${pkgs.virglrenderer}/libexec/virgl_render_server "$out/libexec/cang-helpers/virgl_render_server"
        ${pkgs.lib.optionalString (libkrun != null) ''
          for library in ${pkgs.lib.getLib libkrun}/lib/libkrun.so* \
            ${pkgs.lib.getLib libkrun}/lib/libkrun_init.so*; do
            [ -e "$library" ] || continue
            ln -s "$library" "$out/lib/cang/$(basename "$library")"
          done
        ''}
        ${pkgs.lib.optionalString (libkrunfw != null) ''
          for library in ${pkgs.lib.getLib libkrunfw}/lib/libkrunfw.so*; do
            ln -s "$library" "$out/lib/cang/$(basename "$library")"
          done
        ''}
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
      src = self;

      nativeBuildInputs = ciSccacheNativeBuildInputs;

      cargoLock = {
        lockFile = ../../Cargo.lock;
      };

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
