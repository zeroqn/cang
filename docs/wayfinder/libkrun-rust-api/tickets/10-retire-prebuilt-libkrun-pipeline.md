---
label: wayfinder:task
title: Retire the prebuilt-libkrun pipeline and re-derive the release asset
status: open
blocked_by: ["06-retire-the-dlopen-path"]
claimed_by:
---

## Question

With cang compiling libkrun itself, the prebuilt-libkrun pipeline has no cang
consumer left. Decide what happens to each part:

- `nix/pins.nix`'s `libkrunRelease` (the fork's `libkrun-<arch>-linux-full.tgz`
  pin + hashes) and `nix/pkgs/libkrun.nix` (the `fetchurl` + `patchelf` rpath
  derivation): does cang drop them entirely, or do the container image or other
  consumers still want the C-ABI `.so`? Note the image currently installs
  libkrun (`nix/image/layers.nix`, `nix/image/container.nix`).
- The fork's own `publish-cang-release.yml` (in `deps/libkrun`): it keeps
  building `FFI=1` prebuilts with attestations for third parties - decide
  whether that stays as-is (it is another repo's pipeline; bob owns the push).
- `scripts/update-libkrun.sh` and the pin-gate in `.github/workflows/publish_release.yml`
  ("refuse a tagged cang release while pins are rolling") - the gate exists to
  keep published releases from pointing at pruned fork artifacts; if `pins.nix`
  no longer holds a rolling libkrun pin, that gate either retires or switches to
  the submodule pointer.
- The release asset itself: `publish_release.yml` publishes a *neutral* ELF with
  the interpreter neutralised and `rpath` stripped, asserting no
  `/nix/store/<hash>-` strings. With libkrun static, the binary gains real
  `DT_NEEDED` entries (libvirglrenderer at least) and is far larger - decide
  whether "neutral" still describes anything usable, and whether the release
  notes' "not a standalone portable binary" wording carries it.
- `crates/cang-repository-tests` asserts the pins and asset naming
  (`repository.rs` around the `libkrunRelease` loop) - update or delete with the
  decision.

Done when: no cang build path reads a prebuilt libkrun, every pinned artifact
that remains has a stated consumer, and the release workflow's assertions match
what the asset now is.
