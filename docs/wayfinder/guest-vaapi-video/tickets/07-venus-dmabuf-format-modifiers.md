---
label: wayfinder:research
title: Venus dma-buf format-modifier imports block Vulkan-presenting clients
status: open
blocked_by: []
claimed_by:
---

## Question

Vulkan clients in the guest fail on dma-buf format-modifier imports, so anything
that presents through venus aborts instead of drawing. Who owns the mismatch -
waypipe, venus, or the modifier the host's driver hands out - and can a guest
Vulkan client present through the Waypipe display at all?

Evidence (2026-10-02, `--gpu=drm --waypipe` guest):

- `mpv`'s **default** video output (`--vo=gpu`, `--gpu-api=auto` => Vulkan)
  aborts with `rc=134` in both hwdec arms, with the render server reporting
  `vkr: failed to query resource props: invalid res_id 15`,
  `vkGetMemoryResourcePropertiesMESA resulted in CS error` and
  `ring_submit_cmd: vn_dispatch_command failed` - before any decode happens;
  `--gpu-api=opengl` presents normally;
- the same wall was recorded earlier for Chromium (`vk_helpers.cpp initExternal`
  and waypipe's dmabuf import failing with
  `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`, the host compositor's
  AMD modifier leaking into the guest), and the chromium smoke's
  `tools/chromium-cang-smoke` blocks dmabuf on the waypipe side to avoid it;
- with `GBM_BACKENDS_PATH` fixed the guest now *does* allocate GBM buffers and
  Waypipe carries them as dma-bufs, so the transport half works; what fails is
  venus's import/query of those buffers.

Scoping questions for the first pass: which modifier the guest's driver
advertises for GBM/venus allocations and which one the host compositor hands out;
whether the failure is in venus's `vkGetMemoryResourcePropertiesMESA` path or in
the format-modifier list negotiation; and whether `waypipe`'s dmabuf path or a
guest-side compositor rendering with virgl (no venus) is the smaller fix.

## Acceptance

A Vulkan-presenting client in a `--gpu=drm --waypipe` guest that reaches its
first frame through the host compositor (mpv with its default video output, or
Chromium with `--use-angle=vulkan`), with the render server logging no
`invalid res_id`/CS error.
