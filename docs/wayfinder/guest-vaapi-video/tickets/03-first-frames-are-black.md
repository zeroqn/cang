---
label: wayfinder:research
title: The first frames of a guest encode are black
status: closed
blocked_by: []
claimed_by: bob + pi session (2026-10-01)
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

## Resolution

The upload writes the picture into the buffer the VA encoder reads, and the two
only share memory: `vrend_video_enocde_upload_picture` ->
`sync_video_buffer_to_dmabuf` (`virglrenderer-1.3.0/src/vrend/vrend_video.c:283`
-> `:210`) issues its `glCopyTexSubImage2D`s, and `virgl_video_begin_frame`
(`src/vrend/virgl_video.c:905`) calls `vaBeginPicture` immediately afterwards
with nothing ordering the GL commands against the VA submission. So the encoder
started on a buffer whose copy had not executed yet - black - until, a few frames
in, the accumulated GL work happened to be complete by the time the encode was
submitted. Upstream has no wait in that path either (no `glFinish`/`glFlush` in
`src/vrend/vrend_video.c`, and no `origin/main` commit after `virglrenderer-1.3.0`
touches it). It is also encode-only by construction: the decode direction writes
and is read by the guest through later GL commands in the same context, while
the encode crosses from GL to the VA engine.

Fixed by `nix/pkgs/patches/virglrenderer-encode-upload-fence.patch`: a
`glFinish()` after the blits at the end of `sync_video_buffer_to_dmabuf`. It is a
full pipeline wait per encoded frame - correctness first; a fence would be the
cheaper shape if this ever shows up in a profile.

Verified in a `--gpu=drm` guest (10-frame testsrc 640x360, image VA driver,
patched cang, same probe before and after):

| | per-frame coded sizes | blackframe |
| --- | --- | --- |
| before | `287, 18, 18, 18, 18, 14227, 3064, ...` | `frame:0..4 pblack:100` |
| after | `14530, 3081, 3417, 3084, 2968, 2970, 3209, ...` | `frame:0.. pblack:12` |

HEVC behaves the same (`179, 22, 22, ...` -> `29797, 6753, 6500, ...`), both
streams still software-decode with exit 0, and the guest's VA decode path is
unaffected. The host half of the fix ships with `.#virglrenderer`, so the image
itself is unchanged.

The missing B-frames are a different defect: the fence changed them not at all -
the guest's stream is still `type:I`/`type:P` where the host control emits
`type:B`, so some picture-type configuration does not cross the wire.
