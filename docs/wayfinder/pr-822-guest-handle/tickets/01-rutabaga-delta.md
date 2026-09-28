---
label: wayfinder:research
title: What does the ported VMM half still need from rutabaga_gfx?
status: closed
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

## Resolution (2026-09-28, pi research child-1)

**Under the design rutabaga actually merged, the ported VMM half needs *nothing*
from rutabaga beyond a rev-pinned git dependency.** Deliverable:
`../notes/01-rutabaga-delta.md` + `01-raw-rutabaga-hunks-and-main.txt`.

- Of 822's four rutabaga commits, `a8fd2784` (`attach` keeps handles) is
  **already in main** (reworked, `cross_domain/context.rs:809`); the other three
  (`ec9e5562` the constant, `3391f3c5` component `create_blob`, `16e116ae`
  `ctx_id == 0` routing) are absent - but **main deliberately superseded them**.
  Val, undrafting PR #81 on 2026-08-28: *"I have it all working now without going
  through the component-level create_blob ... it doesn't rely on any new
  constants anymore."* Main's `context_create_blob` already has Case 2
  (`blob_id 0` + `BLOB_MEM_GUEST` + `Some(handle)`) and `attach` keeps the handle.
- What makes that context route reachable is the **guest kernel**, not rutabaga:
  `VIRTIO_GPU_F_BLOB_CTX_ID_FIX (7)` is exactly "it is always safe to pass the
  current ctx_id for guest-only blobs". Ticket 02 already carries bit 7, so the
  fork can adopt it.
- **Therefore: design B (ctx_id route) costs zero rutabaga patching and drops
  three 822 hunks; design A (822 verbatim, component route) needs a second fork
  repo (`zeroqn/rutabaga_gfx`) or a `[patch]`-ed vendored tree (~2.3 MB), because
  the three hunks live *inside* rutabaga and cannot be reached from libkrun's
  device code - `Rutabaga` exposes only `resource_create_blob`, components are
  private, and `default_component` cannot become CrossDomain (venus needs
  VirglRenderer).** Recommendation: **B**.
- **Precondition worth carrying into ticket 04/09:** the ctx the guest names must
  be the **CrossDomain** context, or main falls through to the default component
  whose `create_blob` takes `_handle_opt` and silently drops the handle.
- **Delivery shape:** `src/devices/Cargo.toml` has the dep twice (lines 45, 57) -
  `git = "https://github.com/magma-gpu/rutabaga_gfx", rev = "<full sha>"`.
  `Cargo.lock` then records both `rutabaga_gfx` and the `magma-gpu` path dep with
  `git+...#<sha>` and no checksum. `fetchCargoVendor` **does** vendor git deps
  (proven in-repo by `ffier` and its path-dep crates), so **two** hashes move:
  `nix/pkgs/libkrun-source.nix` `libkrunCargoDeps` **and**
  `nix/pkgs/cang-rust.nix` `cargoDeps` - both `Cargo.lock`s too, because CI runs
  `cargo clippy --locked`. `publish-cang-release.yml` needs no change (no
  `--locked`/`--offline` in `Makefile`).
- **Rename table = a checklist for ticket 07, not open work:** most churn was
  already applied by the ABI-2 port. The one real edit is
  `RutabagaHandle::from(RutabagaMagmaHandle { .. })` (it is an enum now), and
  `RUTABAGA_HANDLE_TYPE_MEM_SHM` not being re-exported is a pre-existing latent
  break in the `virgl_resource_map2` path cang never compiles.
- No `rutabaga_gfx` release newer than 0.1.85 exists and there is no upstream
  release PR/issue (open: #88, #89 - neither release-related), so the git dep is
  unavoidable for now.
