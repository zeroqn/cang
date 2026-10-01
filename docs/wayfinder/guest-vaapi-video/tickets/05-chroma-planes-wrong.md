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

## The dump is built, loaded, and never runs

A working dump was built after fixing the brace surgery (the compile errors above
were mine, not the code's) and it writes to `/dev/shm/h264params.log` directly -
the channel the per-row dump proved. Then, in a cleaned-up run (no stale VMs) with
that build:

```
cang: /nix/store/9ixc0n6ds7073dahg0k29l6yxd0kkp27-cang-0.11.2/bin/cang
console: encode exit=0 size=1006        (the guest's encode succeeds)
/dev/shm/h264params.log: No such file or directory
```

while the same run's processes map exactly that build's library, and the library
contains the dump's strings:

```
pid 479225 virgl_render_se lib=.../crn5siv1vn0axm7ardr4ml8gm1aq7hc1-virglrenderer-1.3.0 H264PIC=1
pid 479242 cang            lib=.../crn5siv1vn0axm7ardr4ml8gm1aq7hc1-virglrenderer-1.3.0 H264PIC=1
```

`h264_fill_enc_picture_param` and `h264_fill_enc_slice_param` are called
unconditionally from `h264_encode_render_picture`/`h264_encode_render_slice`
(`src/vrend/virgl_video.c:1697-1713`, the same function that submits the raw
headers), so **the guest's H.264 encode does not go through
`h264_encode_bitstream` at all** - which contradicts the assumption this ticket has
been built on. The raw-header patch of ticket 02 changed the guest's output
(SPS/PPS appeared, and the stream became decodable), so *something* in that path
does run; the two facts have to be reconciled before any parameter-level
conclusion is drawn.

Next, in this order:

1. instrument the entry points around it - `virgl_video_create_codec` (which
   profile/entrypoint the guest asks for), `virgl_video_encode_bitstream` and the
   CCMD dispatch in `vrend_decode.c` - with the same direct file write, to see
   which of them run for a guest encode;
2. if none of them do, the guest's encode is being served somewhere else entirely
   and the search moves to the guest side (mesa's virtio_gpu VA driver, or whether
   the guest's `ffmpeg` really used `h264_vaapi` against the venus/virgl device);
3. only then read the parameters and diff them against the native trace.

## None of vrend's video entry points run either

Same method, one level up: direct-file markers in `virgl_video_create_codec`
(printing the wire's profile/entrypoint and their VA mapping) and in
`virgl_video_encode_bitstream` (printing the picture's and the codec's profile).
Built clean, ran a full guest encode with it, and again no file at all:

```
console: encode exit=0 size=1006
/dev/shm/vrendpath.log: No such file or directory
```

So for this configuration a guest VA-API encode does not reach
`virgl_video_create_codec`, `virgl_video_encode_bitstream`,
`h264_encode_bitstream`, `h264_fill_enc_picture_param` or
`h264_fill_enc_slice_param` - i.e. **not vrend's video path at all** - while the
guest nevertheless produces a decodable H.264 stream with the host's
characteristics (SPS/PPS matching the client's bytes, luma matching the native
encode to six decimals).

That makes the ticket's premise wrong, and the two things it was built on need
reconciling with it:

- the ticket-02 patches *did* change the guest's output (before them the stream had
  no parameter sets and no decoder accepted it), so some code they touch does run
  for a guest encode;
- the per-row read-back dump *did* fire, but its function,
  `sync_video_buffer_to_dmabuf`, belongs to the *upload* path
  (`vrend_video.c`), not to the encode path (`virgl_video.c`).

The next candidates, in order:

1. the guest's own VA driver: mesa's `virtio_gpu_drv_video.so` may implement
   encode on the guest side (or route it away from vrend's video code), which also
   decides whether the guest's client ever sends the `VIRGL_CCMD_*` video commands
   in this configuration;
2. the native-context path (`--gpu=drm` with a DRM native context / venus-style
   submission, as in the WebGL work), where the encode could reach the host's Mesa
   directly and never touch vrend;
3. `vrend_decode.c`'s CCMD dispatch, to see which video CCMDs are even handled in
   this build (a marker per case, rather than per function).

## The guest-side driver decides the stream, and the encoder process is found

Two measurements from this round, both reproducible.

**A/B on `LIBVA_DRIVERS_PATH` inside one guest** (same command, 10 frames, CQP 26,
`-bf 0`):

| driver dir | size | first bytes | decode |
| --- | --- | --- | --- |
| `/usr/lib/cang-va-runtime/dri` (image's patched driver) | 31 666 | `00 00 00 01 67 64 0c 1e ...` (SPS) | exit 0 |
| `/usr/lib/cang-mesa-runtime/lib/dri` (prebuilt driver) | 31 468 | `00 00 00 01 00 88 80 4f ...` (no SPS) | exit 69, "Format h264 detected only with low score" |

So the guest-side half of the ticket-02 fix is what makes the stream carry the
client's parameter sets; with the unpatched driver the same command still produces
the parameter-set-less stream this ticket started from.

**The process that holds the GL/VA driver during a guest encode** (sampling
`/proc/*/maps` every 2 s through a run):

```
t=2..12 pid=497066 uid=165536 comm=cang libs=libgallium-26.1.8.so libLLVM.so.21.1
    /dev/shm: chroma-dbg2.log chroma-debug.log libpod_rootless_lock_1000 ... va-codedbuf
```

i.e. a `cang` process inside the user namespace (uid 165536), with the *same*
`/dev/shm` the host sees - not the uid-1000 render server, and not a private mount.
That is exactly the combination under which the dump should have written its file,
so the "no dump" result above and this measurement together mean the *encode-time*
video code being executed is not the one reached by the marker I placed; two
explanations remain, and they are testable in one run:

1. the video path runs in the render server's *sandboxed* child and its `/dev/shm`
   write is denied by the sandbox (my code ignores `fopen` failure - a bad habit
   here), in which case the dump has to go through a channel the sandbox allows;
2. a second copy of the video code is being executed - the candidate is
   `libkrun.so` (whose closure includes `virglrenderer`, and whose own
   `virglrenderer` would carry the video code without my marker).

Next measurement (one run, no new code): during a guest encode, print for *every*
process in the tree `/proc/PID/maps` filtered for `virglrenderer|libkrun`, plus
whether `/proc/PID/root/dev/shm/h264params.log` exists in *its* view - that
distinguishes (1) from (2) directly.

## The host does not hand the encoded bitstream over either

The marked build was extended with a marker in
`vrend_video_encode_completed` (`src/vrend/vrend_video.c`) - the function that
copies the encoder's per-frame `coded_bufs`/`coded_sizes` into the guest's buffer
and sends the `virgl_video_encode_feedback` - plus the existing parameter and
entry-point markers (verified present in the loaded library:
`ENCBITSTREAM`/`NEWCODEC`/`PIC fn_h264` all `grep`-hit in
`b3zwksrhz1i1b2ilq3v5ib2pp56dd57c-virglrenderer-1.3.0`, which the run's processes do
map). Result, with the guest's encode succeeding (`encode exit=0 size=1006`):

```
/dev/shm/vrendenc.log:  No such file or directory
/dev/shm/vrendmark.log: No such file or directory
```

`/dev/shm` *is* writable for the process that owns the video path - the render
server's environment already writes there (`MESA_SHADER_CACHE_DIR=/dev/shm/mesa-cache`)
- so this is not a denied write: **the host's vrend does not encode this stream**.
Combined with the A/B on the guest's driver (patched driver -> client's SPS and a
decodable stream; prebuilt driver -> the parameter-set-less stream), the picture is
now: the *guest-side* driver decides the outcome, the *vrend* video code (create
codec, encode dispatch, parameter filling, encode completion) never runs, and the
upload path is the only marked host code that has ever written a file.

The next measurement therefore has to be on the **guest** side, where a marker is
cheap and its output lands in the shared workspace: instrument the guest's
`virgl_video_encode_bitstream`/`fill_h264_enc_*` (cang's `mesaVaApi` build) and see
whether the guest's VA client takes the virgl VA path at all, or whether its
`virtio_gpu_drv_video.so` reaches the host through the DRM native context / venus
route instead (which would explain host-grade luma, the chroma damage, and the
absence of every vrend marker).

Incidental, not part of this ticket: one `nix build .#cang` in this round failed on
`guest_init::components::podman::service::tests::rootless_info_verification_times_out_and_reaps_child`
(`assertion failed: err.to_string().contains("timed out")`) and passed on an
immediate retry - a load-sensitive test, worth knowing about when the machine is
busy with builds.

## Correction: the guest *does* take the virgl VA path

A marker placed on the **guest** side - in mesa's own
`virgl_video_encode_bitstream` (`src/gallium/drivers/virgl/virgl_video.c:1016`,
the function that copies the picture description onto the virgl wire and calls
`virgl_encode_encode_bitstream`), written to stderr and shipped to the guest as a
per-run driver directory in the shared workspace (`mesa-gm/dri`, the same
mechanism the earlier driver A/B used) - **fires immediately**:

```
GUESTMARK encbitstream profile=11 picture_type=3 fnum=0 qp_i=26 qp_p=0 qp_b=0 ndesc=1 idr_period=120 chroma=2 w=320 h=240
GUESTMARK encbitstream profile=11 picture_type=0 fnum=1 qp_i=26 qp_p=26 qp_b=0 ndesc=1 idr_period=120 chroma=2 w=320 h=240
```

(profile 11 = `PIPE_VIDEO_PROFILE_MPEG4_AVC_HIGH`, picture type 3 = IDR,
chroma 2 = 4:2:0, one slice descriptor.) The same probe with the image's
`cang-va-runtime` driver logs no marker, as expected.

So the guest's VA client goes through mesa's **virgl** VA path and puts the encode
on the virgl wire - not through a DRM native context or a venus route - which
means the host's vrend *must* handle it. That reverses the previous section's
conclusion: the silent host markers are an **instrumentation** problem, not
evidence that the host does not encode. The independent evidence agrees: the
image's driver produces a stream carrying the *client's* SPS bytes, and nothing but
the host's raw-header submission (ticket 02's host patch) can put those into the
encoded stream.

What remains is to find a sink that works from the host's render-server process (the
same `fprintf(stderr, ...)` that works in the guest, plus `/tmp` and `/dev/shm`
variants, in one build), then read the parameter fields and diff them against the
native trace. The earlier `/dev/shm`-only marker in `virgl_video.c` was written by a
process that either is not the one encoding or cannot write there - which of the two
is settled by the three-sink build.

## The guest enters the virgl encode path and the host still runs no video code

A three-sink marker build (a `vrendmark()` helper writing to `stderr`, `/tmp` and
`/dev/shm`, used in `h264_fill_enc_picture_param`, `virgl_video_create_codec`,
`virgl_video_encode_bitstream` and `vrend_video_encode_completed`) was run with a
sampler that, for every process mapping any virglrenderer, records the library path,
whether *that file* contains the marker strings, and whether the marker file exists
in that process's **own** `/dev/shm` and `/tmp`:

```
t=10 pid=981676 uid=1000   comm=virgl_render_se lib=.../0gwv0i09...-virglrenderer-1.3.0 marked=1 ownshm=0 owntmp=0
t=10 pid=981377 uid=1000   comm=cang            lib=.../0gwv0i09...-virglrenderer-1.3.0 marked=1 ownshm=0 owntmp=0
t=10 pid=981690 uid=165536 comm=cang            lib=.../0gwv0i09...-virglrenderer-1.3.0 marked=1 ownshm=0 owntmp=0
   (three more uid-165536 cang workers, same library, same zeros)
```

with the guest's encode succeeding, the host sinks empty, and no marker in the
console log. So the library that runs *is* the marked one (`marked=1`), no process
has a private copy of the sink, and no host video hook executes.

Together with the guest-side marker, the chain is now:

- the **guest** enters mesa's `virgl_video_encode_bitstream` on every frame
  (`GUESTMARK ... profile=11 picture_type=3 ...`) - the marker sits at the *top* of
  that function, so what it proves is that the guest's VA client *chooses* the virgl
  VA path, not that the command reaches the host;
- the **host** runs none of the code that would consume such a command.

So the encode command (or the codec creation before it) is being lost or rejected
between the guest's virgl context and cang's vrend, while the guest still receives a
plausible stream carrying the client's SPS - which means the last unexamined link is
the **guest-side submission itself**: mesa's virgl context (`virgl_encode_send_cmd`
and the `VIRGL_CCMD_*` ids it emits for the video commands) and whatever rutabaga /
libkrun does with them. The next marker belongs there, on the guest side where the
output is easy to read, together with the corresponding host-side CCMD dispatch in
`vrend_decode.c` (which the earlier plan already called for).

## Video CCMDs at the host dispatch: nothing (with a caveat)

The dispatch site in `src/vrend/vrend_decode.c:2098`
(`ret = decode_table[cmd](gdctx->grctx, buf, len);`) got a sink that prints every
video CCMD it dispatches - all eight of `CREATE_VIDEO_CODEC`, `DESTROY_VIDEO_CODEC`,
`CREATE_VIDEO_BUFFER`, `DESTROY_VIDEO_BUFFER`, `BEGIN_FRAME`, `DECODE_BITSTREAM`,
`ENCODE_BITSTREAM`, `END_FRAME` - through the three-sink writer
(`stderr` + `/tmp/vrendcmds.log` + `/dev/shm/vrendcmds.log`), and the build's library
was confirmed to carry it. A guest encode with that build produced nothing:

```
encode exit=0 size=1006
/dev/shm/vrendcmds.log: No such file or directory
/tmp/vrendcmds.log:     No such file or directory
console: 0 VIDEOCMD lines
```

Read one way, the guest's `VIRGL_CCMD_ENCODE_BITSTREAM` never reaches vrend's
dispatch. Read the other, the host-side sink is simply not writable from whichever
process dispatches - which is *not* excluded, because the only host-side write that
has ever been observed worked from a function in `vrend_video.c`
(`sync_video_buffer_to_dmabuf`) while every marker in `virgl_video.c` and
`vrend_decode.c` has been silent, and the guest-side channel (where `GUESTMARK`
writes to stderr on every frame) is the only one proved reliable.

The discriminating measurement is therefore on the **guest** side, at the
flush/submit boundary (`virgl_context_flush` -> `vs->vws->submit_cmd` in mesa's
virgl driver), with two markers: one for a CCMD known to work from earlier evidence
(`BEGIN_FRAME`, whose handler's callee wrote the one host-side file ever seen) and
one for `ENCODE_BITSTREAM`. If both are flushed but only `BEGIN_FRAME` reaches the
host, the encode command is lost between the guest's virtio-gpu submit and cang's
vrend (rutabaga/libkrun), which is where cang can act; if `ENCODE_BITSTREAM` is
never flushed at all, the loss is in mesa's own virgl context.

## Reinterpretation: the guest's "encoded" file is ffmpeg's own headers plus unencoded memory

Two facts, each checked twice, force a different reading of everything above.

1. **A control marker in the *upload* path is silent in the current build** - and
   the reason is mine, not the host's: the marker went into
   `sync_dmabuf_to_video_buffer` (the *decode* direction), because my anchor matched
   the first of two identical tails in `vrend_video.c`. So the host-side sink
   question is still open, but the earlier *successful* row dump (placed properly
   inside `sync_video_buffer_to_dmabuf` during `enc27`) does prove the sink works and
   that the **upload half of the encode path runs**.
2. **The SPS in the guest's file proves nothing about the host.** ffmpeg's
   `h264_vaapi` builds SPS/PPS itself and puts them in `avctx->extradata`, which the
   `-f h264` muxer writes into the output. The earlier claim that the client's SPS
   bytes in the stream could only come from the host's raw-header submission is
   therefore wrong.

Put together with the measurements already in this ticket - the guest enters mesa's
virgl encode path per frame, the host's upload runs, but no host encoder-parameter
fill, codec creation or encode completion ever runs - the consistent reading is:

- the guest's encode command reaches the host, whose upload half executes;
- the encode itself does **not** execute on the host, so the coded buffer the guest
  reads back is **not an encoded picture**;
- what the guest's `ffmpeg` then writes is its own extradata plus that unencoded
  coded-buffer memory - which explains every observation at once: the size anomaly
  (31 kB for ten frames of a flat colour), the chroma "damage" (~26% zero samples,
  scattered), the luma that matches the native encode to six decimals (it *is* the
  input surface's luma), and why the guest-side driver choice changes the outcome
  (it decides the coded-buffer path and the profile negotiation).

If that is right, the defect is not in the parameter fields at all: it is that the
host's encode step is skipped after the upload, i.e. in
`vrend_video_encode_bitstream` (`src/vrend/vrend_video.c`) or in the codec/buffer
handle lookups it performs before reaching `virgl_video_encode_bitstream`.

Next (small): markers at the entry and each early return of
`vrend_video_encode_bitstream`, printing the handles it was given and the lookup
results, with the upload-path marker as a live control in the same build.

## Where the host's own error messages went

Before adding more markers, one channel question matters. cang's per-VM task logs
(`<state-root>/cang/workspace/tasks/<task>/helper.stdout.log`) do capture the guest's
console and libva messages - the old ones contain
`libva info: Trying to open /run/opengl-driver/lib/dri/virtio_gpu_drv_video.so` plus
`krun_devices::virtio::fs` lookups - which would make vrend's own `virgl_error`
output (e.g. `%s: feedback res %d not found` in `vrend_video_encode_bitstream`,
`profiles not matched` in `virgl_video_encode_bitstream`) readable for free.

My probe runner does not produce those directories: nothing under the state root has
been written in the last three hours, so the probes run without cang's task-log
capture and the host's messages are not collected anywhere I have looked. Either the
next probe should go through the same managed path the harness uses (to inherit those
logs), or the markers have to stay in-library.

The next marker is decided either way: `vrend_video_encode_bitstream`
(`src/vrend/vrend_video.c:801-844`) has four early returns - codec/buffer lookup,
`feed_res`, `desc_res`, `dest_res` - each printing a `virgl_error`, and only the last
line reaches `virgl_video_encode_bitstream`. Marking its entry and each return (with
the handle values and lookup results) names the line that drops the guest's encode,
with a correctly placed upload-path marker (`sync_video_buffer_to_dmabuf`, not the
decode twin that my anchor matched last time) as a live control in the same build.

## Correctly placed markers: the host's encode handler is never entered

`vrend_video_encode_bitstream` was marked by *brace-counting* from its signature (not
by an anchor guess), so the placements are verified: an entry marker after the two
handle lookups, one inside each of the three `virgl_error` branches, and one directly
before the call into `virgl_video_encode_bitstream`. The upload tail
(`sync_video_buffer_to_dmabuf`, the twin that has written a file before - verified at
line 269, immediately before its `return 0;`) was marked as a live control, and so was
`vrend_video_create_codec`. A guest encode with that build:

```
encode exit=0 size=1006
/dev/shm/ctl-upload.log:  MISSING
/dev/shm/ctl-codec.log:   MISSING
/dev/shm/ctl-encode.log:  MISSING
```

So in this run **no host video entry point executed at all** - not even the upload
callback - while the guest's encoder ran to completion. Two readings, and the second
now becomes the working hypothesis:

- the video CCMDs (`CREATE_VIDEO_CODEC`, `CREATE_VIDEO_BUFFER`, `BEGIN_FRAME`,
  `ENCODE_BITSTREAM`, `END_FRAME`) are lost between the guest's virtio-gpu submit and
  vrend's video context;
- and since the *decode* path demonstrably works end to end on the same transport
  (ticket 01), the loss is not the transport as such but something that distinguishes
  the encode path from the decode path - the most likely candidate being **which
  virgl context the guest submits them to**.

That is directly testable and cheap: print `ctx`/context id and the codec handle in
the guest's marker (the guest side is the reliable channel) and the context id at the
host's dispatch, then compare with the context that carries the working decode. If the
encode lands on a context cang never wires to a video context, the fix belongs in
cang's render-server/context setup, not in virglrenderer's parameter code.

## The guest flushes the whole video command set; the host sees none of it

With markers on the **guest's** flush path (`virgl_flush_eq` in mesa's
`virgl_context.c`, which hands `cbuf` to the winsys) and in mesa's video-codec
creation, a three-frame encode (solid red, CQP 26, `-bf 0`, the marked driver
shipped through the shared workspace) shows the complete command sequence leaving the
guest, all on one virgl context:

```
GUESTCODEC handle=14 profile=11 entry=4 chroma=1 w=320 h=240
GUESTFLUSH ctx=0x5c3d6880000 cdw=1056 video=2 ids=53 55          (CREATE_VIDEO_CODEC, CREATE_VIDEO_BUFFER)
GUESTFLUSH ctx=0x5c3d6880000 cdw=1044 video=4 ids=56 57 60 61    (DESTROY/CREATE_VIDEO_BUFFER, BEGIN_FRAME, ENCODE_BITSTREAM, END_FRAME)
GUESTFLUSH ctx=0x5c3d6880000 cdw=1038 video=3 ids=57 60 61
GUESTFLUSH ctx=0x5c3d6880000 cdw=1038 video=3 ids=57 60 61
GUESTFLUSH ctx=0x5c3d6880000 cdw=1074 video=5 ids=54 56 56 56 56  (teardown)
```

(ids from `virgl_protocol.h`: 53 `CREATE_VIDEO_CODEC`, 55/56
`CREATE_VIDEO_BUFFER`/`DESTROY_VIDEO_BUFFER`, 57 `BEGIN_FRAME`, 60
`ENCODE_BITSTREAM`, 61 `END_FRAME`.) Three frames, three `57 60 61` groups, one
context pointer throughout, and the codec creation in the same stream.

So the guest does not merely *choose* the virgl VA path (the earlier `GUESTMARK`), it
**submits the entire encode protocol to virtio-gpu**. On the host, by contrast, no
video entry point has ever executed in any run measured here, and the earlier
dispatch-site sink printed nothing. Whatever the truth about my host-side sinks, the
guest-side evidence now pins the loss to the gap between the guest's virtio-gpu
submit and vrend's video context - the layer cang owns (libkrun's virtio-gpu device /
rutabaga / the render server's context setup), not virglrenderer's or mesa's
parameter handling.

That is also the first point in this ticket where the defect may be **cang's own**
rather than an upstream one, so the next measurements are about that layer:

1. a marker at the very top of the chain - `virgl_renderer_submit_cmd` (or cang's
   equivalent entry for a virtio-gpu submit) - to see whether the commands arrive at
   the renderer at all, before any vrend code;
2. make the render server's own output visible (cang's per-task helper logs, or a
   cang log level that forwards the child's stderr), because a CCMD that reaches
   vrend but has no handler produces `failed to dispatch %s: -22` from
   `vrend_decode_block`, which is exactly the message that would settle this in one
   run without any patch.

## Why the host-side markers were silent, and which ones to trust

A `VIREND_DEBUG`/`VIRGL_LOG_FILE` run (unpatched cang, `VREND_DEBUG=cmd`, the log
pointed at both the shared workspace and `/dev/shm`) produced no log file at all,
which fits the rest of the picture: every host-side marker placed in **cang's render
server** has been silent, while the one host-side marker that ever wrote a file
(`/dev/shm/chroma-debug.log`) came from the **VM worker** (uid 165536, whose
`/dev/shm` is the host's - the per-row dump). The render server's own `/dev/shm` is
not the host's, so a marker there can vanish without the code being skipped.

The measurements that therefore still stand:

- the **guest** flushes the whole video command set (53/55/56/57/60/61) to
  virtio-gpu, on one virgl context;
- the **VM worker** (the process whose `/dev/shm` is the host's, and where a sink is
  known to work) dispatched no video CCMD in the dispatch-site run, and no encode
  entry point in the brace-counted run - while the guest's encoder completed.

So the loss is between the guest's virtio-gpu submit and vrend's video context, i.e.
in the transport cang owns (libkrun's virtio-gpu device / rutabaga / the context and
capability setup), and the next marker belongs at that layer's entry -
`virgl_renderer_submit_cmd`, which runs in the worker where the sink works - to see
whether the command reaches the renderer at all before any vrend code.

Also worth checking there, cheaply: whether the *decode* path's CCMDs do arrive (the
decoder demonstrably works), which would show the transport carrying video commands
in general and the encode ones being dropped specifically.

## The encode lands on a context that is not vrend's

Two markers at the renderer's own layers, each verified in the built library, settle
the previous section's question and reverse its conclusion.

**The commands do arrive.** A marker at the top of `virgl_renderer_submit_cmd`
(`src/virglrenderer.c`, the renderer's front door, running in the VM worker where the
sink is known to work) logs every submit that contains a video CCMD:

```
RECV ctx=1 ndw=1056 video=2 ids=53 55      <- the guest's encode
RECV ctx=1 ndw=1044 video=4 ids=56 57 60 61
RECV ctx=1 ndw=1038 video=3 ids=57 60 61
RECV ctx=1 ndw=1038 video=3 ids=57 60 61
RECV ctx=1 ndw=1074 video=5 ids=54 56 56 56 56
RECV ctx=2 ndw=1039 video=2 ids=53 57       <- the VA decode
RECV ctx=2 ndw=1035 video=2 ids=59 61
```

So the transport is *not* losing anything, and the guest's `VIRGL_CCMD_ENCODE_BITSTREAM`
(60) really does reach the renderer.

**But it never reaches vrend.** A marker at the top of
`vrend_decode_ctx_submit_cmd` (`src/vrend/vrend_decode.c:2049`, the submit callback
vrend installs on its own contexts, `ctx->submit_cmd` at 2152) logs only the *decode*
context:

```
SUB ctx=2 ndw=1139 video=1 ids=55
SUB ctx=2 ndw=1038 video=2 ids=53 57
SUB ctx=2 ndw=1035 video=2 ids=59 61
SUB ctx=2 ndw=1055 video=1 ids=55
SUB ctx=2 ndw=1029 video=1 ids=57
SUB ctx=2 ndw=1035 video=2 ids=59 61
   ... and 247 more, all ctx=2
```

Every encode submit in the same run is `ctx=1`, and **no `ctx=1` buffer ever reaches
vrend's decode context**. Combined with the guest-side evidence (the guest's marker is
in mesa's *virgl* VA driver, so the guest is submitting virgl CCMDs) the reading is
that the guest's encode traffic is sent on a host context that is **not backed by
vrend** - `--gpu=drm` gives the guest more than one context kind (vrend for GL/virgl,
venus for Vulkan), and the context the VA encoder used is the non-vrend one, whose
submit drops or ignores the video CCMDs. The decode path, on ctx 2, is vrend's and
works.

That makes the defect **cang's context wiring**, not virglrenderer's parameters and
not the transport: the next step is to read cang's context-creation policy (which
guest context becomes a vrend context versus a venus one, and what the guest's
encoder requests) and to check the guest side for which screen/context mesa's VA
driver picks for an encode versus a decode.
