# Neutral cang prebuilt release assets

Status: accepted

Cang release assets consumed by `.#cang-prebuilt` are neutral dynamic Linux
ELF payloads named `cang-<arch>-unknown-linux-gnu`. They are packaging inputs,
not standalone portable executables and not flake-locked Nix outputs.

The GitHub release workflow strips release-builder `/nix/store/<hash>-...`
interpreter and RPATH references before upload. The Nix package then uses
`autoPatchelfHook` to bind ordinary ELF dependencies to the consuming flake and
provides package-relative runtime tools plus `libkrun`/`libkrunfw` paths without
wrapping `bin/cang`.

## Context

The previous `loftd-<arch>-linux-flake-locked` asset embedded glibc and GCC
runtime paths from the release builder's `/nix/store`. `pkgs.fetchurl` consumes
that asset as fixed-output bytes, so Nix rejects the fetch for referring to other
store paths before package fixup can run.

Using `unsafeDiscardReferences` would hide the fixed-output reference check while
preserving the misleading release contract. The better long-term boundary is a
neutral upstream asset plus Nix-side patching in the package that consumes it.

## Decision

- Publish `cang-<arch>-unknown-linux-gnu` assets for cang prebuilts. Assets
  published before the 2026-09 cang rename carry the historical
  `loftd-<arch>-unknown-linux-gnu` name and stay resolvable under it.
- Reject legacy `loftd-<arch>-linux-flake-locked` pins before fetching them.
- Fail release and updater flows when a cang asset contains concrete
  `/nix/store/<hash>-...` references.
- Use `autoPatchelfHook` in `.#cang-prebuilt` for ordinary ELF runtime
  dependencies.
- Keep `libkrun` and `libkrunfw` runtime-loaded through package-relative
  lookup semantics instead of making them required ELF `NEEDED` dependencies.
  (Amended 2026-09: cang now links libkrun's Rust API into the binary, so libkrun
  is neither loaded nor a `NEEDED` edge, and only the firmware stays
  runtime-loaded. `libvirglrenderer.so.1` does become a `NEEDED` edge that the
  consumer supplies - see [ADR 0008](0008-cang-links-libkrun-rust-api.md).)

## Consequences

Existing legacy pins fail early until a new neutral `sha-*` release asset is
published and pinned.

Cutting a release that produces such an asset follows
[the cang release scheme](../maintenance.md#cang-release-scheme): the pin is
committed before the tag, and the tag points at the pin commit.

The raw GitHub asset is honest about its role: it is a neutral dynamic Linux ELF
for packaging. Ordinary users should prefer `nix build .#cang`,
`nix build .#cang-prebuilt`, or the published `ghcr.io/<repo-owner>/cang`
image.

## Considered options

- Keep flake-locked release assets: rejected because fixed-output fetches still
  see stale `/nix/store/<hash>-...` references and the public asset name
  promises the wrong contract.
- Use `unsafeDiscardReferences`: rejected because it bypasses the symptom instead
  of fixing the asset boundary.
- Promise a standalone portable dynamic `cang`: rejected because host cang is
  dynamically linked and also expects package-provided runtime tools and libkrun
  libraries.
