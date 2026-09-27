# 02 — Is cherry-picked PR 822 inert without the guest side?

Wayfinder ticket `02-pr-822-inert-check`, map `docs/wayfinder/libkrun-main-rebase`.
Read-only on `deps/libkrun`, `deps/libkrunfw`, `deps/wl-cross-domain-proxy` and on
cang's own source; the only writes are the `02-raw-*` files in this directory.
The PR head was fetched into the submodule as a *remote-tracking ref only*
(`refs/remotes/upstream/pr-822`), never checked out, never rebased, never pushed.

Refs used:

| what | sha | date |
|---|---|---|
| PR 822 head (as reported by the ticket) | `3819ce5fc090a890dec7dc4fbba50bee5c805b17` | 2026-08-27 |
| `merge-base(pr-822, upstream/main)` | `0d75eb4b9d7f742e9b290b7372e4be491e68b173` | 2026-08-26 |
| `upstream/main` | `a980e7795b86c8151e867b32a43af4cb984364fa` | 2026-09-25 |
| `deps/libkrun` HEAD (pin `v1.19.5-cang.1`) | `237ceac0…` (branch `cang` = `28e79624`) | — |
| `deps/libkrunfw` HEAD (pin `v5.6.2-cang.1`) | `9616ca0ff8789d74ca0917472ddbb1eae6ed0ee3` | — |
| `deps/wl-cross-domain-proxy` subtree merge of upstream PR #24 | `cc64c65` | 2026-09-27 |

---

## Verdict

**The fast path is inert on the pinned stack, but the patch is not: it needs a
fork-local gate.** Concretely:

* **Inert (the host half).** With `libkrunfw v5.6.2-cang.1` (6.12.109-hardened1)
  no guest process can put `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE` (0x8) on
  the virtio-gpu queue: the kernel rejects the flag in `verify_blob()` before it
  reaches the device, and the "is the fast path supported" probe the guest
  userland uses (`VIRTGPU_PARAM` 10) returns `-EINVAL`. So the new device arm —
  like today's `panic!("GUEST_HANDLE unimplemented")` — is **unreachable**.
  Nothing in `libkrunfw` (configs *or* patches) supplies either prerequisite.
* **Not inert (the patch).** PR commit `230f2c55` *"`[XXX]` virtio/gpu: update
  `VIRTIO_GPU_F_CREATE_GUEST_HANDLE` constant"* renumbers the feature bit
  **6 → 5**, reusing the slot cang's pinned kernel reserves for
  `VIRTIO_GPU_F_FENCE_PASSING` (libkrunfw `patches/0018`, `#define
  VIRTIO_GPU_F_FENCE_PASSING 5`, in the feature table at
  `virtgpu_drv.c`). Applied verbatim, libkrun would start advertising bit 5
  whenever the host has `/dev/udmabuf`, cang's guest kernel would *ack* it as
  fence passing, and the guest would be told a capability is present that the
  VMM was not trying to expose. **Keep the constant at 6** (skip/adapt
  `230f2c55`) — that is the fork-local gate.
* **Not unsafe.** The PR's replacement for the current `panic!` is a graceful
  error: with `/dev/udmabuf` absent it returns `ErrUnspec`; with it present it
  creates the udmabuf (and `UdmabufDriver::create_udmabuf` validates region
  bounds, page alignment and file-backedness). A guest that could reach the arm
  gets an error response, not a VMM abort. Carrying it is therefore a *hardening*
  of a today-unreachable panic, not a new attack surface.

---

## 1. Does the patched device advertise the feature unconditionally, or behind a flag/config?

**Conditionally — at runtime, on a host capability probe; there is no config
knob.** `src/devices/src/virtio/gpu/device.rs` (`6c51645c`):

```rust
let udmabuf_driver = UdmabufDriver::new()                     // opens /dev/udmabuf O_RDONLY
    .inspect_err(|err| warn!("Could not open udmabuf device: {err}")).ok();
let avail_features = AVAIL_FEATURES | if udmabuf_driver.is_some() {
    1u64 << uapi::VIRTIO_GPU_F_CREATE_GUEST_HANDLE
} else { 0 };
```

