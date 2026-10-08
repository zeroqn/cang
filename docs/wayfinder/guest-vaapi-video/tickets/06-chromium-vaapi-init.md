---
label: wayfinder:research
title: Chromium's VA-API decoder instantiates but does not offload
status: open
blocked_by: []
claimed_by:
---

## Question

In a `--gpu=drm` guest, Chromium's VA-API decode path is instantiated but never
does any work, while `ffmpeg` and `mpv` decode hardware-accelerated in the same
guest. What does Chromium need that the other clients do not?

Evidence already collected (2026-10-02, 12 flag combinations and 4 guest runs):

- the chromium in the image *has* VA-API (`vaInitialize`, `VaapiVideoDecoder`,
  `VaapiIgnoreDriverChecks`, `AcceleratedVideoDecodeLinuxGL/Linux` are all in the
  unwrapped ELF; the wrapper puts libva on `LD_LIBRARY_PATH`);
- every headless run logs exactly
  `media/gpu/vaapi/vaapi_wrapper.cc:1755 GetHandle(): Either
  VADisplayStateSingleton::PreSandboxInitialization() hasn't been called or that
  method failed` and falls back to software (`Media.VideoDecoderFallback.H264`),
  identically with both generations of feature names, with and without
  `--no-sandbox`, with the GPU sandbox enabled (guest run as the non-root user),
  with `--use-angle=vulkan|gl`, `--use-gl=egl`, `--in-process-gpu` and
  `--disable-features=Vulkan`;
- headless is a dead end for a different reason: without a display the element
  stalls (`readyState=0`); with a display (host weston + `cang --waypipe`) it
  plays at full rate and the fallback line disappears, but the rate is identical
  with the features on and off, so the decode is still software;
- after the `GBM_BACKENDS_PATH` fix (guest-init) the features-on arm constructs
  `VaapiVideoDecoder` (the features-off arm builds and immediately destroys it),
  but the CPU cost is *not* lower (474 vs 347 ticks for the same 46 s of 1080p30)
  and no initialisation-success line appears.

So the open question is Chromium's own `VADisplayStateSingleton`/pre-sandbox
initialisation path: what it requires (a display? a specific GBM/EGL setup? the
render node opened before the GPU sandbox?) and why it is skipped or failing
here. `--vmodule=gpu_init=3,vaapi_wrapper=3` output is the instrument; mpv
already proves the guest's VA-API stack is functional, so this is scoped to
Chromium's bring-up.

## Acceptance

A `--gpu=drm` guest Chromium run that decodes the 1920x1080 clip with hardware
decode engaged - a positive signal from Chromium itself (decoder-initialisation
log line or an actual CPU reduction against the features-off control), not the
absence of the fallback line.

## Additional evidence (2026-10-02, displayed runs)

- **Chromium never initialises libva at all** in a displayed (`--ozone-platform=wayland`,
  host weston + `cang --waypipe`) run with `--enable-features=VaapiVideoDecoder,
  VaapiVideoDecodeLinuxGL,VaapiIgnoreDriverChecks`: zero `libva` messages in the log, zero
  `VideoDecoderFallback` lines, zero `PreSandboxInitialization` lines. The GPU process does
  hold 7 open `/dev/dri/*` descriptors, so it opens DRM nodes - but for GL/EGL, not VA-API.
- The flags that target the pre-sandbox device scan (`--hardware-video-device-path=`,
  `--render-node-override=`, `--enable-primary-node-access-for-vkms-testing`) change nothing
  observable: no libva messages, same fallback behaviour. CPU for the same 20 s of 1080p30
  did drop (base 426 ticks, override 243, devpath 295), which is suggestive but not a
  positive signal - it needs a decoder-level confirmation.
- Chromium 154's own feature definitions (`media/base/media_switches.cc`) show VA-API decode
  enabled by default on Linux (`kAcceleratedVideoDecodeLinux` = "AcceleratedVideoDecoder",
  default-on with `USE_VAAPI`; `kAcceleratedVideoDecodeLinuxGL` default-on), so the gate is
  not the feature flag - it is whichever GPU-feature/GPU-info decision precedes
  `VaapiVideoDecoder`.
