{ pkgs, krunInitBinary, libkrunfw }:

pkgs.mkShell {
  # Building cang now compiles libkrun's Rust API out of `deps/libkrun`, which
  # needs clang/libclang through the bindgen hook, pkg-config plus virglrenderer
  # and gbm for rutabaga_gfx, and rustfmt for ffier's client generator.
  nativeBuildInputs = [
    pkgs.rustPlatform.bindgenHook
    pkgs.pkg-config
    pkgs.rustfmt
    pkgs.cargo
    pkgs.clippy
    pkgs.rustc
  ];

  buildInputs = [
    pkgs.virglrenderer
    pkgs.libgbm
    # cang opens the firmware (`libkrunfw.so.5`) by soname at run time; a
    # buildInput puts its lib directory on LD_LIBRARY_PATH, which is what a
    # `cargo build` binary needs (the packaged binary carries an rpath instead).
    libkrunfw
  ];

  packages = [
    pkgs.btrfs-progs
    pkgs.buildah
    pkgs.cargo-deny
    pkgs.curl
    pkgs.fish
    pkgs.fuse-overlayfs
    pkgs.jq
    pkgs.passt
    pkgs.podman
    pkgs.python3
    pkgs.starship
    pkgs.strace
    pkgs.util-linux
  ];

  # The blob embeds this instead of cross-building it; the host toolchain has no
  # musl rust std.
  KRUN_INIT_BINARY_PATH = "${krunInitBinary}/bin/krun-init";

  shellHook = ''
    export SHELL=${pkgs.fish}/bin/fish

    if [ -z "''${CANG_DISABLE_AUTO_FISH-}" ] && [ -t 0 ] && [ -t 1 ] && [ -z "''${CANG_IN_AUTO_FISH-}" ]; then
      export CANG_IN_AUTO_FISH=1
      exec ${pkgs.fish}/bin/fish -i -C 'starship init fish | source'
    fi
  '';
}
