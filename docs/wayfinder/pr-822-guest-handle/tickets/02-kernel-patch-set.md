---
label: wayfinder:research
title: Which Linux guest-side patch set gives 6.12.109 CREATE_GUEST_HANDLE?
status: open
blocked_by: []
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
