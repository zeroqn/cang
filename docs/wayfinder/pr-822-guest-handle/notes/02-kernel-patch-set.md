# 02 — Which Linux guest-side patch set gives 6.12.109 `CREATE_GUEST_HANDLE`?

Ticket `docs/wayfinder/pr-822-guest-handle/tickets/02-kernel-patch-set.md`.
Read-only on `deps/libkrunfw` (HEAD `9616ca0`, tag `v5.6.2-cang.1`), `deps/libkrun`
(HEAD `3d7af2c2`) and `deps/wl-cross-domain-proxy` (subtree merge `cc64c65`).
Primary sources are `git.kernel.org` plain files and `lore.kernel.org` message
pages; the only writes are this file and the `02-raw-*` files beside it.

## Verdict

There is **no single upstream series**. Getting `CREATE_GUEST_HANDLE` onto the
pinned `linux-6.12.109` + `v6.12.109-hardened1` kernel needs **two** series plus a
config change and one fork-authored gate:

1. **The prerequisite** — `[PATCH v5 0/5] drm/virtio: Import scanout buffers
   from other devices` (Vivek Kasireddy, 2024-11-26), first shipped in **v6.14**.
   It is the PRIME-import-as-guest-blob path (
   `virtgpu_dma_buf_init_obj()` and `virtgpu_gem_dma_buf_funcs`), which both the
   merged `df4dc947c46b` and the new re-send build on. It is **absent from
   6.12.109 and from 6.13** (6.12.109's `virtgpu_gem_prime_import()` is literally
   `return drm_gem_prime_import(dev, buf);`).