* No cargo feature, no CLI/env switch, no VMM-side policy: the only gate is
  `open("/dev/udmabuf", O_RDONLY)` succeeding in `Gpu::new`. On the dev host
  `/dev/udmabuf` **does** exist (kernel 7.2.4, `crw-rw---- root:kvm`), so on this
  host the bit *would* be advertised.
* `UdmabufDriver::new()` (`src/utils/src/linux/udmabuf.rs`) also reads
  `sysconf(_SC_PAGE_SIZE)`; it does not check any udmabuf sub-capability.
* Today's tree advertises exactly the six static bits
  (`deps/libkrun/.../gpu/device.rs:23-28`: `VIRTIO_F_VERSION_1`, VIRGL, EDID,
  RESOURCE_UUID, RESOURCE_BLOB, CONTEXT_INIT) in `AVAIL_FEATURES`, i.e. no bit 5
  and no bit 6, and `virtio_gpu.rs:760-761` panics on the blob flag (main:
  `:773-774`).
* The same commit family changes the constant's **value**: commit `230f2c55`
  comments out `VIRTIO_GPU_F_RESOURCE_SYNC = 5` and sets
  `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 5`. This is the fork hazard described in
  the verdict (see also item 2e).
* Blob-flag plumbing: the flag constant is `protocol.rs:86`
  `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE = 0x0008` (unchanged by the PR; same
  value on the fork and on main).

## 2. With the pinned kernel (no `VIRTGPU_PARAM` 10), can any guest set the flag — and if it could, what happens?

**(a) The guest probe fails.** `deps/wl-cross-domain-proxy` (the merged guest
side) probes `Param::CreateGuestHandle = 10` in `ClientChannel::new`
(`src/source/channel/mod.rs:93-96`, `src/virtio_gpu/mod.rs:56`). In Linux
v6.12.109 `virtio_gpu_getparam_ioctl()` handles only params 1..8 and ends with
`default: return -EINVAL;` (drivers/gpu/drm/virtio/virtgpu_ioctl.c:88-127), so
`ensure_feature` errors and `has_create_guest_handle = false` → the proxy takes
the existing copy path. (The pinned kernel has no patch touching `getparam`;
libkrunfw's only drm/virtio patches are 0018 fence-passing, 0022
virtgpu-gem-partial-map, 0023 virtgpu-mixed-page-size.)

**(b) The kernel rejects the flag itself.** `verify_blob()` (same file:439-481)
starts with

```c
if (rc_blob->blob_flags & ~VIRTGPU_BLOB_FLAG_USE_MASK)
        return -EINVAL;
```

and `VIRTGPU_BLOB_FLAG_USE_MASK` is `USE_MAPPABLE|USE_SHAREABLE|USE_CROSS_DEVICE
= 0x7` in v6.12.109 (`include/uapi/drm/virtgpu_drm.h`); `0x8` is not in it, and
the uapi header does not define a fourth flag. `git -C deps/libkrunfw grep -n
'CREATE_GUEST_HANDLE' v5.6.2-cang.1` and `... grep -c 'blob_flags' v5.6.2-cang.1
-- patches` both come back **empty**: no cang/fork kernel patch adds the flag,
the mask bit, or the param. So `DRM_VIRTGPU_RESOURCE_CREATE_BLOB` with bit 8
never becomes a virtio command; the device never sees it.

**(c) Therefore: inert, exactly as in today's tree.** The new arm is dead code on
this pin, and so is the `panic!` it replaces. The cherry-pick changes nothing a
guest can observe on the host-data path.

**(d) But if a guest *could* set it**, the PR's behaviour is bounded:
* host `/dev/udmabuf` present → `driver.create_udmabuf(mem, &vecs)` builds a host
  dma-buf over the guest's (memfd-backed) pages and hands it to rutabaga as a
  `RUTABAGA_MEM_HANDLE_TYPE_DMABUF` handle;
