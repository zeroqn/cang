---
label: wayfinder:task
title: Enable the vrend VA-API video path (decode)
status: closed
blocked_by: []
claimed_by: bob + pi session (2026-10-01)
---

## Question

Can a `cang --gpu=drm` guest get the host's VA-API decode instead of
`VAProfileNone` alone, without losing venus or virgl GL?

Context: cang already set `VIRGL_RENDERER_USE_VIDEO` (bit 11) in its virtio-gpu
virgl flags word, and virglrenderer 1.3.0 is built with `-Dvideo=true` and links
libva 2.23.0, yet the guest's `vainfo` reported only `VAProfileNone`.

## Resolution

**Yes.** The request never reached the renderer. Upstream `rutabaga_gfx`'s
`VirglRendererFlags` stopped at `VIRGLRENDERER_DRM` (bit 10) and its
`RutabagaBuilder` had no video setter, so libkrun had nothing to forward and
`virgl_renderer_init` never enabled vrend's video path (no `virgl_video_init`,
no `vaInitialize`). Nothing in the guest kernel was involved.

Three parts, all landed:

- `zeroqn/rutabaga_gfx` branch `cang` (`d8479a1`): add
  `VIRGLRENDERER_USE_VIDEO`, `VirglRendererFlags::use_video` and
  `RutabagaBuilder::set_use_video`.
- `zeroqn/libkrun` branch `cang` (`2855f4d1`): pin `rutabaga_gfx` at that rev and
  forward cang's bit with `set_use_video`.
- cang guest-init: export `LIBVA_DRIVERS_PATH=/usr/lib/cang-mesa-runtime/lib/dri`
  with the `--gpu=drm` Mesa environment. libva's default search paths
  (`/run/opengl-driver/lib/dri`, `/usr/lib*/dri`) do not include the mesa runtime
  directory, so without it `va_openDriver()` finds no driver and `vaInitialize`
  fails before anything above matters.

Verified in a live `--gpu=drm` guest (guest kernel 7.2.7-hardened1) running the
packaged `nix build .#cang`:

- `vainfo` lists H.264 ConstrainedBaseline/Main/High (VLD + EncSlice), HEVC
  Main/Main10 (VLD + EncSlice), VP9 Profile0/Profile2, AV1 Profile0 and JPEG
  Baseline (VLD).
- `ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128
  -hwaccel_output_format vaapi` decodes a 1080p H.264 clip into `vaapi` surfaces,
  34 frames with 0 decode errors.
- No regression: venus (`DRIVER_ID_MESA_VENUS`) and virgl GL 4.6 are unchanged
  and `tools/chromium-cang-smoke` reports `VERDICT: PASS`.

Detail, probe commands and the two probe traps (a guest `ffmpeg` stopping on
`SIGTTOU` without `-nostdin`, and a `target/debug/cang` needing
`CANG_RENDER_SERVER_POLICY`) are in `docs/vaapi-video-investigation.md`.
