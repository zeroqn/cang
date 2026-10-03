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
