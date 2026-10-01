---
label: wayfinder:research
title: The first frames of a guest encode are black
status: open
blocked_by: []
claimed_by:
---

## Question

Every VA-API encoder context in a `--gpu=drm` guest loses its first ~5 frames: the
encoder reads a zeroed surface, so the coded stream opens with black frames and
only then carries the picture. Where does the input go missing between the
guest's VA surface and the host's encoder?

## Evidence already gathered

- Per-frame coded sizes of one guest encoder context (10-frame testsrc 640x360,
  H.264): `287, 18, 18, 18, 18, 14227, 3512, ...`, and the decoded stream's first
  five frames are 100% black (`ffmpeg -vf blackframe=amount=0:threshold=32`:
  `frame:0..4 pblack:100`, `frame:5.. pblack:12`). Even the opening I-frame is
  contentless, and the input only lands at frame 6.
- It is per encoder context, not per guest: three encodes in one guest run
  (black, testsrc, white, 10 frames each) each show the same
  `97, 17, 17, 17, 17, ...` pattern, as do the 30-frame and 600-frame runs.
- It is not radeonsi: the identical command on the host against the same render
  node, with no vrend in the path, carries content from frame 0 (`pblack:12` on
  every frame, sizes `31, 8, 162, 5030, ...`).
- The guest's own VA surface is correct: `hwupload,hwdownload` round-trips of
  testsrc/black/white/testsrc2 give four different images, and a guest software
  x264 encode decodes back fine.
- The coded-buffer read-back is faithful (ticket 02), which leaves the host-side
  input copy: `virglrenderer-1.3.0/src/vrend/vrend_video.c:283`
  (`vrend_video_enocde_upload_picture`) -> `:210` (`sync_video_buffer_to_dmabuf`),
  which passes `EGL_DMA_BUF_PLANE0_*` attributes for every plane and never a
  modifier, while the surface it imports was exported with a tiled modifier.
- Observed alongside: the guest's stream has no B-frames (`type:I`/`type:P` only)
  where the host control emits `type:B`, so some picture-type configuration also
  fails to cross the wire.

## Deliverable

- The mechanism behind the ~5 lost frames in the host's encode path, with the
  same kind of evidence the encode ticket used (a host-side trace of the upload
  and the encoder's input surface, or a guest probe that makes the input
  unmistakable per frame).
- A verdict on whether cang can carry a fix in its `virglrenderer` patch set - a
  modifier-carrying EGL import, a missing wait/flush on the upload, or the
  encoder's VA surface pool being created lazily - or whether upstream owns it,
  in which case a guest encode should at least report the loss.
- Whether the missing B-frames are the same defect.
