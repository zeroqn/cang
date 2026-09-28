---
label: wayfinder:note
ticket: 01-rutabaga-delta
title: What does the ported VMM half still need from rutabaga_gfx?
status: closed
resolved: 2026-09-28
---

# 01 - What does the ported VMM half still need from rutabaga_gfx?

Question: PR 822's VMM half is to be carried against a **rev-pinned
`magma-gpu/rutabaga_gfx` git dependency**. What must that dependency contain, and
what must the fork add on top?

## Verdict (short)

1. **Under the design rutabaga actually merged (PR #81, `ec60ee11`) the ported
   VMM half needs *nothing* from rutabaga beyond a rev-pinned git dependency on
   main.** The guest-blob-with-handle path that 822 implements in rutabaga's
   *component* was re-implemented upstream in the CrossDomain **context**;
   the difference is absorbed by a **guest-kernel** convention
   (`VIRTIO_GPU_F_BLOB_CTX_ID_FIX`, spec bit 7: plumb the current `ctx_id` for
   guest-only blobs). If the fork's libkrunfw patch set negotiates bit 7 and
   passes `ctx_id`, PR 822's three rutabaga hunks are *dropped, not carried*.
2. **If the fork instead carries 822 verbatim** (`ctx_id == 0` + flag routed to
   the CrossDomain *component*), then main is **not** enough: three hunks are
   missing and they live inside rutabaga, where libkrun's device code cannot
   reach. They must then be delivered by a **`zeroqn/rutabaga_gfx` fork pinned
   by rev**, or a `[patch]`-ed vendored tree - not by device-code changes.
3. One of the four 822 rutabaga commits (`a8fd2784`, `attach` keeps handles) is
   **already in main**; there is no rutabaga release newer than 0.1.85 and no
   open upstream PR/issue about releasing main, so the git-dep route is
   unavoidable either way.
4. The symbol churn between 822's base and today is real but mostly *already
   applied* in the fork: the fork deleted its vendored rutabaga copy
   (`9008901e`) and was ported to the 0.1.85 names by the prior map. The rename
   table below is therefore a checklist for ticket 07, not open work.

Evidence file (raw diffs, source excerpts, quotes):
`notes/01-raw-rutabaga-hunks-and-main.txt`.

## 1. The four PR-822 rutabaga commits, hunk by hunk

822's payload touches rutabaga only through four commits (all in libkrun's
then-vendored `src/rutabaga_gfx/`, removed by `9008901e` in the ABI-2 rewrite):

| commit | subject | file in 822's tree | status vs `ec60ee11` |
|---|---|---|---|
| `a8fd2784` | cross_domain: allow `attach()` of guest blobs with handles | `cross_domain/mod.rs` | **present in main** |
| `ec9e5562` | rutabaga_utils: add `RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE` | `rutabaga_utils.rs` | **absent** |
| `3391f3c5` | cross_domain: support creating guest resources with handles | `cross_domain/mod.rs` (now `component.rs`) | **absent** |
| `16e116ae` | direct context-less CREATE_GUEST_HANDLE resources to cross-domain | `rutabaga_core.rs` | **absent** |

Detail, with the exact main-file line numbers:

- **`a8fd2784` - already in main, in reworked form.** Main's
  `src/cross_domain/context.rs:809` `CrossDomainContext::attach` inserts
  `ContextResource { handle: resource.handle.clone(), backing_iovecs: ... }`
  unconditionally; the 822 commit made exactly that simplification (its struct
  was still called `CrossDomainResource`; today it is `ContextResource`). **No
  action.**
- **`ec9e5562` - absent.** `grep RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE` over
  main returns nothing. Main's blob flags stop at
  `RUTABAGA_BLOB_FLAG_USE_CROSS_DEVICE = 0x0004`
  (`src/rutabaga_utils.rs:77-79`). Note main no longer *needs* it (see design
  B), and the fork's own device code already has its own
  `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE = 0x0008` in
  `src/devices/src/virtio/gpu/protocol.rs:86`.
- **`3391f3c5` - absent.** Main's `CrossDomain::create_blob`
  (`src/cross_domain/component.rs:92-121`) still takes `_handle_opt`, keeps the
  old `blob_mem != GUEST && blob_flags != USE_MAPPABLE` guard, and returns
  `handle: None`. This is the hunk that would be needed for design A only.
