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

**Status (2026-09-30): done and published.** The 7.2.7 line is the fork's
`cang` branch (23 patches, down from 39), released as **`v5.6.2-cang.3`**
(x86_64) with the LTS line preserved on **`cang-lts`** and released as
**`v5.6.2-cang-lts.1`** (x86_64 + aarch64 + riscv64, and the only line carrying
the arm64 patches). The x86_64 KVM kernel boots a real cang guest (uname
`7.2.7-hardened1`, TSI, vsock, zram, virtiofs) and the **`-kvm-lto`** variant -
the asset cang's pin consumes - passes the live Chromium GPU smoke on venus with
that kernel. Evidence, the release layout and the traps are in
[notes/02](notes/02-two-kernel-lines-and-lto.md); the patch-level detail is in
[notes/01](notes/01-rebase-findings.md).

**Follow-up (2026-10-16): the line moved on to linux-7.2.10-hardened1**, the
next linux-hardened release, published as **`v5.6.2-cang.4`**. A patch-level bump
of the same line, not a new branch: one hunk needed re-authoring (linux-7.2.9
backported upstream's `1e3b08de` revert, so the guest-blob PRIME-import gate the
fork's patch 0023 extends had changed shape), the four x86_64 configs were
refreshed, and both live smokes pass on the pinned `-kvm-lto` bytes with the
7.2.7 baseline's VA-API numbers to the digit:
[notes/03](notes/03-rebase-7.2.10-and-the-lto-kernel.md).

## Notes

- [The 7.2.7 re-base: what dropped, what was re-authored, what runs](notes/01-rebase-findings.md)
- [Two kernel lines, the versioned releases, and the LTO kernel on the GPU smoke](notes/02-two-kernel-lines-and-lto.md)
- [The 7.2.10 follow-up: the upstream revert that landed, and the pinned LTO kernel](notes/03-rebase-7.2.10-and-the-lto-kernel.md)
- The procedure and its pitfalls are recorded in
  [docs/maintenance.md](../../maintenance.md#re-basing-the-libkrunfw-guest-kernel),
  next to the fork-update procedure it belongs to.

## Decided

- **Land it, with two lines.** `cang` is the newest kernel line and cang's
  default (flake `libkrunfw-src` input, primary `libkrunfwRelease` tag);
  `cang-lts` keeps the LTS kernel and the arm64 patches. Each line publishes its
  own permanent release and its own rolling dev prefix.
- **Drop the arm64 patches from the newest line.** They are maintained only on
  `cang-lts`; `release-arches` on each branch states what that line publishes.
- **Version semantics stay upstream's.** The counter is per line: the newest
  line continues `v5.6.2-cang.<n>` and the LTS line starts `v5.6.2-cang-lts.<n>`.
- **Refresh the LTO configs and smoke them.** All four x86_64 configs are built
  and refreshed; the LTO variant is the one the smoke ran on.
- **A patch-level kernel bump rides the same line.** linux-hardened cutting a
  later stable series (`v7.2.10-hardened1`) re-bases the `cang` branch and takes
  the next `v5.6.2-cang.<n>`; it is not a new branch, and the LTS line is not
  dragged along.
