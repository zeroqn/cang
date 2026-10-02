---
label: wayfinder:research
title: Guest Chromium GPU process in the waypipe run
status: closed
blocked_by: []
claimed_by: bob (pi session 2026-09-22)
---

## Question

In the ticket 03 prototype the presenting guest Chromium never initialised a GPU device:
`MESA-LOADER: failed to open dri: /run/opengl-driver/lib/gbm/dri_gbm.so: cannot open shared
object file` and `Failed to initialize drm render node handle`, after which its buffers are
`wl_shm`. Yet the same image renders on venus in the headless smoke, where the guest env
carries `LIBGL_DRIVERS_PATH` and friends.

Find out:

1. Which env/paths the *headless* path sets that the *waypipe* path does not (compare the
   guest env in both runs, and `nix/image/config.nix` plus
   `crates/cang-guest-init/src/guest_init/components/*`).
2. Whether pointing the GBM/dri loader at the guest's mesa (`LIBGL_DRIVERS_PATH`,
   `__EGL_VENDOR_LIBRARY_FILENAMES`, `VK_DRIVER_FILES`) makes the windowed GPU process come
   up on venus, and whether it then presents at all (this may run straight into the
   venus-present wall).
3. Whether it matters for this effort: the charter says the presenting run may be
   renderer-agnostic. The question is only worth answering if it changes a scored check or
   reveals an image env gap that also affects ordinary waypipe use.

## Deliverable

A short verdict plus the env diff, and - if the fix is a one-liner in the image or the smoke
- a recommendation on whether to take it here or leave it to the uv/venus follow-on effort.

## Resolution

Chased to the bottom. There **is** a real env gap, but it is not what blocks venus: the
blocker is Chromium's own platform rule, plus a waypipe/virtio-gpu dmabuf gap.

### 1. The env gap is real

Guest-init exports `MESA_ENV` (`crates/cang-guest-init/src/guest_init/components/wayland.rs`)
when `CANG_GPU_DRM` is set - `LIBGL_DRIVERS_PATH`, `__EGL_VENDOR_LIBRARY_FILENAMES`,
`VK_DRIVER_FILES` (call site `runtime/cang.rs:190`) - and all three were present in the
waypipe guest. **`GBM_BACKENDS_PATH` is set nowhere in the repo**, so Chromium's ozone GBM
loader searched the NixOS default `/run/opengl-driver/lib/gbm` (absent in the guest):

```text
MESA-LOADER: failed to open dri: /run/opengl-driver/lib/gbm/dri_gbm.so: cannot open
shared object file (search paths /run/opengl-driver/lib/gbm, suffix _gbm)
WARNING ui/ozone/platform/wayland/ozone_platform_wayland.cc:278 Failed to initialize
drm render node handle.
```

The guest does ship the backend: `/usr/lib/cang-mesa-runtime/lib/gbm/dri_gbm.so`.

### 2. A/B in the same harness (windowed Chromium, `--ozone-platform=wayland`, 22s)

| configuration | dri_gbm / render-node lines | `not compatible with Vulkan` | GPU process crashes | page painted (host screenshot) |
| --- | --- | --- | --- | --- |
| control: `--use-angle=vulkan`, no GBM var | 6 | 1 | **0** | **yes** - `#ff00ff` x13094, `#ffffff` x2183 |
| `GBM_BACKENDS_PATH` + same Vulkan flags | **0** | 3 | **5** | no (only weston greys) |
| `GBM_BACKENDS_PATH`, no Vulkan flags (GL/EGL) | 0 | 0 | 0 | no - connection died (below) |

Evidence: `<host-btrfs-disk>/chromium-smoke/t06-evidence/{control,gbm,glgbm}/`.

### 3. What actually blocks venus-backed presentation

