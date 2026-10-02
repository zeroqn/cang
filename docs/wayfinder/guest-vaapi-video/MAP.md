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

Also worth adding as a second client: mpv `--hwdec=vaapi` reports its decoder choice
explicitly (`Using hardware decoding (vaapi)`) and is *not* in the image - add it to the image
layers, or ship its closure through the shared workspace the way the guest mesa build was
shipped (a host-store `mpv-with-scripts-0.41.0` is already built for that).

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

## Out of scope

- DRM native context (guest-native RADV/radeonsi over amdgpu): ruled out for this
  host, whose only render node is itself a virtio-gpu node, so virglrenderer's
  native renderer cannot initialise and libkrun strips the DRM capset - see
  `docs/gpu-native-context-investigation.md`. This map is the vrend video path
  only.
- VA-API in `--waypipe --software-renderer` mode: llvmpipe ships no VA-API
  driver, so there is nothing to enable there.
