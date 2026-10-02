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
