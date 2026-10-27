# The guest's and the dev shell's Rust linker: `wild` driven through clang.
#
# Both the cang image and `nix develop` point
# `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` at this wrapper, so a link in a
# guest and a link in the dev shell go through the same driver and the same
# linker. `wild` comes from nixpkgs-unstable (with the Rust zg and cargo-deny);
# the C compiler stays the 26.05 one, so the wrapper behaves like the stdenv's
# `cc` apart from the linker. The binary is pinned by path rather than resolved
# as `ld.wild` through PATH, and the wrapper stays a compiler driver so a
# caller's own Cargo linker configuration still layers on top.
{ pkgs, wild }:
pkgs.writeShellScriptBin "clang_wild_wrapper" ''
  exec ${pkgs.clang}/bin/clang -fuse-ld=${wild}/bin/wild "$@"
''