* host udmabuf absent → `ErrUnspec` (a virtio error response), explicitly
  commented *"Not expecting well-behaved guests to hit this path, as we don't
  set the feature flag without the driver"* — no panic, no abort;
* `create_udmabuf` itself checks region membership, file-backedness, bounds and
  page alignment (`RegionNotFileBacked`, `OutOfBounds`, `NotPageAligned`).
  Note it does **not** require the blob to be `BLOB_MEM_GUEST`; the flag is the
  only gate, which is fine only because nothing can set the flag today.

**(e) The one real "not inert" here.** `230f2c55` gives the device bit 5, and
cang's pinned kernel defines bit 5 as `VIRTIO_GPU_F_FENCE_PASSING` and puts it in
`virtio_gpu_driver.feature_table` (`libkrunfw` tag `v5.6.2-cang.1`,
`patches/0018-…`, lines 46/184/427, plus `VIRTGPU_CONTEXT_PARAM_FENCE_PASSING
0x0005`). The fork's libkrun never advertises bit 5 today (`AVAIL_FEATURES`
excludes `VIRTIO_GPU_F_RESOURCE_SYNC = 5`, and `git grep RESOURCE_SYNC cang`
finds only the constant), so that kernel feature is dormant — until the PR's
renumber turns it on. The device *does* already parse the extended framing
(`protocol.rs:418 num_in_fences`, `worker.rs:341-355` collects `fence_ids`, and
`virtio_gpu.submit_command(ctx_id, commands, fence_ids)`), so this is an
unintended capability flip rather than a guaranteed crash — but it is a
behaviour change on cang's stack and it means the advertised bit no longer means
what the Rust constant says. Upstream `main` has no kernel with bit 5, so only
cang sees this.

## 3. What exactly does the guest need? (the map's real prerequisite)

In order of who owns it:

1. **Guest kernel: `VIRTGPU_PARAM` 10.** The proxy's `Param::CreateGuestHandle =
   10` (`deps/wl-cross-domain-proxy/src/virtio_gpu/mod.rs:56`, comment
   *"XXX: NOT UPSTREAM YET"*) must return non-zero from
   `virtio_gpu_getparam_ioctl`. Today: `-EINVAL` → the proxy's
   `has_create_guest_handle` is `false` → copy path.
2. **Guest kernel: the blob flag.** `VIRTGPU_BLOB_FLAG_CREATE_GUEST_HANDLE`
   (0x8) must be inside `VIRTGPU_BLOB_FLAG_USE_MASK` (else `verify_blob` −EINVAL)
   **and** some kernel path must stamp it — per the PR's own commit messages
   (`16e116ae`: *"The guest can also import DMA-BUFs from other devices, whether
   udmabuf … In that case, the guest userspace has no way of passing in a
   requirements item ID, and the guest kernel does not pass a context ID
   either"*) that path is the **kernel's PRIME import**, not the proxy. The
   proxy defines `BlobFlags::CREATE_GUEST_HANDLE = 8`
   (`src/virtio_gpu/mod.rs:81`) but **never passes it**: every call site uses
   `USE_MAPPABLE`/`USE_SHAREABLE` only. The fast-path flag originates in the
   (patched) guest kernel, so "the proxy sets CREATE_GUEST_HANDLE" is *not* a
   correct prerequisite statement.
3. **Guest kernel: `CONFIG_UDMABUF=y` + `/dev/udmabuf`.** `wl-cross-domain-proxy`
   opens `/dev/udmabuf` in-guest (`src/udmabuf.rs:50`, `Udmabuf::open()` at
   `src/source/channel/wayland.rs:223-226`) and errors `"udmabuf device absent"`
   otherwise. The pinned configs say **`# CONFIG_UDMABUF is not set`** for
   x86_64 (line 1933), aarch64 (2498) and riscv64 (2241) — so even with a patched
   kernel param the guest fast path would fall back. *This is a `libkrunfw`
   config change, not a libkrun change.*
