---
label: wayfinder:research
title: Where the guest VA-API encode loses its bitstream
status: open
blocked_by: []
claimed_by:
---

## Question

In a `--gpu=drm` guest, `vainfo` advertises `VAEntrypointEncSlice` for H.264 and
HEVC, and `ffmpeg -vaapi_device /dev/dri/renderD128 -vf format=nv12,hwupload
-c:v h264_vaapi` runs to completion and writes a file - but the file is not a
bitstream: the whole 87510-byte output of a 30-frame 640x360 encode is a single
access unit (`missing picture in access unit with size 87510`) that neither a
software nor a VA-API decode accepts, and the bytes are real (86947 of 87510 are
non-zero), not an empty coded buffer.

The surrounding controls say the loss is in the guest's encode path, not in the
parts around it:

- The identical `h264_vaapi` command on the **host** (the same vrend to libva
  encode, one nesting level up) produces a stream that software-decodes cleanly.
- A **software** `libx264` encode in the guest, decoded back through the guest's
  **VA-API decoder**, works - so the guest's VA decode path and its container/mux
  plumbing are fine.
- `hevc_vaapi` in the guest also runs, with ffmpeg warning that the driver "does
  not advertise encoder features" and "encoder block size" (guessed defaults).

So, reading the pinned sources:

1. Where does the coded buffer's read-back break - the guest mesa
   `virtio_gpu_drv_video.so` encode front-end, virglrenderer 1.3.0's
   `virgl_video.c`/`vrend_video.c` encode path, or the protocol between them?
   Name the file and line where the bitstream stops being a bitstream (e.g. a
   coded buffer that is never written back, a wrong size, a missing
   `vaEndPicture`/coded-buffer export, or a guest-side `derived` image that is
   read as if it were the coded data).
2. Is the encode caps query part of the same gap? In the guest only,
   `vaGetConfigAttributes`-style queries come back empty (ffmpeg's "does not
   advertise encoder features"), while the host answers them.
3. Given the answer: is this fixable inside cang's stack (virglrenderer 1.3.0,
   the image's mesa 26.1.8, libkrun's fork), or is it upstream work in
   mesa/virglrenderer that cang cannot carry? If cang cannot fix it, what should
   it do about the advertised encode entrypoints until then - leave them (a trap:
   `ffmpeg` exits 0 and writes an unusable file), hide them
   (`virgl_video_fill_caps`'s supported-entrypoint table decides what the guest
   is told), or document them?

## Deliverable

A one-line verdict per item, with the code path (`file:line`) that loses the
bitstream, and - for item 3 - a recommendation with the cang-side change it would
take. Reading the pinned sources is the core of it; a live guest probe (the
probe scripts and the hermetic store used for ticket 01 are still on the host
btrfs disk) is worth it only where the source reading is ambiguous.

## Evidence already gathered

- `docs/vaapi-video-investigation.md` - the Resolution and the "Encode does not
  work yet" bullet: sizes, the decoder messages, the host and software controls.
- virglrenderer source for the encode path is readable from the cang host store
  (`/nix/store/*virglrenderer-1.3.0/` is a dev output; cang's own patches to it are
  `nix/pkgs/patches/virglrenderer-*.patch`), and the guest side is mesa's
  `virtio_gpu_drv_video.so`, which the image already carries.