- **`16e116ae` - absent.** Main's `Rutabaga::resource_create_blob`
  (`src/rutabaga_core.rs:841-885`) fetches the **default component**
  unconditionally and selects a context only when `ctx_id > 0` *and*
  `ctx.component_type() == CrossDomain`. There is no `ctx_id == 0` + flag arm.
  This is design A's routing hunk; design B does not need it because the guest
  sends `ctx_id > 0`.

## 2. Design A - 822's component routing: the delta that must be carried

Required on top of `ec60ee11` (hunk bodies in the evidence file):

1. `RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE = 0x0008` in `src/rutabaga_utils.rs`
   (one line, after the other blob flags).
2. `src/cross_domain/component.rs::create_blob`: rename `_handle_opt` ->
   `handle_opt`; replace the guard with the two-branch check (flag set =>
   require `handle_opt.is_some()`; else require `blob_flags ==
   RUTABAGA_BLOB_FLAG_USE_MAPPABLE`); return `handle: handle_opt.map(Arc::new)`.
   Mechanical deltas: `RutabagaError::SpecViolation(...)` no longer exists - use
   `MagmaGpuError::WithContext(...).into()` (the file already imports
   `magma_gpu::util::Error`); `RutabagaResource.handle` is
   `Option<Arc<RutabagaHandle>>` as before.
3. `src/rutabaga_core.rs::resource_create_blob`: the 13-line routing hunk that
   picks `RutabagaComponentType::CrossDomain` when
   `ctx_id == 0 && blob_flags & RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE != 0`.

That is ~35 lines across three files. It cannot be reduced to a change in
libkrun's own device code: `Rutabaga` exposes only `resource_create_blob` /
`context_attach_resource` (`rutabaga_core.rs` `pub fn` list), never
`context_create_blob`, and components are private, so a VMM cannot select the
CrossDomain component itself. Changing `default_component` to `CrossDomain` is
not an option (venus needs the VirglRenderer component).

## 3. Design B - what rutabaga main already does (recommended): delta = 0

PR #81 was merged **after** 822 was written and deliberately moved the feature
off the component route. Merged main already has, all under `ctx_id > 0`:

- `context_create_blob(resource_id, rcb, iovec_opt, handle_opt)` with
  **Case 1** `(0, RUTABAGA_BLOB_MEM_GUEST, None)` -> `create_blob_from_iovecs`
  and **Case 2** `(0, RUTABAGA_BLOB_MEM_GUEST, Some(handle))` ->
  `create_blob_from_handle` (`src/cross_domain/context.rs:645-700`), where
  `create_blob_from_handle` (`:568`) stores
  `handle: Some(Arc::new(handle.into()))` and derives `map_info` from the
  dma-buf.
- `attach` that keeps the handle (above).
- iovecs plumbed into `context_create_blob` (PR #81 commit `036583f`).

Val Packett's words in PR #81 (2026-08-28, immediately before undrafting):

> "I have it all working now without going through the component-level
> `create_blob`. I do like the new rutabaga diff, and it doesn't rely on any new
> constants anymore (hence the undrafting)..."

What makes `ctx_id > 0` reachable for the udmabuf/PRIME-import case is the
guest kernel, not rutabaga. The virtio-comment v2 series
(`20260903021442.423274-1-val@invisiblethingslab.com`) adds **two** feature bits:

> `VIRTIO_GPU_F_CREATE_GUEST_HANDLE (6)` - guest-only blob resources may be
> created with the `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE` flag.
> `VIRTIO_GPU_F_BLOB_CTX_ID_FIX (7)` - it is always safe to pass the current
> context ID to `VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB`, including for guest-only
> blobs.

and the cover letter lists the working stack as "Backend: rutabaga_gfx#81 +
libkrun#822, Guest: wl-cross-domain-proxy#24, Kernel:
valpackett/linux-qclaptops guest-handle". The kernel-side branch is the part
that sets `params->ctx_id = vfpriv->ctx_id` for `CREATE_GUEST_HANDLE`
(6.12.109's `verify_blob` currently forces `ctx_id = 0` for guest-only blobs and
rejects unknown flags). Because the map already ports the guest-side patch set
into `deps/libkrunfw` (tickets 02/05), the fork can and should adopt bit 7 and
the context route.

**Consequence for the map:** 822's head `3819ce5` (2026-08-27) predates both
PR #81's final shape and the v2 spec series. Carrying 822 verbatim carries a
superseded design (`ctx_id == 0` + component route) and drags a rutabaga fork
behind it. Tickets 03/04/07 should carry 822's **device** half (udmabuf driver,
memfd-backed/sealed guest RAM, flag handling in `virtio_gpu::resource_create_blob`)
and **drop** its three rutabaga hunks, provided ticket 02's kernel patch
negotiates `F_BLOB_CTX_ID_FIX` (7) and plumbs `ctx_id`.