4. **Guest userland: the proxy itself** — already in cang (`deps/…`, upstream PR
   #24 merged at `cc64c65`), including its `has_create_guest_handle` probe and
   its own udmabuf/seal preconditions (guest shm memfd must be memfd-backed, not
   `F_SEAL_WRITE`/`FUTURE_WRITE`, and gets `F_SEAL_SHRINK` added by the proxy,
   `wayland.rs:304-322`).
5. **Host: `/dev/udmabuf`** readable by the VMM (the PR's only advertisement
   gate) on a kernel running `drivers/dma-buf/udmabuf.c` with its seal contract:
   `SEALS_WANTED (F_SEAL_SHRINK)`, `SEALS_DENIED (F_SEAL_WRITE|F_SEAL_FUTURE_WRITE)`
   (v6.12.109 lines 253-268), plus `size_limit_mb` (default 64 MB per dma-buf)
   and `list_limit` (1024 items).
6. **Host VMM: memfd-backed, sealed guest RAM.** The PR's dependency commits
   `4ef22a14` (create file-backed memory "when a GPU device is present as well")
   and `a3962256` (`MFD_ALLOW_SEALING` + `F_ADD_SEALS(F_SEAL_GROW|F_SEAL_SHRINK)`)
   are what make `UdmabufDriver::create_udmabuf` possible at all — it requires
   `region.file_offset()` (`RegionNotFileBacked` otherwise) and the kernel's
   udmabuf seal check requires `F_SEAL_SHRINK`.
7. **Host VMM: the rutabaga/cross-domain plumbing** — `attach()` must stop
   dropping handles for `RUTABAGA_BLOB_MEM_GUEST` resources (`a8fd2784`),
   `create_blob` must accept `handle_opt` (`3391f3c5`), and context-less
   (imported) resources must be routed to the CrossDomain *component*
   (`16e116ae`).

## 4. Conflict surface for cherry-picking onto a rebased `main`

**The PR head is the reported sha and the reported diffstat is the 3-dot diff**
(`3819ce5f`, 12 commits, `merge-base = 0d75eb4b`, main 116 commits ahead;
`git diff --stat 0d75eb4b..3819ce5f` = *36 files changed, 372 insertions(+),
148 deletions(-)* — identical to the ticket).

**Two of the twelve commits are already in main** and must not be re-applied:
`65088540 Upgrade to vm-memory 0.18, vmm-sys-util 0.15` (main `8ec759c3` +
follow-ups `0db3bebb`) and `5849c08d virtio/blk: use imago's new fully sync API`
(main `8e376d82` + `6a11683f`). The cherry-pick payload is therefore
`git diff 5849c08d..3819ce5f` (**10 commits, 575 diff lines**, saved as
`02-raw-pr822-gpu-payload.patch`).

Composite 3-way merge of the full 12 commits onto `upstream/main`
(`git merge-tree --write-tree`, saved raw) conflicts in **13 paths**:

```
Cargo.lock
src/devices/Cargo.toml
src/devices/src/virtio/block/device.rs
src/devices/src/virtio/gpu/virtio_gpu.rs
src/devices/src/virtio/vhost_user/device.rs
src/libkrun/Cargo.toml
src/libkrun/src/vmm/builder.rs          (PR path src/vmm/src/builder.rs, git-renamed)
src/libkrun/src/vmm/linux/vstate.rs
src/rutabaga_gfx/src/cross_domain/mod.rs   (modify/delete: deleted in main)
src/rutabaga_gfx/src/rutabaga_core.rs      (modify/delete)
src/rutabaga_gfx/src/rutabaga_utils.rs     (modify/delete)
src/utils/Cargo.toml
src/vmm/Cargo.toml                         (modify/delete)
```

Per-commit 3-way cherry-pick onto `upstream/main` (raw matrix in this directory)
narrows this down:

| commit | onto `upstream/main` |
|---|---|
| `4ef22a14` vmm/builder file-backed memory | **clean** (auto-renamed to `src/libkrun/src/vmm/builder.rs`) |
| `a3962256` vmm/builder seals | **clean** |
| `a8fd2784` rutabaga attach() keeps handles | **modify/delete conflict** (`src/rutabaga_gfx/` gone) |
| `ec9e5562` rutabaga_utils flag const | **modify/delete conflict** |
| `3391f3c5` rutabaga cross_domain create-with-handle | **modify/delete conflict** |
| `16e116ae` rutabaga_core component routing | **modify/delete conflict** |
| `b3f9c114` utils/linux/udmabuf.rs (new file) | **clean** |
| `230f2c55` `[XXX]` constant 6→5 | **clean** (but see the verdict: this is the one to gate) |
| `6c51645c` device advertises bit + passes driver | **content conflict in `virtio_gpu.rs`, import block only** (3 hunks); `device.rs` and `worker.rs` auto-merge |
| `3819ce5f` udmabuf implementation in create_blob | **clean** |

*(On the fork tip `cang` = `28e79624` the same matrix differs: `4ef22a14`,
`a3962256` and `b3f9c114` conflict too — the fork's 1.19 `vmm/src/builder.rs` and
`src/utils/Cargo.toml` have diverged — while all four rutabaga commits apply
cleanly, because the in-tree crate still exists there. Raw matrix:
`02-raw-pr822-cherrypick-matrix-fork.txt`.)*

**Where the conflicts come from.**

* `src/devices/src/virtio/gpu/virtio_gpu.rs` — every conflict hunk is in the
  `use` block, not in logic. Main's `rutabaga_gfx` moved from the in-tree crate
  to crates.io **0.1.85**, which renamed the symbols the PR's imports use
  (`RUTABAGA_MEM_HANDLE_TYPE_DMABUF` → `RUTABAGA_HANDLE_TYPE_MEM_DMABUF` — now
  an alias of `magma_gpu::util`; `RUTABAGA_CHANNEL_TYPE_*` →
  `RUTABAGA_PATH_TYPE_*`; `AsFd` added) and changed `RutabagaHandle`'s home
  (`src/handle.rs`). The *body* of `3819ce5f` merges cleanly but still names
  `RUTABAGA_MEM_HANDLE_TYPE_DMABUF`, so it needs a mechanical rename to compile
  against 0.1.85.
* `src/rutabaga_gfx/*` (4 hunks) — main deleted `src/rutabaga_gfx` entirely
  (`ls-tree upstream/main | grep -c rutabaga` = 0). Worse than the path change:
  **crates.io `rutabaga_gfx 0.1.85` does not contain the PR's changes** — it has
  no `RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE` (only USE_MAPPABLE/SHAREABLE/
  CROSS_DEVICE, `src/rutabaga_utils.rs:77-79`), its `CrossDomainContext::attach`
  still inserts `handle: None` for guest blobs
  (`src/cross_domain/context.rs:741-757`), its `CrossDomain::create_blob`
  ignores `_handle_opt` and returns `handle: None`
  (`src/cross_domain/component.rs:88-105`), and the component was refactored
  from one `cross_domain/mod.rs` into
  `cross_domain/{component,context,worker,common}.rs`. These four hunks cannot be
  fixed inside cang's tree: they need an upstream `rutabaga_gfx` change (or a
  vendored/patched crate).