- The pre-sandbox device scan in `media/gpu/vaapi/vaapi_wrapper.cc` (154.0.8037.57) skips
  **non-PCI** devices and, when `gpu_info` is supplied, requires the device's
  `vendor_id`/`device_id` to equal `gpu_info->active_gpu()`'s. In this guest the DRM device
  is virtio (vendor 0x1af4) while the GPU the browser sees through venus is the host AMD part
  (0x1002) - a mismatch that would leave `drm_fd_` invalid and produce exactly the
  `GetHandle()` message seen in the headless runs. That is the hypothesis to test next, and
  the `--hardware-video-device-path` switch is the way to bypass the scan.

Next instrument: `--vmodule=video_decoder_pipeline=3,vaapi_video_decoder=3,gpu_init=3` with
`--log-level=0`, grepping for the decoder-selection lines (which decoder Chromium builds for
the config), plus an in-guest `ffmpeg` control using the *image's* ffmpeg path rather than a
host store path (the host path silently produced no output in the last probe).

## Instrument: a libva interposer (and what it proved)

`valog2.so` is a tiny `LD_PRELOAD` shim that logs a constructor line per process (so a
silent log can be distinguished from a shim that never loaded) plus `vaInitialize`,
`vaCreateConfig` (profile and entrypoint), `vaCreateContext` and `vaCreateBuffer`. It is
built with the repo's devshell gcc and dropped in the workspace, so the guest can preload
it. In one `--gpu=drm --waypipe` guest:

```
mpv --hwdec=vaapi --vo=gpu --gpu-api=opengl   shim loaded in 2 processes, 5 VA-API calls
      vaCreateConfig profile=7 entrypoint=1 attrs=0 -> 0      (H264High / VLD)
chromium (VA-API features on)                  shim loaded in 12 processes, 0 VA-API calls
chromium (features off)                        shim loaded in 12 processes, 0 VA-API calls
```

So the shim reaches every Chromium process and **Chromium never calls `vaInitialize`** - not
headless, not on Wayland, with `VaapiVideoDecoder,VaapiVideoDecodeLinuxGL,VaapiIgnoreDriverChecks`
*or* the 154 names `AcceleratedVideoDecoder,AcceleratedVideoDecodeLinuxGL`, with
`--ignore-gpu-blocklist`, `--use-gl=egl`, `--use-angle=gl|vulkan`, `--in-process-gpu`, or
with the sandbox on or off. The earlier "chromium maps libva/libgallium" observation was a
false lead: `libgallium` is mesa's *GL* driver too, so its presence in the GPU process says
nothing about VA-API.

## Correction: the host behaves identically, so this is not the guest

The same binary with the same flags on the **host** (real AMD render node) also produces zero
VA-API calls and the same `vaapi_wrapper.cc:1755 GetHandle(): Either
VADisplayStateSingleton::PreSandboxInitialization() hasn't been called or that method failed`
line. The guest is therefore not the differentiator: this chromium build/configuration does
not reach VA-API on real hardware either, while `mpv` and `ffmpeg` in the same guest do.

What that rules in and out:

- nixpkgs only forces `use_vaapi = false` on **aarch64**
  (`pkgs/applications/networking/browsers/chromium/common.nix:1128`), and Chromium's own GN
  default is `is_linux && (ozone_platform_x11 || ozone_platform_wayland) && x86/x64/arm64`
  (`media/gpu/args.gni`) - so on this host's x86_64 the build should have VA-API compiled in;
- the pre-sandbox device scan in `vaapi_wrapper.cc` (`drmGetDevices2` -> skip non-PCI, then
  require the device's vendor/device to match `gpu_info->active_gpu()`, then `LoadDrmFD`)
  runs *after* whatever decides to call it, and the message says it "hasn't been called or
  ... failed";
- so the gate is the call site of `VaapiWrapper::PreSandboxInitialization` in the GPU
  process's init, which is the thing to read next (it is not in
  `gpu/ipc/service/gpu_init.cc` in 154).

Next, cheapest first: run the host chromium with `--hardware-video-device-path=/dev/dri/renderD128`
and the interposer (if libva calls appear, the scan/gpu_info is the blocker even on the host);
then find the `VaapiWrapper::PreSandboxInitialization` caller and its condition. Only after
that is it worth asking whether cang should ship a VA-API-enabled Chromium in the image -
which would be an image-level decision, not a guest fix.

## Correction (2026-10-03): the interposer is dlopen-blind, and this host is a virtio VM

Two corrections to the section above, both material:

1. **The development environment is not real hardware.** It is itself a QEMU/KVM virtual
   machine (`systemd-detect-virt` = `kvm`, DMI `Standard PC (Q35 + ICH9, 2009)` / `QEMU`)
   whose only DRM device is **virtio-pci** (`/sys/class/drm/card1/device/vendor = 0x1af4`,
   driver `virtio-pci`); the "AMD Radeon RX 7600M XT" name that appears in the compositor
   and WebGL strings arrives through venus passthrough. So "the host behaves identically"
   means "two virtio environments behave identically", not "real AMD hardware behaves the
   same". The vendor-mismatch hypothesis below is therefore still very much alive, and it
   applies to *both* environments equally.
2. **`LD_PRELOAD` cannot see Chromium's libva calls.** Chromium dlopens libva: the binary
   references `libva.so`/`libva.so.` by name, has no `libva` entry in `DT_NEEDED`, and
   resolves `vaInitialize`, `vaCreateConfig`, `vaGetDisplayDRM` and `vaCreateSurfaces` with
   `dlsym`. A preloaded definition of those symbols is bypassed by a `dlsym` on a dlopen'd
   handle, so the "0 VA-API calls" figure for Chromium is **not evidence** - only the mpv /
   ffmpeg numbers are, because those link libva normally.

What still stands after the correction:

