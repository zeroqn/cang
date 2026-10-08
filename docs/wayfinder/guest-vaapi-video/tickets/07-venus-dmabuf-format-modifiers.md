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

## First pass (2026-10-08, static analysis; no live repro - this box has no host compositor)

**Q1 - which modifiers?** The guest advertises the **host renderer's modifier list verbatim**:
`vn_wsi.c`'s `vn_wsi_init` sets `wsi_device.supports_modifiers` from the host's
`EXT_image_drm_format_modifier`, and `vn_physical_device.c`'s `vn_GetPhysicalDeviceFormatProperties2`
→ `vn_sanitize_format_properties` only masks YCbCr feature flags - it never intersects the list with
what *guest-allocatable* resources can back. So the guest tells its clients it supports tiled
modifiers (AMD GFX11 families, DCC variants) while the guest's GBM/virgl shared resources are
**LINEAR only** - `virglrenderer`'s pipe-resource layout reports LINEAR for them (cang's
`virglrenderer-gbm-layout-linear-modifier.patch`), and guest-init sets
`GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm`.

**Q2 - which layer fails?** The mpv abort is the **resource-properties query path**, not
modifier-list negotiation: guest `vn_GetMemoryFdPropertiesKHR` → `vn_get_memory_dma_buf_properties` →
`vn_renderer_bo_create_from_data`, with the modifier arriving as `0xffffffffffffffff`
(`DRM_FORMAT_MOD_INVALID`) - which is the same value `tools/chromium-cang-smoke/README.md:198-207`
records for the chromium flavour, and that README documents the chromium side as *fixed* by making the
guest publish the host's real layout as LINEAR. `can_import_image` passes before the properties query
in the mpv case, so the list itself is not what rejects.

**Q3 - smaller fix?** Waypipe's dmabuf path - one guest process, and cang already controls
`GBM_BACKENDS_PATH` and the venus ICD selection - rather than a guest compositor rendering through
virgl.

**Not established:** no live reproduction (no compositor on this box), and the guest waypipe's
`DmabufDevice` being the venus ICD is inferred from guest-init's `VK_ICD_FILENAMES` pin plus waypipe's
Vulkan usage.

**Next concrete step (no C change, discriminating):** re-run the captured `smoke-mp2` arm with the
guest's venus WSI forced LINEAR (`MESA_ENV` addition `VN_PERF=no_tiled_wsi_image`) and the waypipe
server started with `--test-skip-vulkan`, then require the render server to log neither
`invalid res_id` nor CS error and mpv to reach its first frame. That separates the
cross-context-resource explanation from the modifier one. Also check whether the image ships
`dri_gbm.so` at all and either point `GBM_BACKENDS_PATH` at it or add it to the image's mesa runtime.
It needs a host with a compositor (not this box).

## Experiment: the modifier theory is refuted; the failure is a venus ring hang (2026-10-08)

Ran the discriminating step on this box: headless weston 15.0.1 (host) + waypipe 0.11.0 client +
`cang --gpu=drm --waypipe` guest, presenting client = **mpv v0.41.0 from the image** with `--vo=gpu`
(waylandvk = Vulkan through venus), `--hwdec=no`:

| arm | variant | client rc | playback | host frame |
| --- | --- | --- | --- | --- |
| A | baseline `--vo=gpu` | **134** | no | blank |
| B | `VN_PERF=no_tiled_wsi_image` (LINEAR WSI) | **134** | no | blank |
| C | control `--gpu-api=opengl` | 124 (killed after playing) | **yes** | real video frame |
| D | baseline + `MESA_LOG_LEVEL=debug VN_DEBUG=wsi,result` | 134 | no | blank |
| E | `VN_PERF` + the same debug | 134 | no | blank |
| S | baseline under strace | 134 | no | blank |

The env knob *is* live - arm E logs `rejecting non-linear wsi image format modifier <0x…>` where arm D
logs `rejecting multi-plane (2/3) modifier …`, i.e. the WSI modifier policy really changes - and both
then create the swapchain (`vn_wsi_create_image` x3) and die in the identical place:

```
MESA-VIRTIO: debug: stuck in ring seqno wait with iter at 4096
MESA-VIRTIO: debug: aborting on expired ring alive status at iter 4096
```

**So the abort is a venus ring hang** (the host stops advancing the ring after swapchain creation),
not a dma-buf modifier or import failure. Under strace the venus `VIRTGPU_EXECBUFFER` ioctls all
return 0, then ~5 s of quiet, then a self-raised SIGABRT (`SI_TKILL`) - no failing ioctl.

Also settled by this run:

- **`dri_gbm.so` is shipped and pointed at** (`/usr/lib/cang-mesa-runtime/lib/gbm/dri_gbm.so`,
  in-guest `GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm`), so that half of the earlier
  proposal does not apply;
- **waypipe 0.11.0 has no `--test-skip-vulkan`** (host and image), so that half cannot be run;
- the 2026-10-02 `invalid res_id 15` / CS-error sequence was **not** reproduced on the current practice
  image - the failure today is a silent ring hang. The host-side absence of `vkr:` lines is *not*
  evidence either way: the shipped render server binary has no `VIRGL_LOG_FILE`/`VIRGL_LOG_LEVEL`
  support and the messages are INFO-level.

**Redirect:** ticket 07 is no longer a format-modifier ticket. The next step is to instrument the venus
ring/CS - build cang's virglrenderer with logging (cang can, and the marker recipe is known) and/or
bisect the submit that never completes - with the GL path (arm C) as the healthy control.
