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

## Which context kind is it? The design says, the marker could not

cang's own GPU setup documents the intended split
(`crates/cang/src/runtime/vm/libkrun/launcher.rs:32-51`): `VIRGLRENDERER_VENUS_FLAGS`
= `USE_EGL | THREAD_SYNC | VENUS | RENDER_SERVER | DRM | USE_VIDEO`, with the comment
that *the venus renderer runs in the sandboxed render server (RENDER_SERVER is
respected by venus but ignored by virgl), while vrend runs in-process for GL and VA-API
video (USE_EGL + USE_VIDEO with a get_drm_fd callback)*. So both context kinds exist in
one renderer, and the guest's per-context capset decides which handler a context gets -
which is exactly what determines whether its video CCMDs are handled.

To read that capset, a marker was added to rutabaga's context creation
(`deps/rutabaga_gfx/src/virgl_renderer.rs::create_context`, logging `ctx_id`,
`context_init` and `context_init & RUTABAGA_CONTEXT_INIT_CAPSET_ID_MASK`), the cang
worktree was built with it and a full encode+decode guest run was made: **no log file
appeared**. That is itself the answer to why: the rutabaga instance that serves a cang
VM is **libkrun's**, and `deps/libkrun` is grafted from the flake input by
`nix/pkgs/workspace-src.nix`, so an edit to the worktree's `deps/rutabaga_gfx` - or to
`deps/libkrun` itself - is not what gets compiled. The marker must go into the libkrun
fork (or the local-source build path) instead.

With that, the chain for this ticket is: guest -> submits the whole video protocol;
renderer front door -> receives it on ctx=1; vrend -> handles only ctx=2. What is still
unproven is *which capset* ctx=1 was created with - venus (the external render server)
or virgl (vrend) - and that one datum decides whether the fix belongs in cang's
context/capset wiring or in the libkrun fork's context creation.

## Both contexts are vrend - the "wrong context kind" hypothesis is dead

A marker inside virglrenderer's own `virgl_renderer_context_create_with_flags`
(`src/virglrenderer.c`, logging `ctx_id`, the flags and
`ctx_flags & VIRGL_RENDERER_CONTEXT_FLAG_CAPSET_ID_MASK`; verified present in the
built library, unlike the earlier attempt in rutabaga) gives the capsets for a full
encode-then-decode guest run:

```
CTX id=1 flags=0x2 capset=2 name=ffmpeg     <- the encode
CTX id=1 flags=0x2 capset=2 name=ffmpeg
CTX id=2 flags=0x2 capset=2 name=ffmpeg     <- the decode
```