* `Cargo.lock`, `src/devices/Cargo.toml`, `src/libkrun/Cargo.toml`,
  `src/utils/Cargo.toml`, `src/vmm/Cargo.toml`, `src/devices/src/virtio/
  vhost_user/device.rs`, `src/devices/src/virtio/block/device.rs`,
  `src/libkrun/src/vmm/linux/vstate.rs` — all from the two dependency-bump
  commits that main already has (in a slightly different form). Excluding them
  removes those conflicts from the payload.
* `src/libkrun/src/vmm/builder.rs` — main moved `src/vmm` under
  `src/libkrun/src/vmm` and rewrote the builder for the ABI-2 object API. Git's
  rename detection maps the PR's hunk onto the new path and it *auto-merges*,
  but the merged line is
  `let use_gpu_udmabuf = vm_resources.gpu_virgl_flags.is_some();` and main's
  rewritten builder **has no `gpu_virgl_flags`** (`git grep -n gpu_virgl_flags
  upstream/main -- src/libkrun/src/vmm` → nothing). The nearest surviving signal
  on main is `gpu_shm_size = requirements.iter().filter_map(|r| r.gpu_shm).next()`
  (`builder.rs:723`), threaded into `create_guest_memory` and consumed at
  `builder.rs:1899`; it is `Some` only when a GPU shm region was requested, so
  the port has to decide which condition actually means "the gpu device exists". So this hunk needs a real one-line port
  (choose the correct condition), not just a clean merge — and main's
  `create_guest_memory` still has the `use_vhost_user` memfd branch
  (`builder.rs:1907-1970`) that the PR extends.

