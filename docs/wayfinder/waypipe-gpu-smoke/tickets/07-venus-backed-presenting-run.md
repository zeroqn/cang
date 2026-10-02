---
label: wayfinder:research
title: Venus-backed presenting run through waypipe
status: closed
blocked_by: []
claimed_by: bob (pi session 2026-09-22)
---

## Question

Can a guest Chromium render on **venus** and still present through the waypipe display -
i.e. hardware acceleration, not just a working transport?

## Resolution

**Yes.** Working recipe (evidence:
`<host-btrfs-disk>/chromium-smoke/t06-evidence/anglevk-nogpu/` and `anglevk/`):

Guest Chromium:

```sh
GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm \
chromium --ozone-platform=wayland --no-sandbox --disable-gpu-sandbox \
         --use-angle=vulkan \
         --user-data-dir=/tmp/c --window-size=640,480 \
         --app=file:///workspace/smoke/wp-page.html
```

Host side: weston headless+GL+`--debug` (ticket 01) and the waypipe client started with
dmabuf **blocked** (`waypipe -n ... client`, ticket 02).

Measured (1) with dmabuf blocked, no strace: `gpu_crashes=0`, GPU process alive, and the
page present in **both** the early and late host screenshots (`#ff00ff` x13094, `#ffffff`
x2183). (2) The same configuration under `strace -f -e ioctl` proves venus is what renders:
`VIRTGPU_CONTEXT_INIT=5`, `VIRTGPU_EXECBUFFER=42`, `VIRTGPU_MAP=4`.

### What was actually wrong (three separate things, previously conflated)

1. **`--enable-features=Vulkan` is the harmful flag, not `--ozone-platform=wayland`.**
   That feature switches the display compositor to Vulkan, which needs a `VkSurfaceKHR`;
   ozone-wayland does not implement `CreateVulkanSurface` and logs `'--ozone-platform=wayland'
   is not compatible with Vulkan`. The host emits the same message and carries on with 0
   crashes, so it is not fatal - but with the feature enabled in the guest the GPU process
   crash-loops (5 crashes) and the page never paints. `--use-angle=vulkan` alone is the
   correct knob: ANGLE (WebGL/raster) goes to Vulkan, the compositor stays off it.
2. **`GBM_BACKENDS_PATH` is genuinely missing** (ticket 06): with it, ozone can init a render
   node and the GPU process reaches venus; without it, Chromium silently degrades to
   software/shm.
3. **dmabuf must be blocked for now.** With dmabuf enabled the GPU process crashes 3-5x:
   ANGLE's `vk_helpers.cpp initExternal` and waypipe's dmabuf import both fail with
   `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT` (-1000158000). The host
   compositor's AMD modifiers reach the guest over the waypipe display and venus rejects
   them. Blocking dmabuf (`--no-gpu`) makes Chromium use `wl_shm`, which transports fine.

### Trade-off

Rendering is GPU-accelerated; the **transfer is not zero-copy** (`wl_shm` copy) while dmabuf
is blocked. Closing that gap means fixing modifier negotiation between the guest (venus/
virtio-gpu) and the host compositor's dmabuf feedback - real work, and not required for this
map's transport baseline.

## Update (2026-10-02): the test-time `GBM_BACKENDS_PATH` override is no longer needed

This run needed `GBM_BACKENDS_PATH` supplied by hand. guest-init now exports it as part of the
`--gpu=drm` Mesa environment (`crates/cang-guest-init/src/guest_init/components/wayland.rs`),
so the accelerated run works without the override, and with dmabufs enabled on the waypipe
client the guest's frames travel as dma-bufs (24 buffer creations in the host waypipe log,
none before the fix). The dmabuf-blocked recipe recorded in this ticket therefore has a
working alternative now, though the modifier wall noted there is still real for Vulkan
clients: mpv's default Vulkan video output aborts on it (`vkr: failed to query resource
props: invalid res_id 15`).