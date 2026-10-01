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

## The transfer is clean: the loss is inside the encode

With the process question settled (the VM worker and the render server both map
`.../lib/libvirglrenderer.so.1` from the build under test, and the debug file is
visible from the host at `/dev/shm/chroma-dbg2.log`), the per-row dump answers the
question it was built for. Encoding solid red, **every sampled row of both planes
matches, source and destination, with no zero bytes**:

```
plane 0: 320x240 pitch 512 off 0      fmt 0x20203852 res 320x240
  row   0/30/60/90/120/150/180/210: src=51515151 zeros 0/320 | dst=51515151 zeros 0/320
plane 1: 320x240 pitch 512 off 131072 fmt 0x38385247 res 160x120
  row   0/15/30/45/60/75/90/105:    src=5af05af0 zeros 0/320 | dst=5af05af0 zeros 0/320
```

`51` = Y 81, `5a f0` = U 90 / V 240 (BT.601 red): the guest's source resource and
the encoder's VA surface hold exactly the right picture, on both planes, on every
sampled row. So *neither* the EGL import (as attempt 1 already showed) *nor* the
blit loses anything, and the two plane-layout suspicions are dead.

What the encoder then writes is still wrong, and the shape of the damage says the
fault is in the coding parameters rather than in the data: on the decoded frame,
the chroma is correct in ~40% of samples with `0x00` in ~26%, scattered over the
whole plane rather than aligned to rows or to a clean block grid (zero-U fraction
per 8x8 cell of the chroma plane: the first row of cells is clean, the rest ranges
0.05 to 0.79).

That points at what vrend tells the encoder about the picture: the
`VAEncPictureParameterBufferH264`/slice fields vrend renders itself (the chroma QP
and prediction fields are the ones that would do exactly this), against the same
fields in a native encode. The next experiment is the one that found the packed
headers: refresh the host-side libva trace of the guest path
(`LIBVA_TRACE` in the launcher) and diff its picture/slice parameter fields
against the host control's trace for the same content.

## The trace took a different channel than expected

Capturing what vrend *submits* to the driver needs a working trace, and
`LIBVA_TRACE` is not it: libva's tracer works for a native client (the host
control's trace is written, and shows the native picture parameters for a solid
red frame - `pic_init_qp = 26`, `pic_fields = 0x10b` i.e. CABAC and the 8x8
transform on, `idr_pic_flag`/`reference_pic_flag` set, both chroma QP offsets 0,
and 5 packed-header pairs) but never produces a file for the guest path, because
cang hands the render server a curated environment and libva's tracer is not in
it. `VIRGL_LOG_LEVEL`/`VIRGL_LOG_FILE` *are* passed through - proven by the
`virgl_debug` output written from `enc_render_raw_headers` in earlier runs - so
vrend's own VA submission has to be logged from vrend's code through that
channel, not through libva.

For the record, the same solid-red frame costs **291 bytes** on the host and
**1019 bytes** in the guest (3 frames each), i.e. the guest's encoder is 3.5x
larger on the simplest possible content - the same order as the 2.4x measured on
real video.

Next: a `virgl_debug` print of the fields vrend fills into
`VAEncPictureParameterBufferH264`/`VAEncSliceParameterBufferH264` in
`h264_encode_bitstream`, read through `VIRGL_LOG_FILE`, diffed against the native
trace above.

## Instrumenting vrend's VA submission: what the dump can and cannot read

The dump has to live in vrend's own code and be read through `VIRGL_LOG_FILE`
(libva's tracer never reaches the render server). Two attempts to build it failed
to compile, and the errors are themselves informative about the wire structs:

- `virgl_h264_enc_picture_desc` has **no** `num_ref_frames` and **no** `pps`
  member.
- Its `seq` member, `virgl_h264_enc_seq_param`, carries only
  `enc_constraint_set_flags`, the four cropping offsets, `pic_order_cnt_type`,
  `num_temporal_layers` and the VUI fields - **no geometry and no
  `chroma_format_idc`**, which is exactly what ticket 02 found when it had to
  fill those from the codec instead.

So a working dump reads the picture side from the desc's own fields
(`quant_i_frames`/`quant_p_frames`/`quant_b_frames`, `frame_num`,
`pic_order_cnt`, `picture_type`, `not_referenced`,
`num_ref_idx_l0_active_minus1`/`_l1_`, `num_slice_descriptors`) and the sequence
side from the **codec** (`codec->width`/`height`/`chroma_format`/`level`/
`max_references` - the same values the host patch writes into the VA sequence
buffer), then it has to drop the `(void)codec` line.

Reading the fill code for the dump turned up four things worth checking once the
dump is in place, in the order they would explain a chroma-specific loss:

- `param->CurrPic.picture_id = get_enc_ref_pic(codec, desc->frame_num)` while
  `source` is ignored outright (`(void)source`) - the encoder is told the picture
  is a *reference-list* surface, not the surface the upload wrote into;
- only the *last* entry of `desc->slices_descriptors` becomes a VA slice, so a
  frame the client slices into several pieces is encoded as one;
- `transform_8x8_mode_flag` is left 0 while the native client sets it
  (`pic_fields = 0x10b`), and `deblocking_filter_control_present_flag` likewise;
- `chroma_qp_index_offset`, `second_chroma_qp_index_offset`, `pic_order_cnt_lsb`
  and `frame_num` are deliberately left 0 (both parameter structures *are*
  `memset` first, so they are zeros rather than stack garbage).
