# The guest's and the dev shell's Rust linker: `wild` driven through clang.
#
# Both the cang image and `nix develop` point
# `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` at this wrapper, so a link in a
# guest and a link in the dev shell go through the same driver and the same
# linker. The C compiler stays the stdenv's `clang`, so the wrapper behaves like
# the stdenv's `cc` apart from the linker, and the wrapper stays a compiler
# driver so a caller's own Cargo linker configuration still layers on top.
#
# The `-fuse-ld` goes *after* the caller's arguments on purpose. rustc enables
# its `lld` linker feature by default and appends `-fuse-ld=lld` itself, clang
# honours the last `-fuse-ld` it is given, and a wrapper that put the flag first
# would therefore link with the toolchain's `rust-lld` without saying so.
# Measured on this repository, wild links the `cang` debug binary in 0.38 s
# where rust-lld needs 0.65 s.
#
# `wild` has to be `wild-unwrapped`: `pkgs.wild` is the same binary behind
# nixpkgs' bintools wrapper, whose flag expansion costs about a second on every
# single link (1.26 s against 0.14 s for a link with no inputs, and a cold
# `cargo build --workspace --all-targets` spends 233 s linking rather than 20 s).
{ pkgs, wild }:
pkgs.writeShellScriptBin "clang_wild_wrapper" ''
  exec ${pkgs.clang}/bin/clang "$@" -fuse-ld=${wild}/bin/wild
''
