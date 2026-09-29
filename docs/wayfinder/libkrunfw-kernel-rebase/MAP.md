---
label: wayfinder:map
title: libkrunfw guest kernel onto linux-hardened v7.2.7
---

## Destination

`deps/libkrunfw` (the `zeroqn/libkrunfw` fork) bundles a guest kernel that is
**linux-7.2.7 + linux-hardened v7.2.7-hardened1** instead of 6.12.109, with the
fork's patch series re-authored for the 7.x tree, a refreshed x86_64 config, a
published fork release `v5.6.2-cang.3` and its `libkrunfwRelease` pin - and a
live cang guest on it (uname `7.2.7-hardened1`, TSI, vsock, zram, virtiofs).

**Status (2026-09-30): the x86_64 half is done and verified, nothing is
published.** The re-base lives on the fork branch **`rebase-7.2.7`** (`694c1f6`,
pushed) - 23 patches, down from 39. The x86_64 KVM kernel builds from it and the
resulting `libkrunfw.so` boots a real cang guest (`notes/01-rebase-findings.md`
has the evidence, the dropped patches, the bit-5 trap and the exact commands).
`deps/libkrunfw`'s `cang` branch is untouched, so in-tree and Nix builds still use
6.12.109.

## Notes

- [The 7.2.7 re-base: what dropped, what was re-authored, what runs](notes/01-rebase-findings.md)
- The procedure and its pitfalls are recorded in
  [docs/maintenance.md](../../maintenance.md#re-basing-the-libkrunfw-guest-kernel),
  next to the fork-update procedure it belongs to.

## Not yet specified

- **Whether to land it.** Landing means moving the fork's `cang` branch to
  `rebase-7.2.7`, `nix flake update libkrunfw-src`, and re-pinning
  `libkrunfwRelease` at a published `v5.6.2-cang.3`. The blocker is not code but
  appetite: 7.2.7 is a stable-line release, not an LTS like 6.12.
- **The aarch64 half.** The four arm64 patches (SCOPE_LOCAL_CPU unification,
  arm64 PR_{GET,SET}_MEM_MODEL, ACTLR_EL1 threading, Apple IMPDEF TSO) are not
  ported, so `build-aarch64`/`build-macos` cannot build from this tree yet.
- **The LTO config.** `config-libkrunfw_x86_64-kvm-lto` (the config behind the
  released `libkrunfw-x86_64-kvm-lto.tgz` asset) is still the 6.12.91-era file;
  only `-kvm` was refreshed by the 7.2.7 build.
- **Whether the released asset should stay on 7.2.x at all**, and whether the
  fork should track an LTS line (6.12/6.18) instead: the hardened patch exists
  for whichever base the fork chooses.
