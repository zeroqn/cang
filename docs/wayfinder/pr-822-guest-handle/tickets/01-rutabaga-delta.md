---
label: wayfinder:research
title: What does the ported VMM half still need from rutabaga_gfx?
status: open
blocked_by: []
claimed_by: pi research child-1 (2026-09-28)
---

## Question

The fork's `src/devices/Cargo.toml` depends on crates.io `rutabaga_gfx 0.1.85`,
which predates the guest-blob-handle work.
[magma-gpu/rutabaga_gfx#81](https://github.com/magma-gpu/rutabaga_gfx/pull/81)
merged a *context*-routed version of that work on 2026-09-02 and is
**unreleased** (latest crates.io is still `0.1.85`, 2026-08-06).

Bob has decided the source is a **rev-pinned git dependency** on
`magma-gpu/rutabaga_gfx`. This ticket answers what that dependency has to
contain, and what the fork must add on top.

1. **The remaining delta.** PR 822's own rutabaga commits are `a8fd2784`
   (`CrossDomainContext::attach` keeps handles), `ec9e5562`
   (`RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE = 0x0008`), `3391f3c5` (the
   CrossDomain **component**'s `create_blob` keeps `handle_opt`), and `16e116ae`
   (`rutabaga_core` routes `ctx_id == 0` + flag to the CrossDomain component).
   Two of those are demonstrably absent from main today: no such constant in
   `src/rutabaga_utils.rs`, and `src/cross_domain/component.rs::create_blob`
   still takes `_handle_opt` and returns `handle: None`. Enumerate, hunk by
   hunk, exactly what is still missing (both for 822's component-routing design
   and for upstream's `ctx_id`-routing design), and where each hunk could live:
   a `[patch]`/`[replace]` in libkrun's `Cargo.toml`, a fork-local vendored
   patch, or a change to libkrun's own device code.
2. **The delivery shape that actually builds.** A rev-pinned `git =` dependency
   with a stable `Cargo.lock` rev; what that does to
   `rustPlatform.fetchCargoVendor`'s hash in `nix/pkgs/libkrun-source.nix`
   (does it vendor git deps at all?); and what the fork's release workflow
   (`publish-cang-release.yml`, `FFI=1`) needs. `third_party/mesa3d` in
   rutabaga is a plain tree, not a submodule - confirm that a plain git dep
   therefore resolves the `magma-gpu` path dependency.
3. **The symbol-rename table.** Main's API has moved on since 822's base:
   `RUTABAGA_MEM_HANDLE_TYPE_DMABUF` -> `RUTABAGA_HANDLE_TYPE_MEM_DMABUF`,
   `RUTABAGA_CHANNEL_TYPE_*` -> `RUTABAGA_PATH_TYPE_*`, `RutabagaHandle` moved
   to `src/handle.rs`, and the crate structure changed (`cross_domain/*.rs`).
   List every symbol 822's device hunks name that no longer exists, so the port
   in ticket 07 is mechanical.

## Deliverable

`notes/01-rutabaga-delta.md`: the remaining delta per design, the delivery shape
with its nix/release consequences, and the symbol-rename table.
