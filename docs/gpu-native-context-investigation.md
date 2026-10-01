# GPU native context in the cang guest — investigation log

Status: **answered — not possible on this host, and the gate is host-side.**
Question (2026-09-30, after the libkrunfw re-base to linux-7.2.7): now that the
guest kernel is new, can `--gpu=drm` use virtio-gpu **DRM native context** so
the guest gets the host's own accelerated stack — native Vulkan (RADV), EGL/GL
(radeonsi) and VA-API video decode/encode (VCN) — instead of the venus/virgl
translation paths?

## Verdict

**No.** "Native context" is a *host* capability: the guest's DRM capset is
served by cang's own virglrenderer, and that renderer can only run a native
context against a **real native DRM node** on the host. cang's host has none:
its only render node is itself a virtio-gpu device (`driver=virtio_gpu`,
DRM version 0.1.0), because the dev box is itself a GPU-passthrough guest of
the machine below it. virglrenderer's native renderer init on that node fails,
and because rutabaga always enables `ASYNC_FENCE_CB`, advertising the capset
fails the *whole* virglrenderer init — rutabaga falls back to 2D and the guest
loses venus too. That is exactly why `device_capset_mask` strips the DRM bit
when a render server owns venus, and the live guest confirms the capset is not
offered.

The 7.2.7 guest kernel changes nothing here: the guest already supports
contexts and enumerates the capsets the device advertises. The gate is the
host's DRM node, not the guest kernel.

## What the guest actually gets today (live, kernel 7.2.7-hardened1)

| | host (`/dev/dri/renderD128`, virtio node) | cang guest |
|---|---|---|
| Vulkan | RADV NAVI33 (native) | **venus** — `Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33))`, `DRIVER_ID_MESA_VENUS` |
| EGL/GL | radeonsi 4.6 (native) | **virgl/vrend** — `virgl (AMD Radeon RX 7600M XT (radeonsi, navi33, ACO, DRM 3...))`, GL 4.6, GLES 3.2 |
| VA-API | H.264/HEVC/VP9/AV1 decode + H.264/HEVC/AV1 encode | H.264/HEVC/VP9/AV1/JPEG decode + H.264/HEVC encode, through the vrend video path (was `VAProfileNone` only before that path was enabled — see `docs/vaapi-video-investigation.md`) |
| native context capset | n/a (client of the machine below) | **not advertised**: cap set ids 1, 2, 4, 5 (virgl, virgl2, venus, cross-domain); id 6 (DRM) absent |

So Vulkan and GL are accelerated in the guest through virgl/venus translation,
not native context. Hardware video is the one capability native context would
have delivered that needed another route, and it now comes from the *vrend video*
path instead: `--gpu=drm` guests get the host's VA-API profiles and decode with
them (the fix and its evidence are in `docs/vaapi-video-investigation.md`; the
missing piece was rutabaga dropping cang's `VIRGL_RENDERER_USE_VIDEO` bit, not
anything in the guest kernel).

Native context *would* deliver the host's own stack, and would work on a host
that has a real GPU node (the physical machine, or a cang host with the GPU
passed through): nixpkgs' virglrenderer 1.3.0 is built with
`-Ddrm-renderers=amdgpu-experimental,asahi,msm`, so only the node is missing
here. It is a nesting limit, not a cang one.

## Evidence

### 1. The host's render node is not a native device

```
/dev/dri/renderD128: driver='virtio_gpu' date='0' desc='virtio GPU' drm 0.1.0
amdgpu_device_initialize rc=-9    (stderr: "DRM version is 0.1.0 but this
                                   driver is only compatible with 3.x.x.")
```

(`/sys/class/drm` has a single virtio-pci/virtio-mmio device; `lsmod` has
`virtio_gpu`, no `amdgpu`.)

### 2. virglrenderer cannot initialise its native renderer on that node

A ctypes harness calling `virgl_renderer_init` exactly as libkrun does
(`get_drm_fd` returning an fd on `/dev/dri/renderD128`):

```
flags 0x480 (DRM|NO_VIRGL)                          -> rc=0   (probe not run)
flags 0x580 (DRM|NO_VIRGL|ASYNC_FENCE_CB)           -> rc=-19 ENODEV
   [virgl log] failed to initialize drm renderer
flags 0x583 (..|USE_EGL|THREAD_SYNC)                -> rc=-19
flags 0x5c3 (VENUS|NO_VIRGL|DRM|USE_EGL|THREAD_SYNC|ASYNC) -> rc=-19
```

`ASYNC_FENCE_CB` is the trigger, and rutabaga always sets it
(`rutabaga_gfx` `rutabaga_core.rs:1195` `.use_async_fence_cb(true)`), while the
DRM flag is set iff the DRM capset bit is
(`rutabaga_core.rs:1374` `.use_drm(capset_enabled(RUTABAGA_CAPSET_DRM))`).
A failing `virgl_renderer_init` makes rutabaga log
`error initializing gpu backend=virglrenderer, falling back to 2d` — venus
included. Hence `device_capset_mask` in libkrun strips the DRM bit whenever a
render server owns venus (`deps/libkrun/src/devices/src/virtio/gpu/virtio_gpu.rs`),
and ticket 08 already recorded the same run-time failure
(`docs/wayfinder/libkrun-main-rebase/notes/08-gpu-smoke.md`).