**Does the diff still apply to main's post-rewrite tree?** Partially, yes: the
device/worker/mod/utils/udmabuf and builder hunks still apply (auto-merge, one
mechanical import-block conflict and one symbol rename), i.e. the changes to
gpu device code survived the ABI rewrite because the rewrite replaced
`src/libkrun/src/lib.rs`/the C API, not `src/devices`. The four `src/rutabaga_gfx`
hunks do **not** apply in any meaningful sense and must be re-expressed.

---

## Recommendation recorded for ticket 03

1. Carry the payload **without `230f2c55`**: keep
   `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6` (fork-local gate) and leave
   `VIRTIO_GPU_F_RESOURCE_SYNC = 5` alone, unless and until the guest-kernel
   numbering is settled — cang's pinned kernel already claims bit 5 for
   `VIRTIO_GPU_F_FENCE_PASSING`.
2. Carry the rutabaga-side changes as a vendored/patched `rutabaga_gfx` (or an
   upstream crate change); a plain cherry-pick cannot reach them.
3. The path stays inert until *libkrunfw* grows (a) `CONFIG_UDMABUF=y` and
   (b) the non-upstream virtio-gpu param-10 + blob-flag-8 kernel patch. Ticket
   05's scope should say so explicitly, and ticket 08's GPU smoke cannot
   exercise this fast path on the current pin.
4. The PR is otherwise safe to carry: it replaces a today-unreachable `panic!`
   with an error response and only advertises when the host can actually serve
   the feature.

## Raw evidence in this directory

| file | contents |
|---|---|
| `02-raw-pr822-refs-diffstat.txt` | refs, merge base, 12-commit list, full + payload diffstats |
| `02-raw-pr822-files.txt` | `--name-status` and per-file diffstat of the 3-dot diff |
| `02-raw-pr822-cherrypick-matrix.txt` | per-commit cherry-pick of all 12 commits onto `upstream/main` |
| `02-raw-pr822-cherrypick-matrix-fork.txt` | same 10-commit payload onto the fork tip `cang` (`28e79624`) |
| `02-raw-pr822-merge-tree-and-conflict.txt` | composite `merge-tree` conflicts + the `6c51645c` conflict hunks |
| `02-raw-pr822-gpu-payload.patch` | `git diff 5849c08d..3819ce5f` — the cherry-pick payload |
| `02-raw-pinned-kernel-and-guest-requirements.txt` | libkrunfw config/patch greps, libkrun constants, proxy probe sites, host `/dev/udmabuf` |
| `02-raw-kernel-and-crates-primary-sources.txt` | v6.12.109 `virtgpu_ioctl.c`/`udmabuf.c`/uapi/feature-table extracts, `rutabaga_gfx 0.1.85` extracts |

Primary sources cited: Linux stable v6.12.109
(`drivers/gpu/drm/virtio/virtgpu_ioctl.c`, `virtgpu_drv.c`,
`include/uapi/drm/virtgpu_drm.h`, `drivers/dma-buf/udmabuf.c`, fetched from
git.kernel.org); `libkrunfw` tag `v5.6.2-cang.1` in `deps/libkrunfw`;
`deps/libkrun` refs `upstream/main`, `upstream/pr-822`, `cang`, `HEAD`;
`deps/wl-cross-domain-proxy` at `cc64c65`; crates.io `rutabaga_gfx-0.1.85.crate`.
