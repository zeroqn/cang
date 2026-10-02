---
label: wayfinder:map
title: Guest VA-API video through vrend (decode + encode)
---

## Destination

A `cang --gpu=drm` guest does hardware video the way the host does: `vainfo`
reports the host's profiles, a decoder client (`ffmpeg -hwaccel vaapi`, mpv
`--hwdec`) consumes them into VAAPI surfaces, and a VA-API encode produces a
stream any decoder accepts - all of it through the vrend video path, with the
wiring owned by cang, libkrun's fork and the rutabaga_gfx fork, and no user
environment setup.

**Status (2026-10-01): decode works, encode fixed and verified.** Decode
was enabled by carrying cang's `VIRGL_RENDERER_USE_VIDEO` through the rutabaga_gfx
fork and libkrun's fork plus a guest-init `LIBVA_DRIVERS_PATH` (ticket 01).
Encode was root-caused and fixed by ticket 02: the guest's coded-buffer read-back
is faithful, and the stream the host hands over was already broken because cang's
vrend submitted no packed parameter sets and a zeroed H.264 sequence parameter
buffer. cang now carries one patch per side of that wire extension (host:
`.#virglrenderer`; guest: the image-local `cang-va-runtime` VA driver) and a
`--gpu=drm` guest's H.264 and HEVC encodes software-decode cleanly. The encoder
entrypoint is still advertised while every encoder-attribute query in the guest
is 0 (charted as ticket 04), and ticket 03's black opening frames are fixed too:
the host's upload blits and the VA encode share memory but nothing ordered them,
so a `glFinish()` in the upload path now ships with `.#virglrenderer`.

**Ticket 05's chroma defect is fixed too (2026-10-02).** The guest's H.264/HEVC encode
now matches the host control's PSNR to six decimals at 176x144 through 1920x1080, because
vrend's video surface is allocated with a linear DRM modifier instead of the driver's
default tiling (the plane import is linear, so a tiled surface made GL write the picture
where the encoder did not look - chroma ruined, luma untouched). See
`tickets/05-chroma-planes-wrong.md` and the reproducer in
`notes/repro-egl-import.c`.

## Notes

- Domain: the `--gpu=drm` vrend video path - cang's virgl flags word
  (`crates/cang/src/runtime/vm/libkrun/launcher.rs`) to libkrun's `krun-devices`
  to virglrenderer 1.3.0's `vrend_video`/`virgl_video`, plus the guest's mesa
  `virtio_gpu_drv_video.so` and the host's libva on the render node.
- Skills worth consulting: `diagnosing-bugs`, `research`, `domain-modeling`.
- Evidence base: `docs/vaapi-video-investigation.md` (Resolution and the encode
  finding). The live-run logs and the hermetic podman store the probes used live
  outside the repo, on the host btrfs disk.
- Standing preferences: every claim comes from a live `--gpu=drm` guest running
  the packaged `nix build .#cang`; each probe carries a contrast that can fail (a
  host-side identical command, or a software decode/encode control); a decode
  probe proves the venus path too, because a render server that fails to start
  drops the whole backend to 2D and takes virgl, venus and video with it.
- Versions at the decode landing: virglrenderer 1.3.0 (`-Dvideo=true`), the
  image's mesa 26.1.8, libva 1.23.0, libkrun fork `2855f4d1`, rutabaga_gfx fork
  `d8479a1`, guest kernel 7.2.7-hardened1.

## Decisions so far

<!-- one line per closed ticket, gist plus link -->

