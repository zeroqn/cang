---
label: wayfinder:research
title: Where the guest VA-API encode loses its bitstream
status: closed
blocked_by: []
claimed_by: bob + pi session (2026-10-01)
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

## Resolution

**Item 1 - the coded-buffer read-back is faithful; the bitstream is already
broken when the *host's* VA client reads it.** Interposing `vaMapBuffer` inside
the VM worker shows cang's vrend receiving exactly one coded-buffer segment per
frame and exactly the bytes the guest's `ffmpeg` writes (3-frame run: 97/17/17
bytes = the guest's 131-byte file, frame 0 byte-identical; the same for HEVC),
so nothing is lost between the driver, cang's vrend and the guest. What arrives
broken is what the host asked radeonsi for: cang's vrend submits no
`VAEncPackedHeader*Buffer` at all (the working host control submits 5 + 5, and the
guest's own `ffmpeg` does submit 4 + 4 to its driver, so the client's parameter
sets die at the virgl boundary - the wire structs have no field for a raw or
parsed header list), and its `VAEncSequenceParameterBufferH264`
is zero in every geometry/level/flag field (`picture_width_in_mbs =
picture_height_in_mbs = 0`, `chroma_format_idc = 0`, `frame_mbs_only_flag = 0`,
`direct_8x8_inference_flag = 0`, `log2_max_frame_num_minus4 = 0`,
`log2_max_pic_order_cnt_lsb_minus4 = 0`, `level_idc = 0`, `intra_period =
ip_period = 0`) while the slice parameter buffer from the same guest is correct
(`num_macroblocks = 920` = the real 40x23 picture). The two describe different
pictures, the driver reports no per-NAL segment table, and the coded buffer ends
up holding one parameter-set-less "NAL" per frame whose first byte after the
start code is `0x00`.

Named path: `virglrenderer-1.3.0/src/vrend/virgl_video.c:1243`
(`h264_fill_enc_seq_param` - the geometry/level/flag assignments are `//`
comments at 1253-1277) and `:1440-1570` (`h264_encode_bitstream` - no
packed-header submission), over wire structs that have no fields for the missing
data (`virglrenderer-1.3.0/src/virgl_video_hw.h:150-171`; mesa's copy
`src/virtio/virtio-gpu/virgl_video_hw.h:150`, filled on the guest side by
`mesa-26.1.8/src/gallium/drivers/virgl/virgl_video.c:195-300`). HEVC's sequence
buffer is not zeroed and its output *is* recognizable HEVC (`26 01` IDR, `02 01`
TRAIL_R), but its VPS/SPS/PPS are missing too - the same packed-header gap, one
codec over (`PPS id out of range: 0` on decode).

Side finding, a second defect: the first ~5 frames are encoded from a surface
that does not hold the frame (10-frame black/white/testsrc encodes share the
sizes 97, 17, 17, 17, 17 and only diverge at frame 6), while the guest's own
`hwupload,hwdownload` round-trips differ from frame 1 - so the guest's surface is
fine and the warm-up is in the host's copy
(`virglrenderer-1.3.0/src/vrend/vrend_video.c:283` `vrend_video_enocde_upload_picture`
-> `:210 sync_video_buffer_to_dmabuf`, which passes `EGL_DMA_BUF_PLANE0_*` for
every plane and never passes the modifier the surface was exported with,
`0x200000018601b04`). It does not explain the header damage (frames 6+ have it
too) but a fixed encoder would still need it.