`capset=2` is `VIRTGPU_DRM_CAPSET_VIRGL2`, which `virgl_renderer_context_create_with_flags`
routes to `vrend_renderer_context_create` - so **both the encode's context and the
decode's are vrend contexts**, and the previous section's reading ("the encode lands on
a context that is not vrend's") is wrong. The asymmetry has to be explained elsewhere:

- the earlier dispatch-site marker was placed by a text anchor whose patch was never
  confirmed in the built library, and the brace-counted markers in
  `vrend_video_encode_bitstream` never fired even though the decode's sibling
  `vrend_decode_decode_bitstream` demonstrably runs - so the video *dispatch* is where
  the next verified marker belongs, not the context creation;
- in the run whose submits were logged, every video-bearing buffer was `ctx=2`
  including the encode-flavoured ones (`53 57` = CREATE_VIDEO_CODEC + BEGIN_FRAME,
  `55` = CREATE_VIDEO_BUFFER) - i.e. the guest's two processes did not consistently
  land on ctx 1 and ctx 2 in the order assumed, which is another reason to stop
  reasoning from the ids and instrument the dispatch.

Next: a dispatch marker with verified placement and a confirmed patch (log the CCMD
name, its length and the handler's return value for ids 53..61 in
`vrend_decode_block`), which says in one run whether the encode CCMD reaches
`vrend_decode_encode_bitstream` and what it returns.

## Two invalidated measurements, and the one that now stands

**A stale-output trap invalidated two runs.** The probes reused fixed output paths
(`/workspace/enc35.h264`) and their runner's `rm -f` pattern did not match the file, so
ffmpeg exited immediately with `File '/workspace/enc35.h264' already exists. Exiting.`
and the console's `size=1019` was the *stale* file. The capset reading and the
dispatch reading below were both first taken in such a run, where **no encode happened
at all** - only the decode step, which is exactly what those logs showed. Probes now
pass `-y` and the runner removes the exact output path.

**With the encode verified to run** (`-y`, fresh 1 KiB file, guest log reporting three
frames, exit 0):

```
CTX id=1 flags=0x2 capset=2 name=ffmpeg      <- vrend (VIRGL2)
CTX id=2 flags=0x2 capset=2 name=ffmpeg      <- vrend
DISP ctx=1 cmd=44 END_TRANSFERS len=1023 ret=0   (first for ctx)
DISP ctx=2 cmd=44 END_TRANSFERS len=1023 ret=0   (first for ctx)
DISP ctx=2 cmd=55 CREATE_VIDEO_BUFFER len=7 ret=0
DISP ctx=2 cmd=53 CREATE_VIDEO_CODEC len=8 ret=0
DISP ctx=2 cmd=57 BEGIN_FRAME len=2 ret=0
DISP ctx=2 cmd=59 DECODE_BITSTREAM len=5 ret=0   <- the decode
DISP ctx=2 cmd=61 END_FRAME len=2 ret=0
   ... and the same 55/57/59/61 cycle, all ctx=2, ~40 times
```

So: every context is `VIRGL2` -> vrend (the earlier "not vrend's context" reading is
dead), the decode's video CCMDs dispatch normally on ctx=2, and **no `60
ENCODE_BITSTREAM` is dispatched on any context** while the encode process runs and
writes output. The encode's context (ctx=1) dispatches only `END_TRANSFERS`.

That also settles the front-door question: the earlier `RECV` lines that showed video
ids on ctx=1 were **false positives** - the scanner walked a 1024-dword transfer
buffer whose payload happens to contain bytes in 53..61 (the same parse on ctx=2 is
genuine because those buffers really are command streams). The encode's video CCMDs
therefore never arrive at the renderer at all, which puts the loss in the guest's
submit or in the host's virtio-gpu device, *before* virglrenderer.

**The guest submit path to instrument next**: not `src/virtio/vdrm/vdrm_virtgpu.c`
(a marker there never fired, for the decode either) but
`src/gallium/winsys/virgl/drm/virgl_drm_winsys.c:954`
`virgl_drm_winsys_submit_cmd` -> `drmIoctl(DRM_IOCTL_VIRTGPU_EXECBUFFER)` at :985 -
the path mesa's virgl winsys actually uses. Marking it (the buffer's video CCMD ids
and the ioctl result, to stderr) decides whether the encode's command buffer is
submitted and accepted in the guest, or dropped there.

Incidental but worth knowing: a guest driver built without ticket 02's guest-side
patch (a plain nixpkgs mesa plus only a diagnostic patch) writes 830 bytes for the same
three-frame solid-red encode where the image's patched driver writes 1019 - i.e. two
different, both non-empty, minimal streams; the sizes are not a useful signal on their
own.

## Correction: the encode does reach vrend and dispatches cleanly

The previous two sections are wrong, and the cause is methodological: **each
diagnostic build carried one marker, and several of those runs used the stale-output
probe** (no `-y`, a fixed path that already existed), so "no marker fired" meant "no
encode happened". With *both* host markers in one build and the encode verified to run:

- the **front door** (`virgl_renderer_submit_cmd`) receives the encode's buffers on
  ctx=1, with `ndw` values **identical to what the guest's winsys submitted**
  (`1131/1047/1056/1044/1038/1038/1074` carrying `55`, `53 55`, `56 57 60 61`,
  `57 60 61` x2, `54 56 56 56 56`) - so the RECV parse was genuine after all, and the
  guest-to-renderer transport loses nothing;
- the **dispatcher** (`vrend_decode_block`) shows
  `DEP: ctx=1 cmd=60 ENCODE_BITSTREAM len=5 ret=0` **nine times**, alongside
  `CREATE_VIDEO_CODEC`/`CREATE_VIDEO_BUFFER`/`BEGIN_FRAME`/`END_FRAME` and, for the
  decode, `DECODE_BITSTREAM` - **all `ret=0`**, on vrend contexts created with
  `capset=2` (`VIRGL2`).

And on the guest side, a marker in the path mesa's virgl winsys actually uses
(`src/gallium/winsys/virgl/drm/virgl_drm_winsys.c:954 virgl_drm_winsys_submit_cmd`,
`drmIoctl(DRM_IOCTL_VIRTGPU_EXECBUFFER)` at :985 - *not* `vdrm_virtgpu.c`, whose marker
never fired even for the decode) shows the whole encode protocol being submitted with
`valid=1`: `ids=55`, `53 55`, `56 57 60 61`, `57 60 61` x2, `54 56 56 56 56` for three
frames, and 516 video-bearing submits for the decode control.

So the chain is intact end to end - guest VA driver -> virgl winsys -> kernel ->
host virtio-gpu -> renderer front door -> vrend dispatch -> the VA encode - and the
chroma/bitrate defect is where this ticket started: **inside what vrend tells the VA
encoder**. The next build puts the parameter markers (`h264_fill_enc_picture_param`,
`h264_encode_bitstream`) and the dispatch marker in one build, runs the *verified*
probe (`-y`, fresh output), and reads the fields to diff against the native trace.

Method notes for whoever continues: one marker per build makes a silent marker
uninterpretable; every probe must overwrite its output (`-y`) and the console must
report the size *and* that the file is fresh; and a marker's absence only counts after
the same build is shown to be the one running (check the string in the loaded library,
not just in the patch file).

## Where the fixture work stands, and two build traps that wasted runs

The submission chain is now measured end to end and intact: the guest's VA driver is
mesa's virgl one (marker in `virgl_video.c` fired), the guest's winsys submits the whole
video protocol (`GUESTSUBMIT2`, `valid=1`, with the same `ndw` values the host's front
door receives), and vrend's dispatcher runs `cmd=60 ENCODE_BITSTREAM len=5 ret=0` nine
times for a three-frame encode. So the defect is once again **inside the encode's
parameters/surface handling**, and the fixture that would name it is the parameter dump
in `h264_fill_enc_picture_param` / `virgl_video_encode_bitstream`.

Two traps kept that fixture from ever being read, both about *proving the marker is in
the running build* rather than in the patch file:

- **Unrooted diagnostic builds vanish.** `nix build … --no-link` leaves no GC root, so
  the diagnostic cang and its `virglrenderer` can be collected between a run and the
  check - which is why several `grep` verifications came back empty and why
  `nix path-info -r` on the build now returns nothing for virgl. Diagnostic builds
  should be rooted (`-o <state-root>/diag-cang`) and the check done on the rooted
  output.
- **`nix-store -qR` is not available in these shells** (only `nix` is), so the closure
  must be read with `nix path-info -r`, and the marker string checked in
  `<virglrenderer>/lib/libvirglrenderer.so.1` - the *loaded* library. When that check
  was finally made properly it showed that the two virglrenderer paths carrying the
  dispatch marker are both from *old* builds, i.e. the marker this ticket has been
  reasoning from was present in the library that dispatched, but the newer
  parameter/call-path markers were never confirmed in a library at all.

With those two fixed, the sequence to finish the diagnosis is short: build once with
the dispatch marker plus markers on `vrend_video_encode_bitstream`'s entry, each of its
early returns (`!cdc || !src` is the only one with no `virgl_error`) and
`virgl_video_encode_bitstream`'s entry; root the build; verify every marker string in
the rooted `libvirglrenderer.so.1`; run the `-y` probe; then read the fields and diff
them against the native trace.

## Verified fixture: parameters are sane and the encoder's input is clean

With the diagnostic build **rooted** and every marker string confirmed in the loaded
`libvirglrenderer.so.1` (`vrpmx…` / `k9l76g…`), and the encode verified to run
(`-y`, fresh 1 KiB output, three frames), the two tables are finally readable.

Parameters for the three-frame solid-red encode (320x240, CQP 26, `-bf 0`), one line
per frame:

```
PARPIC src_sfc=1 curr=2 qp=26 qp_chroma=0 qp_chroma2=0 l0=0 l1=0 ndesc=1 fnum=0
       qi=26 qp_p=0 ptype=3 notref=0 idr=1 ref=1 cabac=1 x8=0 db=0
       codec w=320 h=240 chroma=1 level=0 maxref=16 prof=11
PARPIC src_sfc=6 curr=9  … fnum=1 qi=26 qp_p=26 ptype=0 idr=0 ref=1
PARPIC src_sfc=7 curr=10 … fnum=2 qi=26 qp_p=26 ptype=0 idr=0 ref=1
```

So: QP 26 as requested, chroma QP offsets 0 (as native), IDR then P with correct
`reference_pic_flag`, CABAC on, 4:2:0, full 320x240, `num_slice_descriptors = 1`. The
only deltas from the native control are `transform_8x8_mode_flag = 0` (native 1),
`deblocking_filter_control_present_flag = 0` (native 1), `level_idc` 51 because
`codec->level` is 0 (native 30) and `max_references = 16` (native 2) - all of which
affect *compression*, none of which can zero a chroma sample.

`src_sfc != curr` is not a defect: vrend uploads the guest's surface into its own
reference-ring surface and encodes that, which is a legitimate design.

The upload's per-plane view, for the same run (per-row dump through GL):

```
plane 0: 320x240 pitch 512 off 0      fmt 0x20203852 (R8)   res 320x240
  row 0/30/60/90/120/150/180/210: src=51515151 zeros 0/320 | dst=51515151 zeros 0/320
plane 1: 320x240 pitch 512 off 131072 fmt 0x38385247 (GR88) res 160x120
  row 0/15/30/45/60/75/90/105:    src=5af05af0 zeros 0/320 | dst=5af05af0 zeros 0/320
```

(`51` = Y 81, `5a f0` = U 90 / V 240, BT.601 red.) So the encoder's destination
surface holds exactly the right luma **and** the right chroma, with no zeros, on every
sampled row of both planes.

**What that eliminates and what it leaves.** The parameters are sane and the surface
the encoder reads is clean *as seen through GL* - while the decoded stream's chroma is
~28 dB worse with ~26% zero samples scattered over the plane. The two views are both
GL read-backs, so a disagreement between **GL's view of the buffer and the DMA view the
VA encoder reads** would be invisible to this dump, and that is now the working
hypothesis (a plane pitch/offset/layout disagreement, e.g. chroma read with the luma
plane's 512-byte pitch or the wrong plane offset: `plane 1` shows `dl 320x240` while its
resource is `160x120`, the same shape mismatch the per-plane-format attempt found
harmless for the *format*).

The decisive experiment is spatial: encode a frame whose **chroma varies across the
picture** (left half U=90/V=240, right half a different pair, built with a filter), then
decode the guest's stream and look at *where* each chroma value lands. A pitch or offset
disagreement puts them at the wrong columns/rows or duplicates them, which names the
line; if instead the chroma is uniform noise, the fault is inside the encode's chroma
prediction and the search returns to the driver.

## Not a size limit: the chroma is destroyed at every size, luma is bit-identical

The obvious "is the picture too small for the AMD encoder" question is answered by a
size sweep with the *same* command and content on both sides (10 frames of `testsrc`,
CQP 26, `-bf 0`, `-lavfi psnr` of the encoded stream against the source):

| size | guest PSNR y / u / v | host control y / u / v | guest bytes | host bytes |
| --- | --- | --- | --- | --- |
| 176x144 | 44.442958 / **5.718897** / **5.759051** | 44.442958 / 42.445101 / 41.429851 | 4 934 | 3 792 |
| 320x240 | 44.091405 / **6.486577** / **6.612471** | 44.091405 / 45.218977 / 45.499747 | 19 087 | 6 019 |
| 640x480 | 49.843129 / **6.397063** / **6.567003** | 49.843129 / 47.260841 / 47.132959 | 39 954 | 8 473 |
| 1280x720 | 51.187132 / **7.324610** / **7.175346** | 51.187132 / 48.305424 / 48.085073 | 79 551 | 13 807 |
| 1920x1080 | 50.696566 / **7.647622** / **7.649542** | 50.696566 / 49.491349 / 48.406070 | 178 951 | 20 216 |

Three things fall out:

- **luma is identical to six decimals at every size** - the guest's encoder produces
  exactly the host's luma, so the encode itself, the parameters vrend fills and the
  surface's luma are all correct;
- **chroma is ~35-40 dB worse at every size**, from 176x144 to 1920x1080, so it is not
  a minimum-size or alignment limit of the AMD encoder;
- the guest's bitrate is **3x (176x144) to 9x (1920x1080) the host's** - the bytes are
  being spent on the chroma damage.

A frame-level look at the chroma plane of the guest's 640x480 stream against the
source shows the same thing structurally: the guest's U/V is mostly the source's value
(128 for the grey pattern) with the pattern's colour boundary in the wrong place, and a
horizontal-shift search over +-24 chroma samples finds no shift that aligns them
(mean |delta| stays ~100 at every offset) - so the chroma is not displaced (a plain
offset error) but replaced by something else over a large part of the picture.

With the earlier sections (params sane, encoder surface clean *as seen through GL*,
submission chain intact), the loss is now pinned to the step between the GL-written
surface and the encoder's own read of it. The instrument for that is a libva buffer
diff of the worker's VA submission against a native encode's: a `vaMapBuffer`
interposer works (`CODED` segments are logged for both sides), but `vaCreateBuffer` is
not being interposed in either process (no `BUF` lines), so the parameter buffers'
payloads are still unread - `LD_DEBUG=bindings` on the native control is the one
command that says why (a bindings report shows which definition `vaCreateBuffer`
resolves to) and is the next step.

## The VA buffer diff: everything matches except a missing rate-control buffer

A libva interposer that logs every `vaCreateBuffer` payload (the earlier shim missed
them because it checked the *decode-era* type numbers - the encode types are 22
sequence, 23 picture, 24 slice, 25/26 packed header, 27 misc, from libva's `va.h`) was
run on both sides for the same content and command (testsrc 320x240, 3 frames, CQP 26,
`-bf 0`): `LD_PRELOAD` for the native control, and as a `DT_NEEDED` of a `patchelf`'d
cang for the worker (whose `/dev/shm` is the host's, so its log could be read).

Buffer types and sizes are identical on both sides - `22:1132`, `23:648`, `25:28`,
`26:{29|37, 8, 150|162, 8|9}`, `24:3140` - **except that the working native client
creates one `type=27` (`VAEncMiscParameterBuffer`, 28 bytes, rate control) and the
worker creates none at all**; that is the only structural difference. The payload diffs
in the common buffers are surface-id/POC-shaped and benign (`ReferenceFrames[i]`
picture ids, `frame_idx`, `TopFieldOrderCnt`, and `VA_PICTURE_H264_INVALID` vs 0 in
`RefPicList0[0].flags`), plus a handful of QP-region bytes in the picture buffer's tail
(offsets 620/629 of 648).

So vrend's VA submission is a faithful, near-identical reconstruction of what the native
client sends - with no rate-control buffer - and the encoder nevertheless produces
correct luma and destroyed chroma. That leaves the *memory* the encoder reads: the
export path (`export_video_dma_buf` -> `vaExportSurfaceHandle` ->
`fill_video_dma_buf`) *does* record each plane's `modifier`
(`desc->objects[i].drm_format_modifier`), but the import path
(`sync_video_buffer_to_dmabuf`) imports each plane with `EGL_DMA_BUF_PLANE0_FD/OFFSET/
PITCH_EXT` only and **never passes a modifier**, so GL blits into a linear view of a
tiled buffer while the VA engine reads it with the modifier - a "GL sees it right, the
engine sees something else" split, which is exactly what every dump in this ticket has
shown (input surface correct through GL, chroma destroyed in the encoded stream).

Next: print the modifiers and the EGL attribute list on both sides of that path (one
debug build, no VM change), then either pass the modifier on import
(`EGL_DMA_BUF_PLANE*_MODIFIER_{LO,HI}_EXT`) or make both sides linear, and re-run the
size sweep expecting the guest's chroma PSNR to reach the host's.

Also worth keeping in view, since it is the same "the engine's view of memory differs"
family: the missing `VAEncMiscParameterBuffer` may matter for chroma even if luma looks
right (the driver would fall back to its defaults for the rate-control/chroma-QP
derivation), and the `sequence` buffer's first bytes differ from the native client's at
offsets 1, 20 and 29 (`guest 33/10/49` vs `host 0d/01/09`) - those are inside the SPS
payload region and could be the level/VUI/cropping the guest's client chose, which the
encoder's chroma handling could in principle key off.

## The guest-dmabuf import carries a tiled modifier, and passing it changes nothing

A debug build logged the import of every guest dma-buf plane with its modifier and
passed it through (`EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT`, `nattrs=9`):

```
IMPORT plane=0 fmt=0x20203852 modifier=0x18601b04 pitch=256 off=0     wh=176x144 nattrs=9 src=tiled
IMPORT plane=1 fmt=0x38385247 modifier=0x18601b04 pitch=512 off=65536 wh=176x144 nattrs=9 src=tiled
IMPORT plane=0 fmt=0x20203852 modifier=0x18601b04 pitch=512 off=0      wh=320x240 nattrs=9 src=tiled
IMPORT plane=1 fmt=0x38385247 modifier=0x18601b04 pitch=512 off=131072 wh=320x240 nattrs=9 src=tiled
```

So the buffers really are tiled (`0x18601b04`), the plane offsets are 64 KiB-aligned
rather than `pitch * height` (65536 for 176x144, 131072 for 320x240), and the modifier
*is* now handed to the EGL import - and the size sweep with that build returns
**bit-identical results** to before it (same PSNR to six decimals, same file sizes:
4934 / 19087 / 39954 / 79551 / 178951). The guest-dmabuf import is therefore not the
place where the chroma is lost.

That sharpens the remaining hole: the *other* EGL import on this path is vrend's own
**VA-surface** import (`vaExportSurfaceHandle` -> `export_video_dma_buf` ->
`fill_video_dma_buf` in `virgl_video.c`, consumed when the video buffer's plane
textures are created), where the exported surface's modifier is recorded but the
importing side must honour it the same way before GL writes through it - which is what
all the GL-visible dumps in this ticket have measured as "correct", because both the
dump and the writer go through the same (possibly linear) view.

Next, in order: (1) log the modifier on the VA-surface export/import pair in
`virgl_video.c` and pass it through there; (2) the still-untested missing
`VAEncMiscParameterBuffer`, since it is the one structural difference in the VA
submission and none of the memory-layout candidates has moved the needle.

## What the damage actually looks like: real chroma, spatially scrambled

Decoding the guest's and the host's 320x240 encode of the same source and comparing
the chroma planes frame by frame gives a much more specific picture than "chroma PSNR
is low":

```
source  U row0[:16] = 128 128 128 128 128 128 128 128 128 128 128 128 128 128 128 128
host    U row0[:16] = 128 128 128 128 128 128 128 128 128 128 128 128 128 128 128 128
guest   U row0[:16] = 129 129 129 130 128 127  89  87 240 240 240 240  16  16  16  16
```

The guest's chroma holds **real chroma values** - grey (128-130), then a saturated bar
pair (89/87), then 240s, then 16s - i.e. it is a colour pattern with sharp transitions,
but the transitions sit at the wrong places and at the wrong spacing (a few samples
apart where the source's bars are far apart). It is not noise and not a constant, and
`testsrc`'s bars are exactly where a spatial rearrangement would show up.

Cross-checks rule out the simple explanations:

| comparison (frame 2, chroma plane) | mean abs delta |
| --- | --- |
| guest vs source chroma | 100.3 |
| guest vs host chroma (same encoder, same command) | 100.1 |
| guest vs source luma (2x2 sampled, best of 4 alignments) | 96.0 |
| guest vs source chroma of frames 0..4 (any other frame) | 100.0-100.2 |
| guest vs guest or source luma of any frame | 97.0 |
| host vs source chroma | 0.74 |

So the guest's chroma is not the source's chroma, not the host's, not the luma, and not
another frame's - it is related to the picture only in that it contains plausible chroma
values. Together with everything else measured in this ticket (inputs correct through
GL, parameters near-identical, submission near-identical, modifiers now passed with no
effect at all), the remaining mechanism is a **stride/plane-geometry disagreement inside
the driver's own view of the surface** - the plane the encoder walks row by row is not
the plane GL wrote, in a way that compresses/shifts the pattern rather than zeroing it.

Next instrument, and it targets exactly that: log the VA surface vrend creates for the
encoder (`vaCreateSurfaces` attributes: pixel format, memory type, and any
`VASurfaceAttribExternalBuffers` strides/offsets/pitches, plus the resulting surface's
own attributes) and compare them with what a native client's surface has. If vrend hands
the driver the *guest's* strides (512-byte pitch, 64 KiB-aligned plane offsets) while
the driver's chroma walk assumes its own, that is the line to fix - and it also explains
why every GL-visible dump has looked correct.

Also still open from the VA buffer diff, both cheap and both quality-only: vrend leaves
`deblocking_filter_control_present_flag` and `transform_8x8_mode_flag` at 0 where the
native client sets both (the byte-level picture-buffer diff shows the deblocking bit
differing at offset 629), and it submits no `VAEncMiscParameterBuffer` at all.

## The direction of the encode upload needs checking

Reading `vrend_video.c` and `virgl_video.c` together turns up a structural question that
fits the symptom better than anything else so far.

- `virgl_video_create_buffer` creates the encoder's surface as a **driver-allocated VA
  surface** (`vaCreateSurfaces(va_dpy, format, w, h, &sfc, 1, NULL, 0)`), i.e. its layout
  is the driver's own and no client strides are involved.
- `vrend_video_create_buffer` then gives that buffer one GL texture/framebuffer per
  plane, each tied to a **guest resource handle** (`plane->res_handle = res_handles[i]`).
- The encode upload callback (`encode_upload_picture` in `virgl_video.c`) exports *the
  VA surface itself* (`export_video_dma_buf(buffer, VIRGL_VIDEO_DMABUF_WRITE_ONLY)`) and
  hands that dma-buf to `vrend_video_enocde_upload_picture`, which calls
  `sync_video_buffer_to_dmabuf(buf, dmabuf)`.
- `sync_video_buffer_to_dmabuf` imports that image and then blits it into
  `res = vrend_renderer_ctx_res_lookup(plane->res_handle)` - i.e. **into the guest's
  resource**, which for the encode path is the client's own VA surface, not vrend's.

So on this read the encode's "upload" copies *out of* the (still empty) vrend VA surface
*into* the guest's resource, while the encoder later encodes `source->buffer` - vrend's
VA surface. That would leave the encoder reading a surface the picture was never written
into, which is consistent with "the plane the encoder walks is not the plane GL wrote"
but *not* with the luma being bit-identical to the host's, so one of the following is
true and has to be established by instrumenting rather than reading:

1. the two functions are named from the guest's perspective and I have the direction
   backwards - then the write does land in vrend's surface and the chroma loss is
   inside the driver's read of it;
2. the guest's VA surface and vrend's VA surface are the *same* memory in this
   configuration (e.g. the guest's resource is imported as the surface's backing), in
   which case the blit is a no-op-ish copy and the chroma damage is elsewhere again;
3. the blit really is the wrong way round and the picture reaches the encoder through
   some other path (the luma evidence suggests something writes the surface correctly).

Next instrument, one build: log the handles and GL ids on both sides of the encode
upload - `plane->res_handle`, `res->gl_id`, `plane->texture`, and the surface id
(`buf->buffer->va_sfc`) - plus, right before `vaBeginPicture`, read back one row of the
surface's luma and chroma through the plane textures and print them. That answers both
"who wrote where" and "what does the encoder's surface hold at encode time" in one run,
which is the measurement this ticket has been missing.

## Two layout fixes tested and refuted: the modifiers, and the plane offsets

**Modifiers (tested).** A build that passes each plane's modifier to the EGL import
(`EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT`) produced **bit-identical** output - same PSNR
to six decimals, same file sizes - so the import's missing modifier is not the defect,
even though the buffers really are tiled (`0x18601b04`).

**Plane offsets (tested, and worse).** Logging the export against what the driver
reports through `vaDeriveImage` for the same surface shows a real disagreement:

```
OFFS plane=1 export=131072 derived=122880 pitch=512/512 sfc=1     (320x240)
OFFS plane=1 export=65536  derived=36864  pitch=512/256 sfc=1     (176x144)
```

- and aligning the two by rewriting the exported plane offsets with the derived ones
**breaks the encode outright**: luma and chroma PSNR both fall to ~4.9 dB at every size
(against 44-51 dB luma / 6-8 dB chroma before), with much smaller files. So the
exported DMA-BUF geometry *is* the geometry GL and the encoder agree on, and the
`vaDeriveImage` view is a different access path rather than the driver's encode layout.
Both layout hypotheses are now dead by measurement, not by argument.

**What that leaves.** Every layer of the encode - transport, context, dispatch,
parameters, submission contents, surface contents through GL and through the exported
DMA-BUF, size, modifiers, offsets - has now been measured, and the chroma is still
destroyed while the luma is bit-identical to a native encode. The one viewpoint still
unmeasured is the **output side of the VA encoder as vrend consumes it**: the coded
buffer's segments are copied into the guest's resource in
`vrend_video_encode_completed` (`vrend_video.c`) with its own size/stride handling, and
nothing in this ticket has ever compared *those* bytes against what the native client's
`vaMapBuffer` returns for the same content. The ticket-02 evidence checked exactly one
frame of one encode (3-frame run, frame 0 identical) - which is too small a sample to
rule out a per-frame or per-plane defect there, and the chroma damage is exactly the
kind of thing a partially-copied coded buffer produces.

Next: dump the coded buffer the driver produced and the bytes vrend hands to the guest
for the same frame (both sides of `vrend_video_encode_completed`), for 5-10 frames of
real content, and compare them by NAL. If they match, the encoder's own output is
already chroma-damaged and the search returns to the driver with a much sharper
reproduction (one guest encode, one host encode, same command, differing only in chroma).

## The stream metadata matches, so the decoder is not misreading the chroma

`ffprobe` on the guest's and the host's 320x240 streams reports identical stream
metadata - `High` profile, level 13, `yuv420p`, `tv` range, `chroma_location=left` - so
the SPS/PPS are not telling a decoder to treat the chroma differently, and the damage is
inside the slice data rather than in the parameter sets. That eliminates the
"stream says something different about chroma" family, which was the last cheap
explanation left after the layout experiments.

Two discriminators remain, both host-side and both cheap, and they separate
"vrend's parameter set drives the damage" from "the guest path drives it":

1. **Re-drive the native encoder with vrend's parameters** - the same content on the
   host, but with `transform_8x8_mode_flag` and `deblocking_filter_control_present_flag`
   cleared and `max_references`/level set to vrend's values (a small libva client, or
   ffmpeg's `-flags +ildct`-style knobs where they exist). If the host then produces
   scrambled chroma too, the defect is in what vrend fills and the fix is local to
   `virgl_video.c`'s parameter filling; if the host stays clean, the guest path is at
   fault and the coded-buffer handover is the next place to look.
2. **Compare the coded-buffer handover frame by frame** - `vrend_video_encode_completed`
   copies the driver's `coded_bufs`/`coded_sizes` into the guest's resource, and ticket
   02 checked exactly one frame of one encode. Dumping both sides for 5-10 frames of real
   content turns "the encoder's output was already damaged" into a measurement.

## A synthetic checkerboard: the guest's chroma is structured, but not a transform of the source's

A source with a known chroma pattern (16x16 luma-block checkerboard, U = 200/60, luma
constant 16, three frames, ffv1) was encoded in the guest and decoded; the U value per
16x16 luma block reads:

```
SOURCE                             GUEST
..##..##..##..##..##               ################....
..##..##..##..##..##               ################....
##..##..##..##..##..               ................####
##..##..##..##..##..               ................####
..##..##..##..##..##               ################....
..##..##..##..##..##               ################....
##..##..##..##..##..               ................####
##..##..##..##..##..               ................####
```

Two things are clear. The guest's chroma is **not noise and not a constant** - it is a
regular two-level pattern with the *same values* the source used, so the encoder did read
*some* chroma image - and its **vertical period is the source's** (four block rows before
the inversion) while its **horizontal structure is collapsed**: sixteen "high" blocks
followed by four "low", i.e. a period of 20 blocks = the full 160-sample chroma row,
where the source alternates every two blocks.

Fitting simple transforms - guest(x, y) = source(x / k, y + dy) for k in {1,2,4,8,16,32}
and dy in 0..3, plus horizontal shifts - finds nothing: the best residual stays ~86 (an
unrelated image is ~100). So the guest's chroma is neither the source's chroma nor the
source's luma (that is uniform here, and the guest's is not) nor a scaled/shifted copy of
either.

That is a much sharper target than "chroma PSNR is low": a *structured* chroma image whose
vertical period matches and whose horizontal structure is stretched to the width of the
plane. The next step is to model it properly rather than guess: dump the plane the
encoder actually reads (the surface's chroma as the *driver* maps it, through a
`vaDeriveImage`/`vaMapBuffer` copy taken after `vaSyncSurface`, which is a
driver-visible read rather than a GL read) for this synthetic input, and compare it
against the plane GL wrote. One of the two will show the stretched pattern, and that
identifies the side to fix.
