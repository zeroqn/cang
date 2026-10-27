{
  pkgs,
  rustToolchain,
  # nixpkgs-unstable's cargo-deny: the advisory database and lock-file parsing
  # move with the Rust ecosystem, and 26.05's is behind.
  cargoDeny,
  # nixpkgs-unstable's `wild`, the Rust linker the image also links with.
  wild,
  krunInitBinary,
  libkrunfw,
}:

let
  # The same wrapper the image presets as the guest's Rust linker, so a `cargo
  # build` here and a `cargo build` in a cang guest link with wild identically.
  clangWildWrapper = import ../lib/clang-wild-wrapper.nix { inherit pkgs wild; };
in
pkgs.mkShell {
  # Building cang now compiles libkrun's Rust API out of `deps/libkrun`, which
  # needs clang/libclang through the bindgen hook, pkg-config plus virglrenderer
  # and gbm for rutabaga_gfx, and rustfmt for ffier's client generator.
  #
  # rustc, cargo, rustfmt and clippy are the pinned fenix toolchain
  # (`nix/lib/rust-toolchain.nix`) - the same one `nix build .#cang` and the
  # guest image use - and not nixpkgs' 1.95. The `shellHook` below puts it first
  # on PATH and refuses a shell where anything else answers for it.
  nativeBuildInputs = [
    pkgs.rustPlatform.bindgenHook
    pkgs.pkg-config
    rustToolchain.toolchain
    clangWildWrapper
    wild
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
    cargoDeny
    pkgs.curl
    pkgs.fish
    pkgs.fuse-overlayfs
    pkgs.jq
    pkgs.just
    pkgs.nixfmt
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

  # `CARGO_TARGET_<TRIPLE>_LINKER` is how Cargo takes a linker for a target, and
  # the image sets the same variable for the guest. Cargo reads it from the
  # environment, so it covers `cargo build`, `cargo test` and the crates' own
  # build scripts, and no `RUSTFLAGS` are involved - a caller's Cargo config can
  # still layer on top.
  "CARGO_TARGET_${
    pkgs.lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] pkgs.stdenv.hostPlatform.rust.rustcTarget)
  }_LINKER" =
    "${clangWildWrapper}/bin/clang_wild_wrapper";

  shellHook = ''
    # The toolchain check. `nix develop` inherits the caller's PATH, so a
    # rustup shim, a `~/.cargo/bin` or another profile's rustc can sit ahead of
    # this shell's; putting the pinned toolchain first is not enough on its own,
    # because the point of the check is that a shell which *did* end up with a
    # different compiler fails loudly instead of silently building cang (and
    # passing CI) with it. Every rust binary this shell is used through - `cargo
    # build`, `cargo test`, `cargo clippy`, `cargo fmt`, `cargo deny`, and the
    # `nix develop --command ...` invocations the workflows run - resolves
    # through these.
    export PATH=${rustToolchain.toolchain}/bin:$PATH

    cang_devshell_tool="${rustToolchain.toolchain}/bin"
    cang_devshell_require_tool() {
      cang_tool="$1"
      shift
      cang_want="$("$cang_devshell_tool/$cang_tool" "$@")" || {
        echo "cang dev shell: the pinned toolchain has no working $cang_tool" >&2
        exit 1
      }
      cang_got="$("$cang_tool" "$@" 2>/dev/null)" || cang_got=""
      if [ "$cang_got" != "$cang_want" ]; then
        echo "cang dev shell: refusing to start with a $cang_tool that is not the pinned toolchain" >&2
        echo "  expected: $cang_want" >&2
        echo "  got:      ''${cang_got:-<none>}" >&2
        echo "  PATH:     $PATH" >&2
        echo "  fix: leave the shell and re-enter it (or unset RUSTC/RUSTUP_TOOLCHAIN), so ${rustToolchain.toolchain}/bin comes first" >&2
        exit 1
      fi
    }
    cang_devshell_require_tool rustc -vV
    cang_devshell_require_tool cargo -V
    cang_devshell_require_tool rustfmt -V
    cang_devshell_require_tool clippy-driver -V
    # cargo prefers `RUSTC` over PATH, so an exported one has to agree too.
    if [ -n "''${RUSTC:-}" ] && [ "$RUSTC" != "$cang_devshell_tool/rustc" ]; then
      echo "cang dev shell: RUSTC points at $RUSTC, not the pinned toolchain" >&2
      echo "  fix: unset RUSTC (or set it to $cang_devshell_tool/rustc)" >&2
      exit 1
    fi

    # Nix's develop shell does not export LD_LIBRARY_PATH for the shell's build
    # inputs, so a binary that DT_NEEDs a store library - cang's unit tests need
    # `libvirglrenderer.so.1`, which is a `buildInput` - cannot be loaded
    # straight from `nix develop --command cargo test`. Recompute it from the
    # same inputs the shell links against; a caller's own value stays behind it.
    export LD_LIBRARY_PATH="${
      pkgs.lib.makeLibraryPath [
        pkgs.virglrenderer
        pkgs.libgbm
        libkrunfw
      ]
    }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

    export SHELL=${pkgs.fish}/bin/fish

    if [ -z "''${CANG_DISABLE_AUTO_FISH-}" ] && [ -t 0 ] && [ -t 1 ] && [ -z "''${CANG_IN_AUTO_FISH-}" ]; then
      export CANG_IN_AUTO_FISH=1
      exec ${pkgs.fish}/bin/fish -i -C 'starship init fish | source'
    fi
  '';
}