**Item 2 - yes, the same gap one layer up.** Every encoder-attribute query in the
guest is empty because the guest-side caps function implements decode caps only
and returns 0 for every `PIPE_VIDEO_CAP_ENC_*`
(`mesa-26.1.8/src/gallium/drivers/virgl/virgl_screen.c:139-159`), and the virgl
wire caps carry no encoder-attribute fields, so the host's answers cannot reach
the guest. Entrypoints are exempt (they come from the host's profile table),
which is why `vainfo` advertises `EncSlice` while the guest's VP9 encode fails
cleanly with "No usable encoding entrypoint found" and H.264/HEVC proceed on
guessed defaults.

**Item 3 - not a cang flag, and now carried as an in-tree wire extension.** The
guest already sends everything the wire has (slice descriptors, rate control,
picture type all arrive), and the values that are missing already exist on the
guest side - mesa's VA frontend parses the client's packed SPS/PPS into the pipe
picture desc (`mesa-26.1.8/src/gallium/frontends/va/picture_h264_enc.c:801`); it
is only the wire and the host's VA submission that cannot carry them. The fix is
therefore a wire extension with one patch per side, both in this repo:

- **host** - `nix/pkgs/patches/virglrenderer-encode-raw-headers.patch`: the
  raw-header fields in virglrenderer's copy of the vrend wire structs
  (`src/virgl_video_hw.h`), the geometry/level/chroma/`log2` fields in
  `h264_fill_enc_seq_param` (`src/vrend/virgl_video.c:1243`), and the submission
  of the client's headers as `VAEncPackedHeader*` buffers from
  `h264_encode_bitstream` / `h265_encode_bitstream`. It rides with
  `.#virglrenderer`, like the other host vrend patches, so every cang package
  carries it.
- **guest** - `nix/pkgs/patches/mesa-virgl-encode-raw-headers.patch`: the same
  fields in mesa's copy of the wire struct
  (`src/virtio/virtio-gpu/virgl_video_hw.h`) and the copy out of the pipe picture
  desc (`src/gallium/drivers/virgl/virgl_video.c`), taken at
  `virgl_video_encode_bitstream` time because the VA client only appends its
  packed headers after `vaBeginPicture`. This half is **cang image only**: the
  image's mesa is a prebuilt binary drop that no patch can reach, so the overlay
  builds `pkgs.mesa.override { galliumDrivers = [ "virgl" ]; vulkanDrivers = [ ]; }`
  plus the patch (`mesaVaApi`), `nix/image/layers.nix` exposes it as the
  `cang-va-runtime` driver directory (the driver under the name libva looks for),
  `nix/image/container.nix` links it at `/usr/lib/cang-va-runtime`, and
  guest-init's `MESA_ENV` puts that directory first in `LIBVA_DRIVERS_PATH`. The
  image's GL and Vulkan stay the pinned prebuilt mesa.

Verified 2026-10-01 in a `--gpu=drm` guest (cang 0.11.2 with both patches): a
10-frame `testsrc` 640x360 `h264_vaapi` encode now starts
`00 00 00 01 67 64 0c 1e ...` (SPS), `... 01 68 ee 38 30` (PPS), `... 01 06 05`
(SEI) and software-decodes with exit 0; `hevc_vaapi` starts
`00 00 00 01 40 01 ...` (VPS), `... 42 01` (SPS), PPS and also decodes cleanly;
the guest's VA decode path is unaffected (exit 0). The same run was then repeated
end to end through the shipped configuration - `nix build .#container`'s image
(with its wrapper-contract and Nix-DB checks), `nix build .#cang-musl`'s
guest-init, no test-time `LIBVA_DRIVERS_PATH` override - with the same result.

Still open, in ticket order:

1. The caps gap (item 2) is untouched: the encoder entrypoint is still
   advertised while every `PIPE_VIDEO_CAP_ENC_*` in the guest is 0.
2. The ~5-frame input warm-up: frames 1-5 still encode a surface that does not
   hold the frame (the 10-frame sizes are 287, 18, 18, 18, 18, 14227, ...), so
   the host-side input copy in `vrend_video.c:210/283` still needs its own
   ticket.

Raw measurements, the interposition shim and its `patchelf` recipe, the field
tables and the code references are in
[`notes/02-encode-coded-buffer-evidence.md`](../notes/02-encode-coded-buffer-evidence.md).

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

- `docs/wayfinder/guest-vaapi-video/notes/02-encode-coded-buffer-evidence.md` -
  the measurements behind this resolution (host-side coded-buffer view, libva traces
  of both sides, the caps list, the interposition shim and its injection recipe).
  The "guest's read-back of the coded buffer" wording in
  `docs/vaapi-video-investigation.md` was this ticket's opening hypothesis; item 1
  disproves it and that document has been corrected.
