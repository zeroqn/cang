# cang-chromium

Start Chromium **inside a cang guest** with the guest's GPU acceleration wired up:
Vulkan through the venus ICD, EGL through the image's mesa runtime, VA-API video
through the guest's virtio-gpu VA driver.

The script is installed in the cang image as `cang-chromium` (`nix/image/layers.nix`
embeds this file into the browser layer), so a guest shell has it on PATH; the file
here is the source of truth.

```bash
cang-chromium headless [URL] [-- <extra chromium arguments>]
cang-chromium gui      [URL] [-- <extra chromium arguments>]
cang-chromium --check
```

| mode | what it does |
| --- | --- |
| `headless` | Chromium's new headless mode: no display, no compositor. WebGL, screenshots, printing, page automation. |
| `gui` | A Chromium window on the guest's Wayland display — in a cang guest the one `cang --gpu=drm --waypipe` provides — presented on the host's own compositor. |
| `--check` | Reports the guest GPU setup and what Chromium actually got, and exits non-zero unless WebGL renders on the GPU and the guest driver advertises VA-API H.264 decode. |

## The guest has to be a `--gpu=drm` guest

```bash
cang --gpu=drm            -- <command>     # headless mode works
cang --gpu=drm --waypipe  -- <command>     # ... and `gui` has a display to open on
```

Without `--gpu=drm` the guest has no DRM render node, no venus Vulkan ICD and no
VA-API driver, so there is nothing to accelerate with. The script stops with that
explanation (exit 2) instead of starting a software-rendered browser whose window
looks fine:

```
cang-chromium: this guest has no GPU acceleration, because cang was not started
               with --gpu=drm. ...
                   cang --gpu=drm [--waypipe] -- <command>
```

`gui` additionally needs a Wayland display: it fails with the same kind of message
if `WAYLAND_DISPLAY` is unset or its socket is missing.

## What it sets up

The environment `cang --gpu=drm`'s guest-init exports, filled in if the shell lost
it (never `VK_DRIVER_FILES`: pinning that would override the ICD choice an EGL
client — Chromium's ANGLE among them — makes for itself):

| variable | value |
| --- | --- |
| `VK_ICD_FILENAMES` | `<mesa runtime>/share/vulkan/icd.d/virtio_icd.x86_64.json` (venus) |
| `__EGL_VENDOR_LIBRARY_FILENAMES` | `<mesa runtime>/share/glvnd/egl_vendor.d/50_mesa.json` |
| `LIBGL_DRIVERS_PATH` | `<mesa runtime>/lib/dri` |
| `GBM_BACKENDS_PATH` | `<mesa runtime>/lib/gbm` |
| `LIBVA_DRIVERS_PATH` | `<va runtime>/dri:<mesa runtime>/lib/dri` |
| `LD_LIBRARY_PATH` | `<gpu runtime>/lib` (glvnd's `libEGL.so.1` dispatcher) |

and the flags the guest GPU path needs:

| flag | why |
| --- | --- |
| `--use-angle=vulkan` | WebGL and raster go through ANGLE's Vulkan backend, which is venus in this guest. |
| `--disable-vulkan-surface` | ANGLE's Vulkan WSI/swapchain never completes over venus; the GPU process drives venus and then stalls and aborts (`exit_code=6`) if it is left enabled. |
| `--no-sandbox --disable-gpu-sandbox` | the guest has no usable Chromium sandbox. |
| `--ignore-gpu-blocklist` | the guest GPU is not on Chromium's allowlist, so both the WebGL path and the video decoder would be refused before anything is tried. |
| `--enable-features=…Vaapi…` | Chromium's VA-API decoder/encoder feature names; the unknown ones are ignored. |
| `--enable-features=Vulkan,UseSkiaRenderer` | **headless only.** It moves the display compositor onto Vulkan, which needs a `VkSurfaceKHR` the ozone-wayland path does not implement — in `gui` mode that crash-loops the GPU process, so the `gui` flag set deliberately leaves it out. |

`--enable-logging=stderr` is on so a failed GPU process is visible in the output.

## `--check`

```bash
$ cang-chromium --check
cang Chromium acceleration check

chromium               Chromium 154.0.8037.57
mode                   check
render node            /dev/dri/renderD128
venus ICD              /usr/lib/cang-mesa-runtime/share/vulkan/icd.d/virtio_icd.x86_64.json
EGL vendor             /usr/lib/cang-mesa-runtime/share/glvnd/egl_vendor.d/50_mesa.json
GBM backends           /usr/lib/cang-mesa-runtime/lib/gbm
GL drivers             /usr/lib/cang-mesa-runtime/lib/dri
VA drivers             /usr/lib/cang-va-runtime/dri:/usr/lib/cang-mesa-runtime/lib/dri
VA-API profiles        VAProfileH264Main:VAEntrypointVLD,VAProfileH264Main:VAEntrypointEncSlice
renderer               ANGLE (Mesa, Vulkan 1.4 … (venus …))
verdict                WebGL is hardware accelerated (ANGLE (Mesa, Vulkan 1.4 …))
video decode           Chromium reports powerEfficient=false (informational)
VA-API decode          H.264 VLD present (VAProfileH264Main:VAEntrypointVLD,...)

RESULT: PASS - Chromium is using the guest's GPU (venus Vulkan, VA-API video)
```

The check runs one headless Chromium against a page that reports the unmasked WebGL
renderer and `navigator.mediaCapabilities`, and scores two things: the renderer is
not a software rasteriser (`SwiftShader`, `llvmpipe`, `softpipe`, or no WebGL at
all), and the guest's VA driver advertises an H.264 `VAEntrypointVLD` profile. The
`powerEfficient` answer Chromium gives for video is printed but **not** scored: it
is a heuristic that reports `false` even on a host with a working VA-API stack, so
it is not evidence either way. `vainfo` is the VA-API evidence.

## Notes

* `--print-command` prints the resolved Chromium command (with the environment
  already applied) and exits, which is the quickest way to see what a mode does.
* `--timeout SECS` bounds the run; without it the script `exec`s Chromium so
  signals reach it directly.
* `--user-data-dir`, `--window-size` and any arguments after `--` are passed
  through to Chromium.
* The runtime directories are overridable (`CANG_MESA_RUNTIME_DIR`,
  `CANG_GPU_RUNTIME_DIR`, `CANG_VA_RUNTIME_DIR`, `CANG_CHROMIUM`) — that is what
  lets the script be exercised outside the image, and what a downstream image with
  a different layout would set.
