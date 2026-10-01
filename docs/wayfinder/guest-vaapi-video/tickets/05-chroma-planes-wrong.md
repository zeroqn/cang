---
label: wayfinder:research
title: The guest encode's chroma planes are wrong
status: open
blocked_by: []
claimed_by:
---

## Question

A `--gpu=drm` guest's H.264 encode decodes cleanly but is not the picture: the
luma is exactly as good as the host's, and the chroma is badly damaged. Where
does the chroma go wrong between the guest's VA surface and the encoder's input?

## Evidence already gathered

10 s of a real video (640x360, `sample.mp4`), H.264 VA-API, CQP 26, B-frames off,
one run in a `--gpu=drm` guest and one on the host against the same render node
with no vrend in the path:

| | guest | host | ratio |
| --- | --- | --- | --- |
| QP 20 | 7 705 430 B | 3 426 372 B | 2.25x |
| QP 26 | 3 979 917 B | 1 671 174 B | 2.38x |
| QP 32 | 2 196 364 B | 879 340 B | 2.50x |

so the requested QP does reach the encoder (both scale as expected) and the
guest still spends ~2.4x the bits. Decoding both streams and comparing against
the source (same ffmpeg, `-lavfi psnr,ssim`):

| | PSNR y | PSNR u | PSNR v | SSIM All |
| --- | --- | --- | --- | --- |
| guest stream | 28.126378 | 15.926191 | 15.642029 | 0.886263 |
| host stream | 28.126378 | 43.619529 | 41.310856 | 0.957002 |

The luma figures are *identical to six decimals*, so the encoder's luma decisions
and the input luma are right; the chroma is ~28 dB worse, which is roughly "the
chroma planes are not the picture". That also explains the bitrate: the encoder
spends 2.4x the bits coding chroma noise and still reproduces it badly.

This also corrects an assumption behind tickets 02 and 03: "software-decodes with
exit 0" was the acceptance evidence there, and a stream can decode cleanly while
carrying the wrong chroma. The encode verification should include a PSNR/SSIM
comparison against the source from now on.

## Candidate sites

- `virglrenderer-1.3.0/src/vrend/vrend_video.c:270-300`,
  `sync_video_buffer_to_dmabuf`: the per-plane EGL import uses
  `EGL_DMA_BUF_PLANE0_FD_EXT`/`OFFSET`/`PITCH` for *every* plane (with the
  plane's own fd/offset/pitch, so it is the single-plane-fourcc scheme), sets
  `EGL_WIDTH`/`EGL_HEIGHT` to `dmabuf->width / (i + 1)` /
  `dmabuf->height / (i + 1)`, and never passes the surface's modifier (the
  export in the trace carried `modifier = 0x200000018601b04`).
- The guest's copy-out of the surface into that dmabuf
  (`virgl_video.c`'s encode path, `vrend_video_enocde_upload_picture`).
- The upload/luma ordering fixed by ticket 03 (`glFinish`): luma now arrives, so
  the same wait may be insufficient for the second plane, or the plane geometry
  may simply be wrong for NV12.

## Deliverable

- The mechanism, named at the line that misdescribes or loses the chroma plane.
- A fix in cang's `virglrenderer` patch set if it is cang-side, or a precise
  upstream report if not.
- Verification: the guest's PSNR/SSIM against the source matching the host's
  (chroma within ~1 dB), the bitrate gap closed, luma unchanged.

## Attempt 1, 2026-10-01: the per-plane EGL format is not it

Hypothesis: `sync_video_buffer_to_dmabuf` describes every plane with
`dmabuf->planes[i].drm_format`, which is the *layer's* fourcc, so the NV12 chroma
plane would be imported as "NV12 plane 0" (one byte per pixel) while its pitch is
two - half of every row copied, the rest stale. That is the plane's format mix-up
the struct invites (`fill_video_dma_buf` sets `plane->drm_format =
desc->layers[i].drm_format` for every plane).

Implemented as `virglrenderer-encode-chroma.patch`: a `plane_layout_for()`
helper returning the per-plane single-plane fourcc and geometry (NV12 -> R8 +
GR88, P010 -> R16 + GR1616, planar YUV -> R8 with the right subsampling, packed
formats unchanged), used in both `sync_video_buffer_to_dmabuf` and
`sync_dmabuf_to_video_buffer`.

Measured in the same guest with the same probe: **bit-identical results** - QP 26
size 3 979 917 B and PSNR `y:28.126378 u:15.926191 v:15.642029`, exactly the
values before the patch, at every QP. So the chroma damage is *not* in that EGL
import (whatever the correct per-plane description is, this is not the fault).
The patch is not carried.

What that leaves, in order of suspicion: the guest's own upload of its VA surface
into the virgl texture the host blits from (a guest-side transfer, in cang's
mesa build, so patchable here), the blit's source extent (`res->base.width0`,
`res->base.height0` for a plane resource), or the plane's `res_handle` selection
when the guest uses one resource for a multi-plane surface. The next step is
instrumentation rather than another hypothesis: read back the source texture and
the destination plane in `sync_video_buffer_to_dmabuf` (a debug-only build) and
compare the bytes against a synthetic chroma pattern in the guest, which names
the side that loses the chroma.