### 3. Live guest (cang 0.11.2, libkrun v2.0.0-cang.5, libkrunfw 7.2.7-hardened1)

Command: `cang --image localhost/cang:latest --gpu=drm --mem 4 --root
--seccomp=off --landlock=off --log-level debug -- sh /workspace/probe.sh` in a
btrfs workspace with a hermetic podman store (image tarball
`/nix/store/6miplc4zki1b5wnmvaywrbsd6kgi3640-cang.tar.gz`, loaded with
`podman load`; the GPU facts rest on the guest's own versions below, below -
kernel `7.2.7-hardened1`, mesa `26.1.8` at the host's own store path), console
captured with `script -q -e`. Excerpts:

```
[drm] features: +virgl +edid +resource_blob +host_visible -fence_passing
[drm] features: +context_init -create_guest_handle -blob_ctx_id_fix
[drm] number of cap sets: 4
[drm] cap set 0: id 1, max-version 1, max-size 308
[drm] cap set 1: id 2, max-version 2, max-size 1408
[drm] cap set 2: id 4, max-version 0, max-size 160
[drm] cap set 3: id 5, max-version 0, max-size 16
[drm] Initialized virtio_gpu 0.1.0 for virtio-mmio.0 on minor 0

# venus ICD (the guest default, VK_ICD_FILENAMES=virtio_icd)
deviceName = Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33))
driverID   = DRIVER_ID_MESA_VENUS

# radeon ICD (i.e. native context: radeonsi/RADV over amdgpu)
MESA: error: vdrm_device_connect failed
radv/amdgpu: failed to initialize device.
WARNING: failed to initialize winsys (VK_ERROR...)

# eglinfo (GBM platform)
EGL driver name: virtio_gpu
OpenGL core profile renderer: virgl (AMD Radeon RX 7600M XT (radeonsi, navi33, ACO, DRM 3...))
OpenGL core profile version: 4.6 (Core Profile) Mesa 26.1.8
OpenGL ES profile version: OpenGL ES 3.2 Mesa 26.1.8

# vainfo (both LIBVA_DRIVER_NAME=virtio_gpu and =radeonsi)
Driver version: Mesa Gallium driver 26.1.8 for virgl (AMD Radeon RX 7600M XT (radeonsi, ...))
      VAProfileNone                   :	VAEntrypointVideoProc
```

Full console log of that run: `docs/gpu-native-context-investigation.console.log`.
The guest probe staged `vulkan-tools`,
`libva-utils`, `mesa-demos`, `libva` and `libdrm` from the host store into the
guest's `/nix/store`; the image and host share the nixpkgs pin, so the mesa
store path (`/nix/store/dqdfhilmkqpijpa5jhmyqpjgh4mgpzlp-mesa-26.1.8`) is the
same on both sides.

### 4. For contrast: the host is itself a native-context client

On the host, `AMD_DEBUG=info eglinfo` prints radeonsi device info
(`dev_filename = /dev/dri/card1`, NAVI33), `VK_ICD_FILENAMES=radeon_icd`
enumerates a working RADV device, and `vainfo` lists the full VCN profile set —
while the venus ICD enumerates nothing. The dev box gets its GPU through the
same virtio-gpu native-context mechanism, from the machine below it; its
render node is a *client* endpoint, not something libdrm_amdgpu (and therefore
virglrenderer's native renderer) can drive.

## Reproduce

Is the host node a native device?

```python
import ctypes, os
amdgpu = ctypes.CDLL("/nix/store/6ml3xhfzviaagy9qi6sicrga3yxx6chl-libdrm-2.4.133/lib/libdrm_amdgpu.so.1")
fd = os.open("/dev/dri/renderD128", os.O_RDWR)
maj, mnr, dev = (ctypes.c_uint32() for _ in range(2)) + (ctypes.c_void_p(),)
print(amdgpu.amdgpu_device_initialize(fd, ctypes.byref(maj), ctypes.byref(mnr), ctypes.byref(dev)))
```

Can virglrenderer serve a native context on it? Call
`virgl_renderer_init(NULL, flags, &callbacks)` from
`/nix/store/*virglrenderer-1.3.0/lib/libvirglrenderer.so.1` with
`callbacks.version = 4` and `get_drm_fd` returning an fd on the render node
(struct in the `virglrenderer` dev output's `include/virgl/virglrenderer.h`);
`VIRGL_RENDERER_DRM (1<<10) | VIRGL_RENDERER_ASYNC_FENCE_CB (1<<8)` is the
combination rutabaga produces when the DRM capset is enabled and returns
`-19` on this host.

The live guest run is the `cang --gpu=drm ... -- sh /workspace/probe.sh`
command quoted above; the guest probe itself is the one whose output is in the
console log.