- [Enable the vrend VA-API video path (decode)](tickets/01-enable-vrend-video-decode.md): the request died in rutabaga, which never carried `VIRGL_RENDERER_USE_VIDEO`, so `virgl_renderer_init` never enabled vrend's video path; the fork carries the bit, libkrun forwards it with `set_use_video`, guest-init exports `LIBVA_DRIVERS_PATH`, and the guest then reports the host's profiles and decodes into VAAPI surfaces.
- [The first frames of a guest encode are black](tickets/03-first-frames-are-black.md): the host's upload blits the picture into the buffer the VA encoder reads, and only the two sharing memory ties them together - `vrend_video.c:283` -> `:210` then `virgl_video_begin_frame` -> `vaBeginPicture` with no wait, so the opening frames encoded a buffer whose copy had not run (100% black). A `glFinish()` at the end of the upload fixes it; verified by the same probe before and after.
- [Where the guest VA-API encode loses its bitstream](tickets/02-encode-coded-buffer-readback.md): not in the read-back - an interposed `vaMapBuffer` in the VM worker returns exactly the bytes the guest's `ffmpeg` writes. The host's own VA encode was the break: virglrenderer 1.3.0 submitted no packed parameter sets (`src/vrend/virgl_video.c:1440`) and left the H.264 sequence parameter buffer's geometry/level/flags zero (`src/vrend/virgl_video.c:1243`), over wire structs with no fields for them (`src/virgl_video_hw.h:150-171`). Fixed by the two patches described in the ticket's Resolution; verified by software-decoding a guest H.264 and HEVC encode.

## Not yet specified

- Client-level hardware video in the guest (mpv `--hwdec=auto` picking VA-API,
  Chromium `<video>` decode) - not measured; may deserve its own map.
- Whether to offer the ticket 02 wire extension upstream to virglrenderer and
  mesa now that both halves are proven in-tree.

## Client-level video: first measurement (2026-10-02)

`tools/chromium-cang-smoke`'s launch shape is required for chromium to run at all in the
guest (`--alloc hardened`, or it segfaults on startup - and this is *not* affected by the
ticket 05 fix: the WebGL control page passes with the patched build). With that, chromium
154 plays a real 1920x1080 H.264 file from `~/cang/*.mp4` (a 20 s clip) in a `--gpu=drm`
guest: `readyState=4`, `videoWidth=1920`, frames decoded.

**Hardware decode does not engage**, though, and the reason is chromium-internal:

```
media/gpu/vaapi/vaapi_wrapper.cc:1755 GetHandle(): Either VADisplayStateSingleton::
PreSandboxInitialization() hasn't been called or that method failed
Histogram: Media.VideoDecoderFallback.H264 recorded 1 samples
```

- identical with `--enable-features=VaapiVideoDecoder,VaapiVideoDecodeLinuxGL,VaapiIgnoreDriverChecks`
  (the fallback happens before any of that matters) and with the features disabled, so the
  run is a software-decode playback today;
- the same guest decodes VA-API fine through ffmpeg (ticket 01's evidence, re-checked in
  ticket 05), so the gap is chromium's own VA-API bring-up, not the guest's driver;
- `--use-angle=vulkan` makes the video element *stall* outright (`readyState=0`), which is
  the same ANGLE/vulkan wall the chromium smoke documents for drawing.

### The flag matrix (12 runs) and what it says

The chromium in the image *has* VA-API (the real ELF,
`…-ungoogled-chromium-unwrapped-154.0.8037.57/libexec/chromium/chromium`, contains
`vaInitialize`, `VaapiVideoDecoder`, `VaapiIgnoreDriverChecks`,
`AcceleratedVideoDecodeLinuxGL` and `AcceleratedVideoDecodeLinux`, and the wrapper puts
libva on `LD_LIBRARY_PATH`), so the feature names are known. Every combination tried still
produced exactly one `Media.VideoDecoderFallback.H264` and no VA-API initialisation:

| variant (all `--headless=new --alloc=hardened --ignore-gpu-blocklist`) | result |
| --- | --- |
| no feature flags (baseline) | fallback, `GetHandle()` failure |
| `VaapiVideoDecoder,VaapiVideoDecodeLinuxGL,VaapiIgnoreDriverChecks` | identical |
| `AcceleratedVideoDecodeLinuxGL,VaapiIgnoreDriverChecks` | identical |
| `AcceleratedVideoDecodeLinux,VaapiIgnoreDriverChecks` | identical |
| all of the above names at once | identical |
| `--use-angle=gl` / `--use-gl=egl` | identical (`--use-gl=egl` also breaks GL init) |
| `--in-process-gpu` | identical |
| `--disable-gpu-sandbox` removed, guest run as the non-root user | identical (sandbox does initialise) |
| `--disable-features=Vulkan` | identical |

Every failure is the same line:
`media/gpu/vaapi/vaapi_wrapper.cc:1755 GetHandle(): Either
VADisplayStateSingleton::PreSandboxInitialization() hasn't been called or that method
failed`.

**Reading of it:** the missing step is chromium's *pre-sandbox* VA-API initialisation, which
its GPU-init path performs to open the VA device before the sandbox is applied - and in
headless mode that path (and the GL display a VA-API frame import needs) is not set up. The
guest is not the problem: the same guest decodes VA-API through ffmpeg, and the WebGL control
page passes with the patched cang build (so the ticket 05 fix does not disturb chromium).

**Next test, with a reason:** give chromium a display - the harness already has the pieces
(`tools/chromium-cang-smoke`'s `--weston`/`--waypipe` modes, `--ozone-platform=wayland`, a
weston headless backend) - and keep the VA-API features on, then check for the absence of
`Media.VideoDecoderFallback.H264`. If that engages, the combination to document is "displayed
chromium + `VaapiVideoDecoder,VaapiVideoDecodeLinuxGL,VaapiIgnoreDriverChecks`", not a
headless one.

### mpv shipped through the workspace: three dead ends, and what they say

The idea was to avoid an image change by shipping mpv's closure through the shared
workspace (the trick that worked for the guest mesa build). Measured, in order:

- **the closure is nearly all present already**: `nix path-info -r` of
  `mpv-0.41.0` lists 261 paths and the guest has **244** of them at the *same* store
  paths (both sides use the same nixpkgs), so only **17** paths are missing - lua,
  mujs, libplacebo, libdovi, rubberband, uchardet, shaderc/glslang, libcaca, freefont,
  libxpresent, libxscrnsaver, vamp-sdk, xorgproto - and shipping those is cheap
  (~600 MB in the workspace);
- **the guest's `/nix/store` is read-only**, so the obvious fix - symlinking the shipped
  paths back into `/nix/store/<same-name>` - cannot work;
- **rewriting the rpath does not rescue it either**: patchelf'ing every shipped ELF (99
  files) with an rpath carrying both the workspace and the store path for all 261 closure
  entries still ends in `libplacebo.so.360: cannot open shared object file`, i.e. the
  loader is not honouring an rpath of ~1000 entries (the same run with a 33-entry rpath
  got as far as the next library, so the list itself is the problem, not the paths).

So hand-shipping a large closure into the guest is the wrong mechanism here, and the
honest conclusion is the one the chromium result already pointed at: **client-level hardware
video in this guest needs a display/compositor, which the image does not have.** mpv with
`--vo=null` would decode without one - but the interesting part of a client-level test is the
interop path (decoded surface -> GL/compositor), and that needs weston or equivalent to be
present.

### The displayed route works: host weston + host waypipe + `cang --waypipe`

No image change is needed after all. The chromium smoke's waypipe shape runs the
compositor **on the host** (`weston --backend=headless --renderer=gl --socket=cang-video`)
with a host `waypipe client` on a socket, and passes that socket to the guest as
`cang … --waypipe=<socket>`; guest-init then exports a Wayland display
(`WAYLAND_DISPLAY=cang-waypipe-0`, `XDG_RUNTIME_DIR=/run/user/1000`) and chromium uses it.
A first displayed probe with that shape changed the picture immediately:

