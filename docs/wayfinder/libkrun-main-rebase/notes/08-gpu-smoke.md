# 08 - The GPU smoke on the ABI-2 rebase: a venus regression and its fix

Ticket `08-gpu-smoke-against-abi2`, map `docs/wayfinder/libkrun-main-rebase`.
Runs 2026-09-28, same host, same image (`localhost/cang:latest` from the loaded
`cang.tar.gz`), same kernel (`6.12.109-hardened1`), harness
`tools/chromium-cang-smoke/chromium-smoke.sh`.

## The baseline that has to be beaten

The pre-rebase pin (libkrun `1.19.5-cang` = the fork's vendored rutabaga +
`krun_set_gpu_options3`) passed both modes on 2026-09-26 with

```
renderer=ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)) (0x00007480)), venus)
```

(`chromium-smoke/kernel-6.12.109e` and `kernel-6.12.109-waypipe`, kept in
`/home/dev/cang/disk/chromium-smoke/`). So the target is: same renderer string,
both modes, on the v2 pin.

## What the v2 pin did

`--gpu=drm` reached the guest with `/dev/dri/card0` + `renderD128`, the venus ICD
and the DRM capset path, and then:

- guest: `renderer=no-webgl`; ANGLE logs `vk_renderer.cpp initialize` →
  `Internal Vulkan error (-3)` → `eglInitialize Vulkan failed` → `Exiting GPU
  process due to errors during initialization`.
- host (`--log-level debug --preserve-debug`, libkrun's log in the preserved
  task's `helper.stderr.log`):

```
virtio_gpu: set_shm_region ... GET_shm_region
INFO  rutabaga_gfx::virgl_renderer] no valid GPU path provided
INFO  rutabaga_gfx::virgl_renderer] gl_version 46 - core profile enabled
INFO  rutabaga_gfx::virgl_renderer] proxy: proxy server with pid -1
ERROR rutabaga_gfx::virgl_renderer] failed to initialize drm renderer
WARN  rutabaga_gfx::rutabaga_core] error initializing gpu backend=virglrenderer, falling back to 2d.
ERROR init_or_kernel] [drm:virtio_gpu_get_capsets] *ERROR* timed out waiting for cap set 1
```

So the host renderer degraded to **2D**, and venus was never there.

## Three device-side causes (all fixed in `3d7af2c2`)

1. **No DRM node for virglrenderer.** The fork's vendored rutabaga opened
   `/dev/dri/renderD*` itself (`open_host_render_node`) and answered
   `get_drm_fd` from it. Upstream rutabaga answers that callback from its "GPU
   path" list, and the ABI-2 device registered only wayland/X11/pipewire paths,
   so virglrenderer got `-1` for the vrend winsys and VA-API video. Fix: register
   the first `/dev/dri/renderD*` that opens **read-write** (Mesa's vdrm mmaps its
   shmem buffer `PROT_WRITE`; `O_RDONLY` fails with EACCES) as
   `RUTABAGA_PATH_TYPE_GPU` whenever the DRM flag is set.
2. **`drm_renderer_init` failure killed the whole backend.** Upstream rutabaga
   always enables `ASYNC_FENCE_CB`; virglrenderer reacts to
   `ASYNC_FENCE_CB | DRM` with `drm_renderer_init(get_drm_fd())`, and on this host
   the native amdgpu probe fails (no virtio native context), which virglrenderer
   treats as fatal - `virgl_renderer_init` returns an error and rutabaga falls
   back to 2D, venus included. The fork's vendored rutabaga never set
   `ASYNC_FENCE_CB` (it passed cang's flag word through verbatim), so the block
   never ran before. Fix: with a render server descriptor present, do not ask for
   the DRM native-context capset (`device_capset_mask`) - that mode is in-process
   only, and the render server owns venus. The DRM *fd* stays (1).
3. **`num_capsets` mismatch.** The device reports
   `virtio_gpu_config.num_capsets` as the capset-mask bit count, so clearing the
   DRM bit in rutabaga but not in the device config left the guest asking for an
   index rutabaga does not have; the device never answers and the guest kernel
   logs `timed out waiting for cap set 4`. Fix: both sides compute the mask
   through `device_capset_mask` (this is what made the first attempt at fix (2)
   look like it had failed).

Evidence for (2)+(3): with fix (2) half-applied (rutabaga mask only) the log lost
`failed to initialize drm renderer` and gained
`[drm:virtio_gpu_get_capsets] *ERROR* timed out waiting for cap set 4`;
with both applied the guest reaches venus.

## Result with the fixes (local build, unpublished)

Both smoke modes pass, with the **same renderer string as the pre-rebase
baseline**:

```
$ tools/chromium-cang-smoke/chromium-smoke.sh --container localhost/cang:latest     --cang <cang 0.7.2 linked against 3d7af2c2> --out-dir .../v2-gpu
PASS  version            PASS  chromium-rc            PASS  webgl-vulkan
PASS  webgl-png          VERDICT: PASS (14 evidence files)
renderer=ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)) (0x00007480)), venus)

$ tools/chromium-cang-smoke/chromium-smoke.sh --waypipe ... --out-dir .../v2-waypipe
PASS  version  PASS chromium-rc  PASS webgl-vulkan  PASS webgl-png
PASS  waypipe-transport  PASS venus-presenting  PASS frame-presented
PASS  renderer-on-frame  PASS control-no-frame  VERDICT: PASS (25 evidence files)
presenting mode=waypipe wayland_display=cang-waypipe-0 … alive_after_dwell_secs=90 alive=yes
```

Notes for a re-run: the loaded image can be reused
(`--container localhost/cang:latest` with `CONTAINERS_STORAGE_CONF` pointing at
the run's `container-storage/storage.conf`), which skips the ~3 min podman load.
The smoke's `--cang` wants a package prefix whose `lib/cang` can be swapped by
exporting `CANG_LIBKRUN_LIBRARY=<libkrun.so.2>` - that is how the unpublished
library was exercised without re-pinning.

## Open item

The fix is committed on the fork (submodule commit `3d7af2c2`) but **not
published**, so the pinned `v2.0.0-cang.2` still degrades to 2D and the smoke
still fails against the pin. Publishing `v2.0.0-cang.3` and re-pinning is
[Publish the GPU fixes and re-pin](14-publish-gpu-fix.md).