- with the default configuration, chromium's GPU process logs
  `vaapi_wrapper.cc:1755 GetHandle(): Either VADisplayStateSingleton::PreSandboxInitialization()
  hasn't been called or that method failed`, and **`--hardware-video-device-path=` and
  `--render-node-override=` both silence it** - the scan in
  `VADisplayStateSingleton::PreSandboxInitialization` (skip non-PCI, then require the DRM
  device's vendor/device to equal `gpu_info->active_gpu()`) is what fails by default, in this
  VM and in the cang guest alike, and both switches set `drm_fd_` directly;
- libva's *own* logging is dlopen-proof, and with `LIBVA_MESSAGING_LEVEL=1` a chromium run
  (device-path switch included) emits **no `libva` messages at all** - so satisfying the scan
  is not by itself enough for chromium to initialise VA-API here.

Next instrument, then: interpose `dlopen`/`dlsym` (to see whether chromium opens libva and
which symbols it asks for), and compare against a chromium run on hardware whose DRM node is
the same device the browser reports as active - which this development VM cannot provide, so
that comparison needs a genuinely bare-metal AMD host.

## The gate is the GL backend - and the oracles are weaker than they look

`--use-angle=vulkan` (venus) is what flips Chromium's VA-API path on:

| arm (guest, `--gpu=drm --waypipe`) | `GetHandle()`/PreSandbox error | mediaCapabilities `powerEfficient` (H264/HEVC) | `prefer-hardware` |
| --- | --- | --- | --- |
| default GL | yes | false | false |
| `--use-angle=gl` | - | false | false |
| `--use-gl=egl` | - | false | false |
| `--use-angle=swiftshader` | - | false | false |
| **`--use-angle=vulkan`** | **no** | **true (H264 High, HEVC)** | true/false |
| `--use-angle=vulkan --disable-gpu` | - | false | false |

The same A/B on the development VM (also virtio) behaves identically, so the guest is not the
variable - Chromium's VA-API availability is decided by its GPU/GL information, and only the
Vulkan/ANGLE backend (venus) makes it consider hardware decode available. Playback through
that arm works in a waypipe guest (`readyState=4`, 1920x1080, ~434 frames in 20 s, ~3%
dropped) even though the same configuration makes the GPU process crash-loop (9 restarts,
ticket 07's venus wall) - the video element survives it.

**But neither oracle proves the decode is really VA-API:**

- `powerEfficient=true` survives `LIBVA_DRIVERS_PATH=/nonexistent`, so it is not a probe of the
  VA driver at all;
- `prefer-hardware` disagrees with it between runs (it is an advisory hint);
- the `LD_PRELOAD` interposer is invisible to Chromium (it dlopens libva), and the render
  server's own log is not reachable either: `VIRGL_LOG_LEVEL`/`VIRGL_LOG_FILE` do not reach the
  render server (its environment is curated), and the render server's `/dev/shm` is not the
  host's nor the guest's, so a log written there is invisible to both.

So the honest state of ticket 06 is: **Chromium in a `--gpu=drm` guest enables its VA-API path
only under `--use-angle=vulkan`, and whether that path actually decodes on the GPU is still
unproven.** The instrument that would settle it, and the next step here: extend the interposer
to `dlopen`/`dlsym` (log which library and which symbols Chromium asks for, then call through
so behaviour is unchanged), which works regardless of how libva is loaded.

## Resolved by instrument: Chromium does reach VA-API in the guest - and then stops

`vawrap.so` interposes `dlopen` and `dlsym` and hands Chromium *wrappers* for the VA-API entry
points (`vaInitialize`, `vaCreateConfig`, `vaCreateContext`, `vaCreateSurfaces`,
`vaBeginPicture`, `vaEndPicture`, `vaRenderPicture`), which is the only way to see a client
that dlopens libva instead of linking it. Run in a `--gpu=drm --waypipe` guest with plain
Chromium flags (`--ozone-platform=wayland`, `--alloc=hardened`, the VA-API feature names), on a
20 s 1920x1080 H.264 clip:

```
DLOPEN libva.so.2 -> ok
DLOPEN libva-drm.so.2 -> ok
DLOPEN /usr/lib/cang-va-runtime/dri/virtio_gpu_drv_video.so -> ok     <- the image's patched VA driver
vaInitialize -> 0 (1.23)                                              x4 (vulkan arm) / x1 (default)
vaCreateConfig profile=6|7|13|19|21|32 entrypoint=1 (VLD)         -> 0    x2 each
vaCreateConfig profile=6|7|13           entrypoint=6 (EncSlice)   -> 0    x6 each
playback: readyState=4, 1920x1080, 427-564 frames in 20 s, ~2% dropped
```

So **Chromium in the guest initialises VA-API on the guest's own patched driver and probes
configurations** - the earlier "never calls `vaInitialize`" reading was an artefact of the
`LD_PRELOAD` instrument, and the guest is not blocking anything. What it does **not** do is
decode: `vaCreateContext`, `vaCreateSurfaces`, `vaBeginPicture`, `vaRenderPicture` are all
**zero**, so the picture playing in these runs is decoded in software. The `vaCreateConfig`
probe storm is exactly what makes `mediaCapabilities` flip `powerEfficient` around, which is
why that oracle was unreliable.

The remaining question is therefore precise and chromium-internal: after probing profiles and
entrypoints, why does Chromium not create a decode context and submit pictures for a video it
says it supports? (Its `VaapiVideoDecoder` object is constructed and then destroyed - visible as
`VaapiVideoDecoder():` / `~VaapiVideoDecoder():` in the log - so the rejection happens between
config probing and surface creation.) Next instrument: Chromium's decoder-selection logging
(`--vmodule=video_decoder_pipeline=3,media=3,vaapi_video_decoder=3 --log-level=0`) plus the
`vawrap` witness extended to `vaExportSurfaceHandle`/`vaDeriveImage`, which will show which step
of `VaapiVideoDecoder::Initialize` returns early.

**Ticket 06 status: the guest is cleared; the gap is Chromium's own post-probe decoder setup.**

## The precise failure: `DecoderStatus::205` = `kFailedToCreateDecoder`

Chromium names its own failure once the logging is raised (`--v=1 --log-level=0`):

```
ERROR:media/mojo/services/mojo_video_decoder_service.cc:290] DecoderStatus::205
```

`media/base/decoder_status.h` defines 205 as **`kFailedToCreateDecoder`** (in the "reasons for
failing to initialize" block: 200 `kUnsupportedProfile`, 201 `kUnsupportedCodec`, 202
`kUnsupportedConfig`, 204 `kCantChangeCodec`, **205 `kFailedToCreateDecoder`**, 206
`kTooManyDecoders`). So the GPU-process video-decoder factory refuses to build a decoder (or the
decoder's own initialize fails) even though VA-API is initialised and the profiles/entrypoints
probe clean.

Arm matrix in the guest (all `--ozone-platform=wayland`, all with the VA-API feature names):

| arm | `DecoderStatus` | `vaCreateContext` | `vaBeginPicture` | frames/20 s |
| --- | --- | --- | --- | --- |
| `--use-angle=vulkan` | 205 | 0 | 0 | 438 |
| `--use-angle=vulkan --disable-gpu-driver-bug-workarounds` | 205 | 0 | 0 | 438 |
| `--use-angle=vulkan` + `UseMojoVideoDecoder` | 205 | 0 | 0 | 429 |
| `--use-angle=vulkan --disable-features=UseChromeOSDirectVideoDecoder` | (none) | 0 | 0 | 436 |
| plain GL | (none) | 0 | 0 | 569 |

Either the decoder is refused with 205 or the software path is taken silently; in no arm does a
VA-API decode context appear. The failure is therefore after VA-API initialisation and config
probing and before surface/context creation - i.e. inside Chromium's decoder creation
(`GpuVideoDecodeAcceleratorFactory`/`VaapiVideoDecoder::Initialize`), with the VA-API stack
itself proven healthy in the same guest by `mpv` (341 buffers, `vaCreateConfig profile=7
entrypoint=1`).

Next step (kept for the next session): the `vawrap` witness extended to every VA-API entry point
in the decoder's initialisation order (`vaCreateSurfaces`, `vaExportSurfaceHandle`,
`vaDeriveImage`, `vaSyncSurface`), so the *last* VA call before the refusal is visible, plus
Chromium's `--vmodule=vaapi_video_decoder=3,gpu_video_decode_accelerator_factory=3` output at
`--log-level=0`.

## The full VA-API call inventory: enumeration only, and nothing is missing on the driver side

Extending the witness to the whole decode-init surface (`vaQueryConfigProfiles`,
`vaQueryConfigEntrypoints`, `vaGetConfigAttributes`, `vaCreateConfig`,
`vaQuerySurfaceAttributes`, `vaDestroyConfig`, `vaCreateSurfaces`, `vaCreateContext`,
`vaCreateBuffer`, `vaBeginPicture`, `vaEndPicture`, `vaRenderPicture`, `vaSyncSurface`,
`vaMapBuffer`, `vaDeriveImage`, `vaExportSurfaceHandle`, `vaTerminate`) and logging every call
**in order** for a 20 s 1080p playback in a `--gpu=drm --waypipe` guest gives:

```
152 vaQuerySurfaceAttributes   152 vaCreateConfig   152 vaDestroyConfig
148 vaQueryConfigEntrypoints   112 vaGetConfigAttributes    4 vaInitialize
 64 dlsym  15 LOADED  12 DLOPEN   4 vaQueryVendorString  4 vaTerminate
  0 vaCreateSurfaces   0 vaCreateContext   0 vaCreateBuffer
  0 vaBeginPicture     0 vaEndPicture      0 vaRenderPicture   0 vaSyncSurface

tail: ... vaQueryConfigEntrypoints profile=32 -> 0 n=1
      vaQueryConfigEntrypoints profile=-1 -> 0 n=1
      vaGetConfigAttributes profile=-1 entry=10 -> 0
      vaCreateConfig profile=-1 entry=10 attrs=1 -> 0
      vaQuerySurfaceAttributes -> 0 n=33 / n=23
      vaCreateConfig profile=-1 entry=10 attrs=0 -> 0
      vaDestroyConfig -> 0 (x2)   vaTerminate -> 0
```

So Chromium's VA-API use is **pure capability enumeration** (every profile x entrypoint, plus
the surface-attribute table) and then it destroys the configs and terminates the display - it
never creates a surface, a context, a buffer or a picture. The refusal happens between "the
config probes succeed" and "create surfaces", which is Chromium's own decoder-creation logic.

**The driver side is not the gap.** The surface-attribute table the guest's driver returns
(`vaQuerySurfaceAttributes -> n=24`, identical for `mpv`'s decode config) advertises
`PixelFormat` NV12 (`0x3231564e`), NV21, I420, P010/P012/P016, Y800, YUY2, 422V, 444P, RGBP,
RGBAP, RGBX/ARGB/XRGB/BGRX/ABGR variants and a `MemoryType` of `0x68000001` - so both the
format Chromium wants for H.264 decode and a DRM-PRIME memory type are offered. Nothing in the
VA-API capability surface explains `kFailedToCreateDecoder`.

That closes the investigation this ticket can do from inside cang: the guest, the driver, the
VA-API library and the vrend path are all healthy (mpv decodes hardware-accelerated in the same
guest), and the remaining defect is inside Chromium's decoder creation, which needs either
upstream knowledge of that code path or a Chromium built with decoder logging that this official
build does not emit at any `--vmodule` tried.

## Upstream read (2026-10-09): `kFailedToCreateDecoder` is a decoder-*selection* code, and neither documented flag set engages the guest's decode

Sources are the pinned Chromium **154.0.8037.57** tree on
`chromium.googlesource.com/chromium/src/+/refs/tags/154.0.8037.57/` (line numbers
are that tag's) and Chromium's own `docs/gpu/vaapi.md`.

### (a) `VaapiVideoDecoder::Initialize` never returns `kFailedToCreateDecoder`

- `media/base/decoder_status.h:50` defines `kFailedToCreateDecoder = 205`.
- `VaapiVideoDecoder::Initialize` returns `kFailed` on every failure path
  (`media/gpu/vaapi/vaapi_video_decoder.cc:190,304,468,798`), never 205. So 205 is
  produced one level up, by the **decoder pipeline / mojo factory**, not by the
  VA-API decoder:
  - `MojoVideoDecoderService::Initialize`: `if (!decoder_) OnDecoderInitialized(
    kFailedToCreateDecoder)` — `media/mojo/services/mojo_video_decoder_service.cc:289`;
    `decoder_` is `mojo_media_client_->CreateVideoDecoder(...)` (`:223`).
  - `VideoDecoderPipeline::InitializeTask`: `if (!decoder_) { OnError("|decoder_|
    creation failed."); ... kFailedToCreateDecoder }` —
    `media/gpu/chromeos/video_decoder_pipeline.cc:688`.
- The Linux factory `GpuMojoMediaClient::CreateVideoDecoder`
  (`media/mojo/services/gpu_mojo_media_client.cc`) returns `nullptr` when
  `IsAcceleratedDecodingDisabled()` (i.e. `--disable-accelerated-video-decode`, or
  `GPU_FEATURE_TYPE_ACCELERATED_VIDEO_DECODE != kGpuFeatureStatusEnabled`, or GL is
  disabled) or when there is no command buffer id; otherwise
  `GpuMojoMediaClientLinux::CreatePlatformVideoDecoder`
  (`media/mojo/services/gpu_mojo_media_client_linux.cc`) returns `nullptr` when
  `GetActualPlatformDecoderImplementation()` is `kUnknown` (its `default: return
  nullptr`). That function is the real selection gate:
  - `kAcceleratedVideoDecodeLinux` must be enabled (`media/base/media_switches.cc:1486`,
    default on with `USE_VAAPI`), then
  - **GL** (`gr_context_type == kGL`) needs `kAcceleratedVideoDecodeLinuxGL`
    (`:1494`, default on) — vendor-independent; **Vulkan** needs `gr_context_type ==
    kVulkan` and `features::kVulkanFromANGLE` **and** `features::kDefaultANGLEVulkan`,
    both `FEATURE_DISABLED_BY_DEFAULT` (`ui/gl/gl_switches.cc:299-310`), plus a
    non-empty `gpu_info.vulkan_info`, plus Intel or `kVaapiIgnoreDriverChecks`.
- So the profile/entrypoint storm is the *capability* query
  (`GetSupportedVideoDecoderConfigs` → `VideoDecoderPipeline::GetSupportedConfigs` →
  `VaapiVideoDecoder::GetSupportedConfigs` → `VaapiWrapper::GetSupportedDecodeProfiles`),
  which is a different call from `CreateVideoDecoder`. 205 is the latter declining to
  build a decoder, or the pipeline's own decoder creation failing.

### (b) `GetHandle()/PreSandboxInitialization` is a real gate, and it is bypassable

- `VADisplayStateSingleton::GetHandle` returns nothing when `drm_fd_` is invalid —
  "PreSandboxInitialization() hasn't been called or that method failed to find a
  suitable render node" (`media/gpu/vaapi/vaapi_wrapper.cc:1754`).
- `PreSandboxInitialization` (`:1623-1732`) skips non-PCI devices, needs a render node,
  and when `gpu_info` is supplied requires the device's PCI vendor/device to equal
  `gpu_info->active_gpu()` (`:1683-1690`). In this guest the only DRM device is virtio
  (`0x1af4`) while venus surfaces the host AMD part (`0x1002`), so the scan leaves
  `drm_fd_` invalid. Three switches set `drm_fd_` directly:
  `--hardware-video-device-path` and `--render-node-override` (`:1634-1656`), and the
  primary-node fallback `--enable-primary-node-access-for-vkms-testing` (`:1708-1728`;
  upstream extended that switch to cover `USE_VAAPI`).
- So yes, the complaint is the gate for `VaapiWrapper::Create`
  (`media/gpu/vaapi/vaapi_wrapper.cc:1915-1918`), and `--render-node-override` clears it
  (measured below). It is **not** the whole story: clearing it does not by itself get a
  decode context.

### (c) Verified: Chromium's own documented VA-API flags still produce zero decode contexts

Chromium documents an unsupported "VaAPI on Linux" OpenGL and Vulkan flag set in
`docs/gpu/vaapi.md` ("VaAPI on Linux with OpenGL" / "with Vulkan"). Both, plus the
`--render-node-override` from (b), were run in one bounded `--gpu=drm --waypipe` guest
(cang `cang-baseline`, guest-init `gi-gbm`, image `1393b4cc`, the guest's own patched
`virtio_gpu_drv_video.so`), playing the 20 s 1920x1080 H.264 clip under the
`vawrap4.so` dlopen/dlsym witness (evidence:
`/home/dev/cang/disk/nctx/t06{,b,c}/`):

| arm (`--ozone-platform=wayland`, all `--alloc=hardened --ignore-gpu-blocklist`) | libva init | `vaCreateConfig` | surfaces / contexts / begin / render | GetHandle complaint |
| --- | --- | --- | --- | --- |
| ticket's flags: `--use-angle=vulkan` + `VaapiVideoDecoder,VaapiVideoDecodeLinuxGL,VaapiIgnoreDriverChecks` | 0 | 0 | 0 / 0 / 0 / 0 | 1 |
| + `--use-gl=angle --use-angle=gl` + doc GL features + `--render-node-override` | 1 | 38 | 0 / 0 / 0 / 0 | 0 |
| + `--use-gl=angle --use-angle=vulkan` + doc Vulkan features (`Vulkan,DefaultANGLEVulkan,VulkanFromANGLE`) + override | 1 | 38 | 0 / 0 / 0 / 0 | 0 |
| default GL + doc GL features + `--render-node-override` | 1 | 38 | 0 / 0 / 0 / 0 | 0 |
| `--use-angle=vulkan` + doc Vulkan features (no `--use-gl=angle`) + override | 1 | 38 | 0 / 0 / 0 / 0 | 0 |
| as above, `--v=2 --log-level=0` and `video_decoder_pipeline/decoder_selector` vmodule | 1 | 38 | 0 / 0 / 0 / 0 | 0 |

Every arm played software (`readyState=4`, ~570-596 frames of 1920x1080 in 20 s). The
witness log is identical in all five "improved" arms: `vaInitialize` once, the whole
`VASupportedProfiles` enumeration (`profile=7 entry=1` created and destroyed as a
probe, `MemoryType 0x68000001`, NV12 `0x3231564e`, Max 16384x16384), then
`vaCreateConfig profile=-1 entry=10` and `vaTerminate` — i.e. `GetHandle()` was taken
and released **once**, and no decoder wrapper, surface or context was ever created.
`--render-node-override=/dev/dri/renderD128` alone (arm 2/4/5) removes the
`PreSandboxInitialization` failure, so (b) is a genuine, bypassable gate — it is just
not the binding one.

**Therefore cang has no Chromium flag to set here.** The remainder is inside Chromium's
decoder creation *after* the capability query, and the official build emits no
`VideoDecoderPipeline`/`DecoderStatus`/`decoder_selector` line to stderr even at
`--v=2 --log-level=0`, so 205 vs 202 vs "no decoder built at all" cannot be told apart
from the console.

### Smallest next experiment

Read the decoder the renderer actually selected from the media pipeline's own log
surface instead of stderr: run the arm and open `chrome://media-internals` (or capture
the `MediaLog` via `--vmodule=media_log=3`), and look at the player's `video_decoder`
property and `error` field. That names `GpuVideoDecoder`/`FFmpegVideoDecoder` (so the
selection result is known) and prints the `DecoderStatus` (205 vs 202), which decides
between (i) `CreateVideoDecoder` returning nullptr and (ii) the pipeline being built and
rejecting the config. Only after that does it make sense to ask whether the fix is an
upstream Chromium change (Linux VA-API is documented as unsupported) or a cang-side
image/GPU-property decision.