```
--- wayland  rc=124 secs=61  fallbacks=0
    VIDEOPROBE t12000 readyState=4 wh=1920x1080 t=11.14 frames=339 dropped=15
    VIDEOPROBE t20000 readyState=4 wh=1920x1080 t=19.14 frames=578 dropped=23
--- egl      rc=124 secs=61  fallbacks=0
    VIDEOPROBE t20000 readyState=4 wh=1920x1080 t=19.73 frames=595 dropped=18
```

- **the fallback is gone**: `Media.VideoDecoderFallback.H264` does not appear in either
  displayed run, where every headless run produced exactly one;
- the video plays at ~30 fps with ~4% dropped frames at 1920x1080 (against a stalled
  element, `readyState=0`, in the headless runs);
- **but hardware decode is still not proven**: chromium's own
  `media/gpu/vaapi/vaapi_wrapper.cc:1755 GetHandle()` failure is still logged, and no
  `VaapiVideoDecoder`/`GpuVideoDecode` initialisation line appears in either mode, so the
  decode may well still be software with the fallback simply not logged on this path.

**Correction (measured, same round): the displayed run is still software decode.** A
features-on vs features-off contrast under that display gives the same playback rate in both
arms - 574 and 592 frames decoded in 40 s of the same 1920x1080 clip, 0 fallback lines each -
and chromium's `GetHandle()` failure is still logged with no decoder-initialisation line, so
the fallback *message* is simply not emitted on the displayed path; the decode did not change.

**And there is a structural reason for that, which is the real finding of this round:** the
waypipe shape the smoke uses runs with **`-n` (`--no-gpu`)**, i.e. the guest's buffers travel
as `wl_shm` and the presented surface has no GPU buffers at all (the smoke's own comment: with
dmabuf enabled "the presenting Chromium GPU process still aborts and never paints"). VA-API
decode needs to *import* its frames into a GL/GPU surface, so on a shm-only display path
hardware decode cannot engage no matter which feature flags are used.

So the shape of the remaining work is now clear and it is not a flag question:

