# The workspace source a cang build compiles: cang's own tree with the fork
# checkouts grafted into `deps/`.
#
# `crates/cang-libkrun` depends on libkrun by path inside `deps/libkrun` (cang
# links libkrun's Rust API, so libkrun has to be compiled by cang's own rustc),
# and `deps/libkrunfw` is the fork checkout a local kernel build reads. Both are
# Git submodules, and a flake's own source cannot carry submodule contents:
# `inputs.self.submodules = true` does not either - Nix records
# `submodules = true` in the ref a downstream `flake.lock` writes for `github:`
# flakes and that scheme then rejects it with "input attribute 'submodules' not
# supported by scheme 'github'" (NixOS/nix#13571), while the flake's own
# `self.outPath` stays the submodule-less tree. The fork revisions therefore
# arrive as flake inputs and are copied in here, so a build compiles the same
# source from any flake ref - a local checkout, CI, or `github:zeroqn/cang`.
#
# A local fork edit needs no commit, only a different input ref:
#
#   nix build .#cang --override-input libkrun-src "git+file://$PWD/deps/libkrun"
#
# (`git+file:` copies the tracked files plus uncommitted changes; `path:` would
# copy the whole directory, build output included.)
{
  pkgs,
  src,
  libkrunSrc,
  libkrunfwSrc,
}:
pkgs.runCommand "cang-workspace-source"
  {
    # This is a source tree, not a package: keep what fixupPhase does to a
    # package's output (shebang rewriting and the like) away from it.
    dontFixup = true;
  }
  ''
    cp -r --no-preserve=mode,ownership ${src}/. $out/
    rm -rf $out/deps/libkrun $out/deps/libkrunfw
    cp -r --no-preserve=mode,ownership ${libkrunSrc} $out/deps/libkrun
    cp -r --no-preserve=mode,ownership ${libkrunfwSrc} $out/deps/libkrunfw
    chmod -R u+w $out
  ''
