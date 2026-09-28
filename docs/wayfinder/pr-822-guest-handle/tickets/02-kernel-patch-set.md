---
label: wayfinder:research
title: Which Linux guest-side patch set gives 6.12.109 CREATE_GUEST_HANDLE?
status: closed
blocked_by: []
claimed_by: pi research child-2 (2026-09-28)
---

## Question

The pinned kernel (`libkrunfw v5.6.2-cang.1` = `linux-6.12.109` +
`v6.12.109-hardened1`) has **none** of the guest side: `VIRTGPU_PARAM` stops at 8,
`VIRTGPU_BLOB_FLAG_USE_MASK == 0x7`, `virtgpu_gem_prime_import()` is the plain
`drm_gem_prime_import()`, and `CONFIG_UDMABUF is not set` in all six configs.

The Linux side is also **in flux**: the PRIME-import change landed upstream
(`df4dc947c46b`, *"drm/virtio: Allow importing prime buffers when 3D is
enabled"*) and was **reverted on 2026-09-15** because it broke vrend. Val
Packett said he would re-send it gated on `VIRTIO_GPU_F_CREATE_GUEST_HANDLE`, and
posted a virtio-comment series (v2, 2026-09-03, thread starting
`20260903021442.423274-1-val@invisiblethingslab.com`) proposing feature bits
**6 (CREATE_GUEST_HANDLE)** and **7 (BLOB_CTX_ID_FIX)**.

1. Locate the public series: the merged commit, the revert, and the current
   (v2/v3) kernel series on lore.kernel.org / dri-devel. Give message-ids,
   URLs, dates and commit hashes.
2. Reduce it to **what must be patched onto 6.12.109** for the path to run:
   `CONFIG_UDMABUF`, `VIRTGPU_PARAM` 10, blob flag `0x8` inside the use mask,
   the PRIME-import gate, and - if the carried design uses the newer route - the
   `BLOB_CTX_ID_FIX` ctx_id behaviour. For each piece say whether it backports
   cleanly to 6.12, must be re-authored, or depends on the revert.
3. Confirm the **feature-bit numbering** as it stands (cang's kernel claims bit 5
   for `VIRTIO_GPU_F_FENCE_PASSING`; the spec series says 6 and 7) and name what
   the fork must never renumber.
4. Say whether `deps/wl-cross-domain-proxy` (merged at `cc64c65`; it probes
   `Param::CreateGuestHandle = 10` and opens its own `/dev/udmabuf`) needs any
   change under each design, or whether the flag originates purely in the
   kernel's PRIME import path.

## Deliverable

`notes/02-kernel-patch-set.md`, plus the raw patches/series it cites in the same
directory. Do not modify `deps/libkrunfw`; this ticket only collects the patch
set.

## Resolution (2026-09-28, pi research child-2)

**There is no single series to cherry-pick: the port is two series plus a
fork-authored gate.** Deliverable: `../notes/02-kernel-patch-set.md`, with 20
`02-raw-*` files beside it (both series as patch text, the reverts, the apply
matrix, primary sources).

1. **The prerequisite is missing from 6.12.109.** Vivek Kasireddy's
   `[PATCH v5 0/5] drm/virtio: Import scanout buffers from other devices`
   (2024-11-26) is the PRIME-import-as-guest-blob base
   (`virtgpu_dma_buf_init_obj()`, `virtgpu_gem_dma_buf_funcs`) that both the
   merged `df4dc947c46b` and Val's re-send build on. It first shipped in
   **v6.14**; 6.12.109's `virtgpu_gem_prime_import()` is literally
   `return drm_gem_prime_import(dev, buf);`. It must be **backported** - four of
   its six patches apply clean to a 6.12.109 tree, the `virtgpu_plane.c` hunk
   needs re-authoring.
2. **Val's re-send is not on lore.** The six-commit implementation lives only on
   `github.com/valpackett/linux-qclaptops` branch **`guest-handle`** @
   `3e6a365d2ac9` (2026-08-28), based on linux-next 20260901 - i.e. *before*
   the revert. The virtio-comment v2 (`20260903021442.423274-1`) proposed bits 6
   and 7; v3 (which would make `CREATE_GUEST_HANDLE` require `BLOB_CTX_ID_FIX`)
   was promised on 2026-09-04 and is **not posted**.
3. **The fork must author the unlock.** `df4dc947c46b` (merged 2026-02-12,
   backported only to 6.18.32-rc1/7.0.10-rc1, never 6.12.y) was reverted as
   `1e3b08de6327` (misc-fixes, 2026-09-20) after it broke vrend; Val's branch
   predates the revert, so its unconditional unlock does not apply. 6.12.109 has
   no `df4dc947` to revert, so the port writes the final conditional itself:
   `if (!vgdev->has_resource_blob || (vgdev->has_virgl_3d &&
   !vgdev->has_create_guest_handle)) return drm_gem_prime_import(...);`
4. **Apply state on 6.12.109** (dry runs, `02-raw-backport-apply-matrix.txt`):
   blob flag `0x8` + `USE_MASK`, the flag stamp, the `.open/.close` funcs and the
   `verify_blob` ctx_id hunk apply **clean**; the `virtio_gpu.h` bit table,
   `uapi` param 10 (skip 9) and the `kms.c`/`drv.h` hunks need **re-authoring**,
   purely because upstream's context carries `VIRTIO_GPU_F_BLOB_ALIGNMENT = 5`
   and param 9, which 6.12.109 does not.
5. **Numbering decision, standardised:** keep cang's `5 =
   VIRTIO_GPU_F_FENCE_PASSING` (`patches/0018`), add `6 =
   CREATE_GUEST_HANDLE`, `7 = BLOB_CTX_ID_FIX` exactly as upstream numbers them,
   and **never import upstream's `BLOB_ALIGNMENT = 5`** - mainline has since
   assigned 5 to it, so the collision is latent, not hypothetical. This is the
   same rule as ticket 03's (the Rust side already says 5/6 and advertises
   neither).
6. **`deps/wl-cross-domain-proxy` needs no change.** The flag is stamped purely
   by the kernel's PRIME-import path; the proxy only probes `Param::CreateGuestHandle
   = 10` and opens its own `/dev/udmabuf`, and never passes
   `BlobFlags::CREATE_GUEST_HANDLE`.
7. **Only one config line:** `CONFIG_UDMABUF=y` in all six
   `deps/libkrunfw/config-libkrunfw_*`.

No stable or mainline line has param 10 or blob flag `0x8` as of 2026-09-28
(mainline HEAD tops out at bit 5 `BLOB_ALIGNMENT` / param 9; `6.12.y` HEAD ==
`v6.12.109` at param 8 / mask `0x7`), so there is nothing to cherry-pick from a
release - the whole guest side is fork-carried.
