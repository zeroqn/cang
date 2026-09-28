---
label: wayfinder:task
title: Retire the prebuilt-libkrun pipeline and re-derive the release asset
status: closed
blocked_by: ["06-retire-the-dlopen-path"]
claimed_by: pi session (2026-09-28)
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

## Resolution

**Bob's call: retire entirely, and keep the released asset a bare ELF whose
consumer supplies what it needs.** Landed in `038ef6f`.

Removed: `nix/pkgs/libkrun.nix`, `scripts/update-libkrun.sh`, the
`libkrunRelease` pin in `nix/pins.nix`, the `libkrun-loadable` check in
`flake.nix` and the `libkrun` package export. The release pin-gate in
`.github/workflows/publish_release.yml` is now
"Require a versioned libkrunfw pin" and loops over `libkrunfwRelease` alone.
The image's tooling layer swaps `libkrun` for **`libkrunfw`**
(`nix/image/layers.nix`): cang links libkrun, but the firmware is still opened
by soname, and a cang that runs inside the guest still needs it. The fork's own
`publish-cang-release.yml` is untouched - another repo's pipeline, still useful
to third-party C-ABI consumers.

Tests and docs follow the removal, including a new
`prebuilt_libkrun_pipeline_stays_retired` repository test (file absence plus the
`libkrunRelease`/`libkrun-loadable`/`packages.libkrun` strings - written with
`packages.libkrun;` rather than `packages.libkrun`, because the latter is a
prefix of `packages.libkrunfw`) and ADR 0005's amendment pointing at ADR 0008.

**The release asset.** Bob: keep it a bare neutral ELF and let consumers supply
`libvirglrenderer.so.1`, which is what `publish_release.yml` already documents
("not a standalone portable binary; Nix packaging patches ordinary ELF runtime
dependencies"). Two measured facts shaped that:

- After the release normalization (`patchelf --set-interpreter <neutral>
  --set-rpath ""`) the asset contains **0** `/nix/store/<hash>-` references, so
  the existing gate still passes; the pin-gate problem was the *unpatched* binary.
- The unpatched binary with an empty rpath cannot start at all
  (`libvirglrenderer.so.1: cannot open shared object file`), so
  `.#cang-prebuilt` had to gain `virglrenderer` in its `autoPatchelfHook` inputs
  - it had no such dependency before, because the old binary only `dlopen`ed
  libkrun through package-relative paths.

**Verified before the release exists**: `.#cang-prebuilt` was built against a
locally normalized copy of the v0.9.0 asset (temporarily pointed at it, since
the release is not published yet). `autoPatchelfHook` resolved the new
`libvirglrenderer.so.1` NEEDED, the wrapper plus `lib/cang/libkrunfw.so.5`
survived, `./result/bin/cang --version` printed `cang 0.9.0`, and a **live guest
boot through that package** printed `prebuilt-asset-boot-ok` /
`6.12.109-hardened1`, exit 0. What remains for bob is the tag itself,
`nix build .#cang-prebuilt` against the published asset, and comparing the
uploaded `.sha256` with the pinned hash.

Also worth recording from the release prep: the pinned hash must come from
`.#cang-ci-sccache`, not `.#cang`. Same source, same features, but with
`RUSTC_WRAPPER=sccache` the binary comes out un-LTO'd (10,068,000 bytes) while a
plain `.#cang` links with `lto = "thin"` (4,597,800 bytes); the hash is stable
across `SCCACHE_DIR` values, which is the only environment difference. Noted in
`docs/maintenance.md` with the local caveat that `SCCACHE_DIR` points at
`/nix/var/cache/sccache`, which only CI creates.
