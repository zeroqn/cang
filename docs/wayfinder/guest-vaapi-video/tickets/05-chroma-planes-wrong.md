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

## Attempt 2: the import is fine, the plane is only partly written

A debug build (temporary patch, not carried) read back, inside
`sync_video_buffer_to_dmabuf`, the bytes the host has for each plane: the guest's
source resource (read from the framebuffer the blit copies from) and the
encoder's destination texture (the imported VA surface plane). Encoding a solid
red frame in the guest, row 0 of both is exactly right:

```
plane 0: dl 320x240 pitch 512 off 0      fmt 0x20203852  res 320x240 src=51 00 ... dst=51 00 ...
plane 1: dl 320x240 pitch 512 off 131072 fmt 0x38385247  res 160x120 src=5a f0 ... dst=5a f0 ...
```

`51 00` = Y 81 and `5a f0` = U 90 / V 240, i.e. the luma and chroma of BT.601 red;
`0x38385247` is `GR88`, so the wire already describes the chroma plane with its
own fourcc (`fill_video_dma_buf` sets the layer's format, but here the plane
arrives from the guest already correct) - which is why attempt 1 changed nothing.
`res 160x120` confirms the chroma resource is chroma-sized.

So the upload's *first row* is correct on both sides, and the decoder says the
rest is not: decoding that stream and looking at the chroma plane, the correct
values appear in ~40% of samples while **~26% are `0x00`** (U mode 89 in 7335 of
19200, 0 in 5228; V mode 241 in 7968, 0 in 3680) with luma 99.6% correct. Zeros in
the chroma of a solid-colour frame mean the encoder's chroma input had zeros
there, i.e. the destination plane is written only in part.

That moves the suspicion from the EGL attributes (correct, as measured) to the
blit's *extent*: `glCopyTexSubImage2D(..., res->base.width0, res->base.height0)`
from a framebuffer that holds the guest's plane resource, into an EGLImage-backed
texture whose real layout is the VA surface's (pitch 512, chroma offset 131072).
Next: dump the whole destination plane (per-row modes) in the same debug build to
see *which* rows are stale, which decides between the copy's extent and the
driver's view of the imported texture.

## Instrumentation snag (2026-10-01)

The debug read-back that produced the row-0 dump and the per-row variant of the
same code behave differently in ways the code cannot explain, and it matters for
the next attempt:

- The first debug build (`cang-dbg`, virglrenderer `5f4wbv...`) wrote its lines to
  `/dev/shm/chroma-debug.log` on every frame.
- Three rebuilt variants (including one that writes a *unique* file name and
  `fprintf(stderr, ...)` at the top of the block) executed no debug code at all:
  no file, no stderr marker in the console log, while the guest's encode still ran
  and the stream is the same size.
- Each build's `libexec/cang-helpers/virgl_render_server` symlink points at its own
  `virglrenderer` store path, and the per-row format string is present in the
  later build's `lib/libvirglrenderer.so.1`.

So the process that runs vrend's video path in a given run is not simply "the
virglrenderer this cang was built with", and the first instrumented build is not
proof of where the bytes went. Before trusting any further read-back, find the
process: instrument a place that cannot move (the guest-visible side of the wire,
or cang's own video glue rather than vrend), or log through cang's logger and a
path that both namespaces share. Also note `/dev/shm/chroma-debug.log` is owned by
the VM's mapped uid in a sticky directory, so a host-side `rm` silently fails and
an old file can be mistaken for fresh output - which is how the first null reading
of the per-row dump happened.