2. **Val Packett's new re-send** — 6 commits on
   `github.com/valpackett/linux-qclaptops` branch **`guest-handle`** @
   `3e6a365d2ac9988058faef27629817908961422a` (2026-08-28), adding
   `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`, `VIRTIO_GPU_F_BLOB_CTX_ID_FIX = 7`,
   `VIRTGPU_PARAM_CREATE_GUEST_HANDLE = 10`, blob flag `0x0008`, the
   `virtgpu_dma_buf_init_obj()` flag stamp and the ctx_id plumbing. **This series
   has never been posted to lore** (checked: only the 2023 gfxstream-era mention
   and Val's 2026-09-03 *spec* series exist); the cover letter itself says
   *"Kernel: want to link here when posting to lkml :)"*.
3. **`CONFIG_UDMABUF=y`** in all six `deps/libkrunfw/config-libkrunfw_*`
   (currently `# CONFIG_UDMABUF is not set` in every one) — for the *guest
   userland* half, not the kernel patch.
4. **A fork-authored PRIME-import gate.** Val's branch is based on linux-next
   **20260901**, i.e. *before* the revert (`1e3b08de`, 2026-09-20) and on top of
   the still-applied `df4dc947c46b` — its
   `virtgpu_gem_prime_import()` has `if (!vgdev->has_resource_blob)` with **no
   `has_virgl_3d` check**. After the revert the unlock has to be conditional, as
   Val said on 2026-09-15: *"I'll 'unlock' this again in new patches but … gated
   on the new `F_CREATE_GUEST_HANDLE` flag"*. So the 6.12.109 port must author
   `if (!vgdev->has_resource_blob || (vgdev->has_virgl_3d &&
   !vgdev->has_create_guest_handle)) return drm_gem_prime_import(...);` itself.

No upstream or stable line has param 10 or blob flag `0x8` as of 2026-09-28:
mainline HEAD (`torvalds/linux`) tops out at `VIRTIO_GPU_F_BLOB_ALIGNMENT = 5` /
`VIRTGPU_PARAM_BLOB_ALIGNMENT = 9`, and `6.12.y` HEAD == `v6.12.109` at param 8
and mask `0x7`. There is nothing to cherry-pick from stable.

## 1. The public series, with hashes

| date | event | id / ref |
|---|---|---|
| 2024-11-26 | `[PATCH v5 0/5] drm/virtio: Import scanout buffers from other devices` (Vivek Kasireddy) — 5 patches, `20241126031643.3490496-{1..6}-vivek.kasireddy@intel.com`; ships in **v6.14** | commits `25c3fd1183c0`, `2885e575abc7`, `ca77f27a2665` |
| 2025-12-10 | `[PATCH] drm/virtio: Allow importing prime buffers when 3D is enabled` (Val Packett), patch 2/2, msgid `20251210154755.1119861-2-val@invisiblethingslab.com` | — |
| 2026-02-12 | merged as `df4dc947c46bb9f80038f52c6e38cb2d40c10e50` (author 2025-12-10, committer Dmitry Osipenko `drm-misc-next`); `Fixes: ca77f27a2665` | `git.kernel.org/.../commit/?id=df4dc947c46b…` |
| 2026-05-20 | stable backports to **6.18.32-rc1** (#187/957) and **7.0.10-rc1** (#0230/1146) by Greg KH; **not** backported to 6.12.y | `20260520162153.457299487@linuxfoundation.org`, `202605201611+` |
| 2026-09-03 | `[PATCH v2 0/2] virtio_gpu: add F_CREATE_GUEST_HANDLE and F_BLOB_CTX_ID_FIX` (Val, virtio-comment) — bits 6 and 7; v1 `20260827092120.97295-1-val@invisiblethingslab.com` was lost to moderation | msgid `20260903021442.423274-1-val@invisiblethingslab.com` |
| 2026-09-04 | Demi Marie Obenour asks whether `CREATE_GUEST_HANDLE` should require `BLOB_CTX_ID_FIX`; Val agrees and promises **v3** (v3 not posted as of 2026-09-28) | `a5d79e14-0064-4941-9891-c71a9660cf5a@gmail.com`, reply `8d47c1b6-18ec-47d6-baf2-8a1d8c27de57@…` |
| 2026-09-11 | `[PATCH v1] Revert "drm/virtio: Allow importing prime buffers when 3D is enabled"` (Dmitry Osipenko), `Fixes: df4dc947c46b`, reverts the one-line check in `virtgpu_prime.c` | msgid `20260911144204.2089401-1-dmitry.osipenko@collabora.com` |
| 2026-09-15 | Val: `Reviewed-by:` + "I'll unlock again gated on `F_CREATE_GUEST_HANDLE`" | `579d804b-d548-4c8d-8350-e1e181a612ae@invisiblethingslab.com` |
| 2026-09-20 | Dmitry: *"Applied to misc-fixes"*; merged as `1e3b08de63274d0b009e99ef51cd6a9c0c6bf08c` (committed 2026-09-20 20:48:52 +0300) | `1803e07b-63d9-45f7-80d7-f52980c3b321@collabora.com` |
| 2026-08-28 | kernel implementation series (6 commits) on branch `guest-handle` @ `3e6a365d2ac9` — **not on lore** | `api.github.com/repos/valpackett/linux-qclaptops/branches/guest-handle` |

Kernel series (branch order, oldest→newest):

| # | sha | subject |
|---|---|---|
| 1 | `a412cc915ec7` | `drm/virtio: support VIRTIO_GPU_F_CREATE_GUEST_HANDLE` |
| 2 | `828df4d292f2` | `drm/virtio: use CREATE_GUEST_HANDLE for imported DMA-BUFs` |
| 3 | `2a41d37ca1d0` | `drm/virtio: attach imported DMA-BUFs to current 3D context if exists` |
| 4 | `953b78600d13` | `drm/virtio: add VIRTGPU_PARAM_CREATE_GUEST_HANDLE to params` |
| 5 | `1c148608fe3a` | `drm/virtio: support VIRTIO_GPU_F_BLOB_CTX_ID_FIX` |
| 6 | `3e6a365d2ac9` | `drm/virtio: pass the current ctx_id during PRIME import` |

Note the branch's base includes upstream `VIRTIO_GPU_F_BLOB_ALIGNMENT = 5`
(and param 9); 6.12.109 does not, and that is the cause of most of the textual
conflicts below.

## 2. What must be patched onto 6.12.109

`git apply` dry runs against a minimal tree of the 10 `v6.12.109` files the two
series touch (raw log: `02-raw-backport-apply-matrix.txt`).

| # | piece | where | state on 6.12.109 | verdict |
|---|---|---|---|---|
| 1 | PRIME-import-as-guest-blob base (`dtach-backing`, map helper, init/free helpers, `virtgpu_dma_buf_init_obj`, prepare/cleanup) | `virtgpu_{prime,vq,object,plane,drv,drv.h}.c` | absent (first in **v6.14**) | **backport**; first 4/5 patches apply clean, patch 6's `virtgpu_plane.c` hunk needs re-author |
| 2 | blob flag `VIRTGPU_BLOB_FLAG_CREATE_GUEST_HANDLE 0x0008` + in `USE_MASK` | `virtgpu_ioctl.c`, `include/uapi/drm/virtgpu_drm.h` | mask `0x7`, no flag | **applies clean** |
| 3 | `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6` (feature table, `has_create_guest_handle`, `virtio_gpu_init`) | `virtgpu_drv.c`, `drv.h`, `kms.c`, `include/uapi/linux/virtio_gpu.h` | bits 0..4 only | **re-author** (upstream context is `BLOB_ALIGNMENT = 5`; the `verify_blob` hunk applies) |
| 4 | stamp the flag in `virtgpu_dma_buf_init_obj()` | `virtgpu_prime.c` | needs piece 1 | **applies clean** once piece 1 is in (`val-02`) |
| 5 | `.open/.close = virtio_gpu_gem_object_{open,close}` on `virtgpu_gem_dma_buf_funcs` | `virtgpu_prime.c` | needs piece 1 | **applies clean** (`val-03`) |
| 6 | `VIRTGPU_PARAM_CREATE_GUEST_HANDLE = 10` (getparam + uapi) | `virtgpu_ioctl.c`, `include/uapi/drm/virtgpu_drm.h` | max param 8 | ioctl hunk **applies clean**; uapi hunk **re-author** (insert after 8, value 10, skipping 9) |
| 7 | `VIRTIO_GPU_F_BLOB_CTX_ID_FIX = 7` + `params->ctx_id` gating in `verify_blob()` | `virtgpu_drv.c`, `drv.h`, `kms.c`, `virtgpu_ioctl.c`, `virtio_gpu.h` | absent | **re-author** on the bit/context hunks; the `verify_blob` hunk applies |
| 8 | `.prime_fd_to_handle = virtgpu_prime_fd_to_handle` + `params.ctx_id = vfpriv->ctx_id` | `virtgpu_drv.c`, `drv.h`, `virtgpu_prime.c` | absent | **applies with context re-author** (upstream PM/restore fields in `drv.h` are absent from 6.12.109) |
| 9 | the PRIME-import unlock, gated | `virtgpu_prime.c` | 6.12.109 pre-dates `ca77f27a`, so no `df4dc947` and nothing to revert | **newly authored**: `if (!vgdev->has_resource_blob \|\| (vgdev->has_virgl_3d && !vgdev->has_create_guest_handle))` |
| 10 | `CONFIG_UDMABUF=y` | all six `config-libkrunfw_*` | `# CONFIG_UDMABUF is not set` | config edit, trivial |

Only piece 9 *depends on the revert*: upstream after `1e3b08de` rejects the
import when `has_virgl_3d`; the re-send has to relax that exactly when the new
feature is negotiated. On 6.12.109 there is no `df4dc947` to revert, so the port
authors the final conditional form directly.

Piece 7's `BLOB_CTX_ID_FIX` is the "newer route": `verify_blob()` sets `ctx_id`
when `*host3d_blob || has_blob_ctx_id_fix`, so a guest-only blob created via
PRIME import carries the current context id only when bit 7 was negotiated. The
virtio-comment v3 (promised, not posted) additionally makes
`CREATE_GUEST_HANDLE` *require* `BLOB_CTX_ID_FIX`; as of the 2026-08-28 branch
the two bits are still independent in the driver.

## 3. Feature-bit numbering — and what must never move

* virtio spec v2 (Val, 2026-09-03): `VIRTIO_GPU_F_CREATE_GUEST_HANDLE (6)`,
  `VIRTIO_GPU_F_BLOB_CTX_ID_FIX (7)`.
* Kernel branch (`a412cc915ec7`, `1c148608fe3a`): same — `6` and `7`, with the
  branch's base already using `5 = VIRTIO_GPU_F_BLOB_ALIGNMENT`.
* cang's pinned 6.12.109: bits `0..4` only (`VIRGL`…`CONTEXT_INIT`).
* cang's fork kernel claims **`5 = VIRTIO_GPU_F_FENCE_PASSING`**
  (`deps/libkrunfw/patches/0018-…patch` line 427) and puts it in the
  `features[]` table. Upstream mainline has since assigned `5` to
  `VIRTIO_GPU_F_BLOB_ALIGNMENT` — a real, latent collision.

**Never renumber 5, 6 or 7** on the fork: keep `5 = FENCE_PASSING` (do not import
upstream `BLOB_ALIGNMENT = 5`), add `6 = CREATE_GUEST_HANDLE` and
`7 = BLOB_CTX_ID_FIX` as upstream numbers them. Exclude libkrun PR 822's commit
`230f2c55` (`VIRTIO_GPU_F_CREATE_GUEST_HANDLE` `6 -> 5`); it would make the
device advertise cang's kernel bit 5 and switch `FENCE_PASSING` on. libkrun's
Rust constants already say `VIRTIO_GPU_F_RESOURCE_SYNC = 5`,
`VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`
(`src/devices/src/virtio/gpu/mod.rs:28-29`), and `AVAIL_FEATURES` advertises
neither.

The blob flag is a separate namespace: `VIRTGPU_BLOB_FLAG_CREATE_GUEST_HANDLE =
0x0008`, making `VIRTGPU_BLOB_FLAG_USE_MASK = 0x1|0x2|0x4|0x8`. The kernel uapi
value (`0x0008`) and the guest proxy's `BlobFlags::CREATE_GUEST_HANDLE = 8` must
agree; the value is independent of the feature bit.

## 4. Does `deps/wl-cross-domain-proxy` (`cc64c65`) change?

**No — under the carried design the flag originates purely in the kernel's PRIME
import path.** The proxy (`src/source/channel/wayland.rs`) turns the client
memfd into a dma-buf with its own `/dev/udmabuf` and then calls
`drm.prime_fd_to_buffer(dma_fd)`; that enters `virtgpu_gem_prime_import()` →
`virtgpu_dma_buf_init_obj()`, which is what ORs in
`VIRTGPU_BLOB_FLAG_CREATE_GUEST_HANDLE`. The proxy *probes*
`Param::CreateGuestHandle = 10` (`src/source/channel/mod.rs:93-96`) only as a
gate, and its `BlobFlags::CREATE_GUEST_HANDLE = 8` is defined but never passed.

Its needs are exactly: (a) the kernel's `VIRTGPU_PARAM_CREATE_GUEST_HANDLE = 10`
returning non-zero (piece 6), and (b) `CONFIG_UDMABUF=y` so `/dev/udmabuf`
exists (`src/udmabuf.rs:50`, `Udmabuf::open()` at `wayland.rs:223`). A
proxy-side-blob design (passing the flag in `create_blob`) would need proxy
changes, but that is not the design Val implemented or the one PR 822 serves.

## 5. Raw evidence

| file | contents |
|---|---|
| `02-raw-backport-apply-matrix.txt` | `git apply` + `patch --fuzz=2` runs of both series against a minimal `v6.12.109` tree; base feature bits; `virtgpu_gem_prime_import()` at `v6.12.109`; `v6.13` vs `v6.14` probe |
| `02-raw-val-01…06-*.patch` | the 6 branch commits as `git format-patch` text |
| `02-raw-prereq-01-cover.patch.txt` … `02-raw-prereq-06-prepare-cleanup.patch` | Vivek's `[PATCH v5 0/5]` cover + 5 patches (lore `/raw`) |
| `02-raw-revert.patch` | Dmitry's revert message (lore page for `20260911144204…`) |
| `02-raw-kernel-series-primary-sources.txt` | commit pages, spec thread excerpts, GitHub branch/commit listings, `wl-cross-domain-proxy` probe sites |

## Sources

* `https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/plain/…?h=v6.12.109` —
  `include/uapi/linux/virtio_gpu.h`, `include/uapi/drm/virtgpu_drm.h`,
  `drivers/gpu/drm/virtio/virtgpu_prime.c`; `?h=v6.13` / `?h=v6.14` for the base.
* `https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/commit/?id=df4dc947c46b…`,
  `…/commit/?id=1e3b08de6327…`, `…/log/drivers/gpu/drm/virtio/virtgpu_prime.c`,
  `…/plain/include/uapi/{linux/virtio_gpu.h,drm/virtgpu_drm.h}` (HEAD).
* `https://lore.kernel.org/…` message pages / `/raw`: `20260903021442.423274-1-val@…`,
  `a5d79e14-0064-4941-9891-c71a9660cf5a@…`, `8d47c1b6-18ec-47d6-baf2-8a1d8c27de57@…`,
  `20260911144204.2089401-1-dmitry.osipenko@…`,
  `579d804b-d548-4c8d-8350-e1e181a612ae@…`,
  `1803e07b-63d9-45f7-80d7-f52980c3b321@…`,
  `20241126031643.3490496-{1..6}-vivek.kasireddy@intel.com`.
* `https://github.com/valpackett/linux-qclaptops` branch `guest-handle` and its
  6 commit patches; `https://api.github.com/repos/valpackett/linux-qclaptops/…`.
* repo tree: `deps/libkrunfw/patches/0018-…`, `deps/libkrunfw/config-libkrunfw_*`,
  `deps/libkrun/src/devices/src/virtio/gpu/{mod.rs,device.rs}`,
  `deps/wl-cross-domain-proxy/src/{virtio_gpu/mod.rs,source/channel/{mod.rs,wayland.rs},udmabuf.rs}`.