1. **Chromium refuses Vulkan on the Wayland platform.** With the render node finally
   working, Chromium tries the GPU path and then rejects it itself:

   ```text
   ERROR ui/ozone/platform/wayland/gpu/wayland_surface_factory.cc:249
   '--ozone-platform=wayland' is not compatible with Vulkan. Consider switching to
   '--ozone-platform=x11' or disabling Vulkan
   ```

   followed by a GPU-process crash loop (`exit_code=6`, `8704`). So `--use-angle=vulkan`
   over a waypipe display cannot produce venus presentation; it makes things *worse* than
   the control, which degrades to software and still paints.
2. **The dmabuf path fails inside waypipe.** Without the Vulkan flags but with a working
   GBM backend, Chromium does export dmabufs (it binds `zwp_linux_dmabuf_v1`), and then the
   **guest-side waypipe server** cannot import them:

   ```text
   libwayland: wl_display#1: error 0: waypipe-server internal error: src/dmabuf.rs:2093:
   Failed to create Vulkan image when importing dmabuf:
   ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT
   ```

   which kills the connection and the app. That is why the working transfer is `wl_shm`
   (matching ticket 03), and why the handshake's `may use dmabufs: true` is a trap rather
   than a capability in this configuration.

### 4. Does it matter for this effort?

No. The charter already allows the presenting run to be renderer-agnostic, and the control
configuration - the one the ticket 03 prototype used - paints the page reliably because it
degrades to the shm path. Recommendations:

- For the smoke: keep the presenting run on the shm/software path (do **not** set
  `GBM_BACKENDS_PATH` for it, and do not pass `--use-angle=vulkan` with a Wayland
  platform). If a GPU path is ever wanted, the guest waypipe server needs `--no-gpu`.
- Product follow-ons (not this map, listed under Out of scope on the map): add
  `GBM_BACKENDS_PATH` to `MESA_ENV` in guest-init (a genuine gap: the guest's GBM backends
  are unreachable), and investigate waypipe's dmabuf import against virtio-gpu/venus format
  modifiers. Both are real work with their own risk, and neither is needed for a trustworthy
  transport baseline.

## Correction (same session, superseded by *Venus-backed presenting run through waypipe*)

The conclusion above was too strong: the `'--ozone-platform=wayland' is not compatible
with Vulkan` message is **not** fatal by itself (the host emits it too, with 0 GPU crashes),
and venus-backed presentation **is** reachable. What actually crash-looped the GPU process
was passing `--enable-features=Vulkan` (Vulkan for the display compositor, which needs a
`VkSurfaceKHR` ozone-wayland does not implement). With `--use-angle=vulkan` *without* that
feature, the guest GPU process runs cleanly and uses venus. Details and evidence in the new
ticket; the GBM/dri finding here stands and is in fact required for the accelerated run.

## Follow-on closed (2026-10-02): `GBM_BACKENDS_PATH` is now in `MESA_ENV`

The product follow-on this ticket listed - "add `GBM_BACKENDS_PATH` to `MESA_ENV` in
guest-init (a genuine gap: the guest's GBM backends are unreachable)" - is done
(`crates/cang-guest-init/src/guest_init/components/wayland.rs`, with a unit-test
assertion). Verified in a live `--gpu=drm --waypipe` guest: `MESA-LOADER: failed to open
dri` disappears (3+ per run to 0) and a presenting client's buffers travel as **dma-bufs
instead of `wl_shm`** (24 dmabuf buffer creations in the host waypipe log, where the same
probe previously produced none). So the second half of this ticket's finding is closed, and
the "keep the presenting run on the shm/software path" advice above no longer applies: the
guest can now allocate GBM buffers, and presentation over the dmabuf transport works.

The other follow-on (waypipe's dmabuf import against virtio-gpu/venus format modifiers)
remains open, and is now reproduced independently by mpv: its default Vulkan video output
aborts with `vkr: failed to query resource props: invalid res_id 15` /
`vkGetMemoryResourcePropertiesMESA resulted in CS error`.