- **client-level hardware video needs GPU buffers in the guest's display path** - either the
  waypipe **dmabuf** path (currently broken: the presenting chromium GPU process aborts,
  see `docs/graphics-audio.md`), or a **compositor inside the guest** (weston in the image,
  rendering through the guest's virgl/venus GL) with chromium/mpv talking to that local
  compositor;
- the compositor-in-the-guest route is the one that keeps `--gpu=drm`'s GPU in the loop, and
  it is the minimal image change that makes the interop half testable (`weston` + `mpv-unwrapped`
  in the agent tooling layer, ~600 MB, since the image already carries 244 of mpv's 261
  closure paths);
- until then, the guest's *decode* half is proven (ffmpeg VA-API, ticket 01) and the
  *presentation* half is proven only in software (this round).

### Why the dmabuf path is not used: the guest has no GBM backend

With the host `waypipe client` run **without** `-n` (dmabufs enabled - note the guest side
already omits `--no-gpu` whenever `--gpu=drm` is set, per `docs/graphics-audio.md`), the guest
still presents its window as a **`wl_shm` buffer**: the host waypipe log shows
`create_buffer(wl_buffer#...)` and no dmabuf buffers, while both sides do negotiate
(`Connected waypipe-server may use dmabufs: true`). The reason is in the guest's chromium log,
identically with `--use-angle=vulkan`, `--use-angle=gl` and the default GL:

```
MESA-LOADER: failed to open dri: /run/opengl-driver/lib/gbm/dri_gbm.so:
  cannot open shared object file (search paths /run/opengl-driver/lib/gbm, ...)
```

i.e. the guest's mesa searches a *host-shaped* GBM path (`/run/opengl-driver/lib/gbm`) that
does not exist in the guest, so no GBM backend loads, chromium cannot allocate a dmabuf-backed
buffer, and presentation falls back to shm. That is a cang guest-wiring gap of exactly the kind
the VA-API driver path already had (an env var pointing into the guest's own runtime), and it
is the concrete next step: check whether the image ships `dri_gbm.so` at all (the prebuilt
mesa runtime's `lib/gbm`, or `libgallium`'s), and either point `GBM_BACKENDS_PATH` at wherever
it lives or add it to the image's mesa runtime, then re-run this test and confirm dmabuf
buffers on the host side.

Note also that VA-API did **not** engage in any of these runs (the `GetHandle()` failure is
still logged, one line per run, in every mode): that failure is local to the guest's chromium
and display-independent, so fixing GBM may or may not be enough for it - the two are separate
questions, and the GBM one is now the concrete, checkable one.

### Fixed: `GBM_BACKENDS_PATH` (and the dmabuf path now engages)

`crates/cang-guest-init/src/guest_init/components/wayland.rs`'s `MESA_ENV` now exports
`GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm` (with a unit-test assertion, so
removing it fails the crate's tests). The image already ships `dri_gbm.so` at exactly that
path, so this is a guest-init-only change - no image rebuild.

Verified in a live `--gpu=drm --waypipe` guest (host weston + host `waypipe client` with
dmabufs enabled), same probe as before:

```
GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm
/usr/lib/cang-mesa-runtime/lib/gbm/dri_gbm.so            (149824 bytes, present)
MESA-LOADER gbm failures: 0        (was 3+ per run)
guest: VIDEOPROBE t20000 readyState=4 wh=1920x1080 t=18.46 frames=557 dropped=10
host waypipe client: dmabuf create_params/create_immed = 12, wl_shm_pool = 4   (was 0 dmabuf)
```

So the guest now allocates GBM buffers, chromium presents them, and **Waypipe carries them as
dma-bufs** instead of `wl_shm` - the dmabuf presentation path works, which is the precondition
for any client that imports decoded frames into a GPU surface.

One open question this leaves: with GBM fixed, chromium's `vaapi_wrapper.cc` `GetHandle()`
failure line **disappeared** (0 vaapi lines in the run, where every earlier run had exactly
one), but that is the absence of a failure, not proof of hardware decode.

That follow-up run (GBM fixed, dmabufs on, `--vmodule=vaapi_video_decoder=3,vaapi_wrapper=3`,
features on vs off) gives a *partial* positive signal and a clear negative one:

```
hw rc=124 cpu_ticks=474  frames=556 dropped=9   VaapiVideoDecoder():            (constructed)
sw rc=124 cpu_ticks=347  frames=574 dropped=8   VaapiVideoDecoder(): / ~VaapiVideoDecoder():  (built, then destroyed)
```

- the VA-API decoder is now **constructed** in the features-on arm (before this fix the run
  never got that far), and in the features-off arm it is built and immediately destroyed;
- but the **CPU cost is not lower** with the feature on (474 vs 347 ticks for the same 46 s of
  1080p30, i.e. slightly higher), so the decode is not actually being offloaded yet, and no
  `VaapiVideoDecoder::Initialize` success line appears.

So: presentation is now GPU-backed (dmabufs over Waypipe) and chromium's VA-API decoder is at
least instantiated; the remaining gap is the decoder's own initialisation. With GBM in place
the interop precondition is met, so this is the right point to re-test with a client that
states its choice outright (`mpv --hwdec=vaapi`, which needs the image-layer decision) or to
instrument chromium's VA-API initialisation path directly.

### mpv is now in the image - and its default video output hits the venus wall

`mpv-unwrapped` joined `agentImagePackages` (`nix/image/layers.nix`), the container built, the
archive was loaded into the hermetic store, and the guest now resolves
`/nix/store/...-cang-agent-layer/bin/mpv` (v0.41.0). Two results from the first runs:

- **mpv's default `vo=gpu` aborts in the guest** (`rc=134`), in both the `--hwdec=vaapi` and
  `--hwdec=no` arms, with the render server reporting
  `vkr: failed to query resource props: invalid res_id 15` /
  `vkGetMemoryResourcePropertiesMESA resulted in CS error` /
  `ring_submit_cmd: vn_dispatch_command failed` - i.e. mpv's Vulkan path lands on the same
  venus dma-buf/format-modifier wall that the chromium work already recorded, before any
  decode happens. A GL video output (`--vo=gpu --gpu-api=opengl`) is the way around it.
- the decode-only arms (`--vo=null`) exit 0 but produced **empty logs** and no
  `Using hardware decoding` line, so they were inconclusive: mpv's messages did not reach the
  redirected stdout in that probe. Reading them through mpv's own `--log-file` fixed that.

### Client-level hardware video works in the guest

With the GBM fix in place and mpv reading its own log file, a `--gpu=drm --waypipe` guest
decodes 1920x1080 H.264 on the GPU and **presents VA-API surfaces**:

```
copy-null  --hwdec=vaapi-copy --vo=null           rc=0 hwdec=1  Using hardware decoding (vaapi-copy).
sw-null    --hwdec=no          --vo=null           rc=0 hwdec=0
gl-hw      --hwdec=vaapi       --vo=gpu --gpu-api=opengl
                                                   rc=0 hwdec=1  Using hardware decoding (vaapi).
                                                   VO: 1920x1080 vaapi[yuv420p]
```

The third line is the one that matters: mpv's video output reports the frames arriving as
**`vaapi[...]`** surfaces, i.e. the full client path - decode on the guest's VA-API device
through vrend's render server, surface import, and presentation over the Waypipe display - is
GPU-side. (CPU ticks are not a useful metric for the `-copy` variants, which copy back to
system memory by design; mpv's own statement plus the `VO:` format is the signal.)

Requirements, both now in place: the guest needs `GBM_BACKENDS_PATH` (the earlier sections),
and **mpv's default Vulkan video output must be avoided** - `vo=gpu` with the default
`--gpu-api=auto` picks Vulkan and aborts on the venus wall
(`vkr: failed to query resource props: invalid res_id 15`), while `--gpu-api=opengl` (virgl)
presents normally. That is the combination to document for media clients in a cang guest:

```
mpv --hwdec=vaapi --vo=gpu --gpu-api=opengl <file>
```

This closes the map's destination question for mpv. Chromium remains the outlier: it
instantiated its VA-API decoder after the GBM fix but still does not offload (its
`GetHandle()`/pre-sandbox initialisation path, a chromium-internal question).

Practical notes for that pass: the runner is `/home/dev/cang/disk/nctx/run-wg.sh`-shaped
(host weston + host waypipe client + `cang --gpu=drm --alloc hardened --mem 4 --seccomp=off
--landlock=off --waypipe=<socket> --guest-init …`), weston must use `--debug` for
screenshots, and the guest's chromium needs `--alloc=hardened` or it segfaults at startup.

## Open tickets

- ~~[The guest encode's chroma planes are wrong](tickets/05-chroma-planes-wrong.md)~~ -
  **done (2026-10-02)**: vrend's video surface is now allocated with a linear DRM
  modifier (`nix/pkgs/patches/virglrenderer-linear-surface.patch`), and the guest's
  encode matches the host's PSNR to six decimals at every size. Original report:
  the guest's H.264 stream decodes cleanly and its luma PSNR matches the host's to
  six decimals, but chroma PSNR is ~28 dB worse (15.6 vs 43.6 dB) and the encode
  spends ~2.4x the bits of the same command on the host at the same QP (measured
  at QP 20/26/32). A quality defect first, an efficiency one second, and the
  reason ticket 02's "decodes with exit 0" acceptance was too weak.
- [The guest's encoder attribute queries are all zero (and cost B-frames)](tickets/04-encode-attribute-queries-are-zero.md):
  `virgl_get_video_param` implements decode caps only, and the caps wire
  structure has no encoder-attribute fields, so the host's answers cannot reach
  the guest. Beyond the missing log line it costs B-frames: ffmpeg's VAAPI GOP
  comes from `VAConfigAttribEncMaxRefFrames`, mesa's frontend falls back to
  "past references only" when the cap is 0, and the guest's stream is therefore
  `type:I`/`type:P` where the host control emits `type:B`.

  A first attempt at forwarding the attribute is measured and reverted: the guest
  then agrees (`intra, P- and B-frames (1 / 1)`) but the stream grows 1.5x
  (testsrc) because a B-frame's reference lists do not cross the wire -
  `vrend/virgl_video.c` fills `VAEncSliceParameterBufferH264` from the wire
  desc and leaves `RefPicList0`/`RefPicList1` commented out, inventing the
  picture's `ReferenceFrames` from its own `frame_num % 32` ring of surfaces.
  B-frames need a reference-list extension (per-picture DPB with picture-order
  counts, per-slice lists plus active counts) before the attribute can be
  advertised. The prize, measured on the host for the same real content:
  1 671 174 B at `-bf 0` vs 1 123 666 B with B-frames, i.e. 33%.
- [Chromium's VA-API decoder instantiates but does not offload](tickets/06-chromium-vaapi-init.md):
  with the guest's VA-API stack proven working (`ffmpeg`, `mpv`), Chromium is the
  remaining client that does not use it - twelve flag combinations and four guest
  runs all fall back, and after the GBM fix the decoder object is constructed but
  the CPU cost is unchanged and no initialisation-success line appears. Scoped to
  Chromium's own `VADisplayStateSingleton`/pre-sandbox path.
- [Venus dma-buf format-modifier imports block Vulkan-presenting clients](tickets/07-venus-dmabuf-format-modifiers.md):
  `mpv`'s default Vulkan video output aborts (`vkr: failed to query resource
  props: invalid res_id 15`, `vkGetMemoryResourcePropertiesMESA resulted in CS
  error`) where `--gpu-api=opengl` presents fine - the same modifier wall the
  chromium smoke records for `--use-angle=vulkan`. The transport half is fixed
  (GBM buffers now travel as dma-bufs), so what remains is venus's import/query.

## Client-level video works with mpv (2026-10-02)

The map's destination is now met for a media client. With `GBM_BACKENDS_PATH` in
guest-init's `MESA_ENV` (Waypipe carries dma-bufs instead of `wl_shm`) and with
`mpv-unwrapped` added to the image's agent layer, a `--gpu=drm --waypipe` guest runs

```
mpv --hwdec=vaapi --vo=gpu --gpu-api=opengl <file>
```

and reports `Using hardware decoding (vaapi)` with a video output of
`1920x1080 vaapi[yuv420p]` - decode through vrend's render server, surface import
and presentation all GPU-side. The GL video output is required: mpv's default
Vulkan output hits the venus wall of ticket 07. `--hwdec=no` reports no hardware
decode, so the A/B attribution is real.

The change is also verified against the repository's own acceptance harness: the
`tools/chromium-cang-smoke` waypipe mode run against the patched cang and guest-init
reports `VERDICT: PASS` on every check (version, chromium-rc, webgl-vulkan, webgl-png,
waypipe-transport, venus-presenting, frame-presented, renderer-on-frame,
control-no-frame) and logs `gbm_backends_path=/usr/lib/cang-mesa-runtime/lib/gbm` in its
presenting-mode line, so the guest now presents with GBM available and nothing regressed.

## Out of scope

- DRM native context (guest-native RADV/radeonsi over amdgpu): ruled out for this
  host, whose only render node is itself a virtio-gpu node, so virglrenderer's
  native renderer cannot initialise and libkrun strips the DRM capset - see
  `docs/gpu-native-context-investigation.md`. This map is the vrend video path
  only.
- VA-API in `--waypipe --software-renderer` mode: llvmpipe ships no VA-API
  driver, so there is nothing to enable there.