Two preconditions to check in ticket 02/04 (not answered here):
- the `< 6.12`/old-vs-new compatibility story: bit 7 exists precisely so a new
  kernel + old rutabaga does not break, so the fork advertises bit 7 only when it
  implements the context route (we do).
- the ctx the guest names must be the **CrossDomain** context for
  `resource_create_blob` to route there. Otherwise main falls through to the
  default (VirglRenderer) component, whose `create_blob` takes `_handle_opt`
  and ignores it (`src/virgl_renderer.rs:839-868`), so the udmabuf handle is
  silently dropped. For the wl_shm/udmabuf path the buffer's ctx is the
  cross-domain proxy's context, which is the ctx name; a PRIME import bound to a
  venus ctx would not take this route (same in 822's design, which sidestepped it
  by forcing the component).

## 4. Delivery shape

### 4a. The dependency

`src/devices/Cargo.toml` has the dep twice (both `[dependencies]` and
`[target.'cfg(target_os = "linux")'.dependencies]`, lines 45 and 57):

```toml
rutabaga_gfx = { version = "0.1.85", features = ["virgl_renderer"], optional = true }
```

becomes

```toml
rutabaga_gfx = { git = "https://github.com/magma-gpu/rutabaga_gfx", rev = "<full sha>",
                 features = ["virgl_renderer"], optional = true }
```

(or the `zeroqn/rutabaga_gfx` fork URL with the same shape if design A is
chosen). No `[patch]`/`[replace]` is needed when the *fork repo* is the source.
Cargo resolves rutabaga's own `magma-gpu = { path =
"third_party/mesa3d/src/virtio/magma-gpu-rs/lib", version = "0.1.85" }`
(`Cargo.toml` root) inside the git checkout, so `Cargo.lock` records **both**
`rutabaga_gfx` and `magma-gpu` with
`source = "git+https://github.com/magma-gpu/rutabaga_gfx?rev=<sha>#<sha>"` and no
registry checksum (replacing today's registry entries with their checksums).

### 4b. `fetchCargoVendor` - yes, it vendors git deps, and the hash moves twice

`pkgs/rustPlatform.fetchCargoVendor` does **not** run `cargo vendor`; it parses
`Cargo.lock` and builds the vendor tree itself
(`pkgs/build-support/rust/fetch-cargo-vendor-util.py`, nixpkgs
`d3498f786f97ac0bded21b34bae0bf3809b45aa3`):

- every `source = "git+..."` package is fetched with
  `nix-prefetch-git --builder --quiet --fetch-submodules --url <url> --rev <sha>`;
- for each git package it locates the crate's own `Cargo.toml` in the fetched
  tree (`cargo metadata --no-deps`, shallowest first) and copies
  `manifest.parent` into `source-git-N/<name>-<version>/`;
- `[source]` replacement config in `.cargo/config.toml` maps the original git
  selector to that directory.

Consequences:

- **git deps are vendored** (and `--fetch-submodules` is harmless: rutabaga has
  no `.gitmodules`; `third_party/mesa3d` is a plain tree, `git ls-tree` shows a
  tree entry, not a gitlink).
- path/workspace deps inside a git dep are vendored too - proven in-repo today:
  `ffier` is a git dep whose crates are `path = "../ffier-rt"` members, the lock
  records `ffier`, `ffier-impl`, `ffier-builtins`, `ffier-rt` all as one git
  source, and the current `nix build` succeeds. `rutabaga_gfx` (repo-root crate)
  is copied whole - including `third_party/mesa3d/...` - and `magma-gpu` is
  copied again as its own vendor dir.
- **two hashes move**, not one, because the rutabaga rev appears in two locks:
  - `nix/pkgs/libkrun-source.nix` `libkrunCargoDeps` (builds `krun-init` from
    `deps/libkrun`), and
  - `nix/pkgs/cang-rust.nix` `cargoDeps` (cang's own lock, which contains
    libkrun's crates because `crates/cang-libkrun` depends on `deps/libkrun` by
    path - `deps/libkrun/Cargo.lock` **and** the root `Cargo.lock` both list
    `rutabaga_gfx`/`magma-gpu` 0.1.85 as registry deps today).
  Both `flake.nix` checks/`nix build` will need the new hashes in the same
  commit as the submodule pointer. A rev bump later re-moves both.
- the rev must be **committed in both lockfiles**; the fork's CI uses
  `cargo clippy --locked` in `code-quality.yml`/`integration_tests.yml`, which
  fails on a stale lock. There is no `--offline` anywhere.
- the pinned build requires network only at `fetchCargoVendor` time (a
  fixed-output derivation) and in GitHub CI.

### 4c. Release workflow

Nothing in `deps/libkrun/.github/workflows/publish-cang-release.yml` needs a
change: it runs `make BLK=1 NET=1 GPU=1 INPUT=1 TIMESYNC=1 FFI=1` and `make
install`, and `Makefile` builds with plain `cargo build` (no `--locked`,
no `--offline`). The only new requirement is that CI runners can fetch the
rutabaga git repo, which they can. `VIRGL_RESOURCE_MAP2=1` is opt-in and the
release job does not set it.

### 4d. If design A is chosen: where the three hunks can live

- **Owner fork + rev-pinned git dep** (recommended, same mechanics as 4a):
  `zeroqn/rutabaga_gfx`, branch off `ec60ee11`, three commits mirroring
  `ec9e5562`/`3391f3c5`/`16e116ae` rebased onto the new file layout. No
  `[patch]`; `Cargo.toml` points at the fork. Costs one more repo bob owns.
- **`[patch.crates-io]` + a checked-in vendored tree**: keep the registry
  (`version = "0.1.85"`) dependency and add, in libkrun's **workspace-root**
  `Cargo.toml`, `[patch.crates-io] rutabaga_gfx = { path = "vendor/rutabaga_gfx" }`
  with that tree committed into the fork. Feasible and small (main's tree is
  ~2.3 MB / 236 files including `third_party/mesa3d`), and `fetchCargoVendor`
  simply skips local (source-less) packages, so the vendored tree is built from
  `libkrunSrc` (the flake sets `inputs.self.submodules = true`, so it must be
  git-tracked). `[replace "rutabaga_gfx:0.1.85"]` would work identically but is
  deprecated.
- **Not possible:** a `[patch]` that adds a delta on top of a *git* source while
  keeping the same git URL (the patch's source must be a different
  source/repo/path), and any "apply a .patch after fetchCargoVendor" scheme
  (the vendor dir is a fixed-output derivation keyed by hash; patching it
  invalidates the hash and cargo's git-source checksums).
- **Not possible, either design:** doing the routing in libkrun's device code
  (see section 2), unless the fork's kernel also plumbs `ctx_id` - which is
  design B.

## 5. Symbol-rename table (822 base -> `ec60ee11`)

822 was written against libkrun's vendored rutabaga snapshot; the fork deleted
it and ported to 0.1.85 in the ABI-2 rewrite, so most of these are *already
applied* in `deps/libkrun`. Every name 822's hunks use that no longer exists:

| 822's name (sender/use site) | today | status in `deps/libkrun` |
|---|---|---|
| `RUTABAGA_MEM_HANDLE_TYPE_DMABUF` | `RUTABAGA_HANDLE_TYPE_MEM_DMABUF` (`src/lib.rs:33`, = `MAGMA_GPU_HANDLE_TYPE_MEM_DMABUF` 0x0002) | already ported |
| `RUTABAGA_MEM_HANDLE_TYPE_OPAQUE_FD` | `RUTABAGA_HANDLE_TYPE_MEM_OPAQUE_FD` (`src/lib.rs:34`) | already ported |
| `RUTABAGA_MEM_HANDLE_TYPE_APPLE` | **gone** - no Apple handle type anywhere in `magma-gpu` (`defines.rs` has OPAQUE_FD/DMABUF/OPAQUE_WIN32/SHM/ZIRCON only) | n/a (fork's GPU code has no macOS handle-type use) |
| `RUTABAGA_MEM_HANDLE_TYPE_SHM` | only `MAGMA_GPU_HANDLE_TYPE_MEM_SHM`; **no** `RUTABAGA_HANDLE_TYPE_MEM_SHM` re-export | pre-existing latent break: `virtio_gpu.rs:30` imports it under `feature = "virgl_resource_map2"`, a feature `Makefile` only enables with `VIRGL_RESOURCE_MAP2=1`, so cang never compiles it |
| `RUTABAGA_CHANNEL_TYPE_PW` / `_X11` / `_WAYLAND` | `RUTABAGA_PATH_TYPE_PIPEWIRE` / `_X11` / `_WAYLAND` (`rutabaga_utils.rs:615-620`) | already ported |
| `RutabagaHandle { os_handle, handle_type }` (struct literal) | `RutabagaHandle` is an **enum** `MagmaGpuHandle(MagmaGpuHandle) | AhbInfo(AhbInfo)` in `src/handle.rs:31`; the struct is `RutabagaMagmaHandle` (alias of `magma_gpu::util::Handle { os_handle: OwnedDescriptor, handle_type: u32 }`, `defines.rs:36-39`). Build with `RutabagaHandle::from(RutabagaMagmaHandle { .. })` (`handle.rs:36-40`) | n/a - this is ticket 07's one real device-code edit |
| `RutabagaDescriptor`, `RutabagaFromRawDescriptor`, `RutabagaIntoRawDescriptor` | still exported (`src/lib.rs:27-31`, aliases of `magma_gpu::util::{OwnedDescriptor, FromRawDescriptor, IntoRawDescriptor}`) | no change needed; `RutabagaFromRawDescriptor` must stay in scope |
| `RutabagaError::SpecViolation(msg)` (rutabaga hunks) | `MagmaGpuError::WithContext(msg).into()` - `RutabagaError` still exists but has no `SpecViolation` variant (`rutabaga_utils.rs:232-371`) | main's cross_domain code already uses `WithContext` |
| `CrossDomainResource` | `ContextResource` | already ported (0.1.85 included) |
| `cross_domain/mod.rs` (one file) | split into `cross_domain/{mod,component,context,worker,...}.rs` (PR #59, in 0.1.85) | already ported |
| `RutabagaResource` / `ResourceCreateBlob` / `RutabagaIovec` / `RutabagaPath` / `RutabagaBuilder` setters / `Transfer3D` / `ResourceCreate3D` / `RutabagaFence{,Handler}` / `RUTABAGA_MAP_{CACHE,ACCESS}_*` | unchanged in `ec60ee11` (RutabagaResource moved to `src/resource.rs` but is not `pub use`d; libkrun never names it) | no change needed |
| 822's `uapi::VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 5` (commit `230f2c55`) | spec bit **6** (virtio-comment v2); fork already has `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6` in `src/devices/src/virtio/gpu/mod.rs:29` | map already excludes `230f2c55` |

Non-rutabaga symbols in 822's payload that also need adaptation (ticket 07, listed
for completeness): `utils::linux::udmabuf::UdmabufDriver` does not exist yet
(`src/utils/src/linux/` has only `epoll.rs`, `eventfd.rs`), and the
`use_gpu_udmabuf` / `MFD_ALLOW_SEALING` / `F_SEAL_GROW|F_SEAL_SHRINK` part of
822's `vmm/builder.rs` is absent from `src/libkrun/src/vmm/builder.rs:1958`.

## 6. What this ticket hands on

- **Ticket 02/05 (libkrunfw):** the kernel patch must negotiate and honour
  `VIRTIO_GPU_F_BLOB_CTX_ID_FIX (7)` (plumb `vfpriv->ctx_id` for
  `CREATE_GUEST_HANDLE` guest-only blobs) if design B is adopted. Without it,
  main routes the blob to the default VirglRenderer component and the fast path
  does not work.
- **Ticket 04 (carried design):** decide design A vs B. B = zero rutabaga
  patching, one extra kernel bit; A = a second fork repo and ~35 rutabaga lines.
- **Ticket 07 (fork port):** carry 822's device half; the only rutabaga-facing
  edit is constructing `RutabagaHandle::from(RutabagaMagmaHandle { .. })`, plus
  dropping the three rutabaga hunks under design B.
- **Ticket 08 (fork release/pin):** a rutabaga rev change refreshes **two**
  `fetchCargoVendor` hashes (`nix/pkgs/libkrun-source.nix` and
  `nix/pkgs/cang-rust.nix`) and both `Cargo.lock`s.
- **Retirement:** when magma-gpu cuts a release > 0.1.85, swap the git dep back
  to `version = "...";` (still no rutabaga source change under design B). No
  such release or release PR/issue exists as of 2026-09-28 (crates.io max is
  0.1.85; open upstream PRs are #88 and #89, neither release-related).
