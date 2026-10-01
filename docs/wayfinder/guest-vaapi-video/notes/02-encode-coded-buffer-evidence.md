# Ticket 02 evidence — where the guest VA-API encode loses its bitstream

Raw measurements behind `tickets/02-encode-coded-buffer-readback.md`. Everything
below was taken on 2026-10-01 on the NAVI33 host, from a live `--gpu=drm` guest
running the packaged `nix build .#cang` (cang 0.11.2, virglrenderer 1.3.0 with
cang's patch set, image mesa 26.1.8, libva 1.23.0, guest kernel
7.2.7-hardened1). The probe scripts and the hermetic podman store live outside
the repo, on the host btrfs disk (`$D/workspace/probe-enc*.sh`, `$D/run-enc*.sh`).

## 1. Method

Three views of the same encode, all with a contrast that can fail:

| view | how |
| --- | --- |
| guest | `ffmpeg -vaapi_device /dev/dri/renderD128 -f lavfi -i testsrc=size=640x360:rate=30 -frames:v N -vf format=nv12,hwupload -c:v h264_vaapi -f h264 /workspace/x.h264` inside the guest, plus the guest's own `LIBVA_TRACE=/workspace/va-guest13.log` |
| host VA client (cang's vrend) | a libva-interposing shim injected into the VM worker: every `vaMapBuffer` of a `VAEncCodedBufferType` buffer logs its segment list (size/bit_offset/status/pointer + first 24 bytes), and the first frame's segment bytes are written to a file |
| host control (native) | the identical `ffmpeg` command on the host against the same render node, i.e. the same mesa 26.1.8 + radeonsi + libva without virglrenderer in the path, with `LIBVA_TRACE`/`LIBVA_TRACE_BUFDATA=1` |

The VM worker writes its libva trace to the shared workspace path; pointing
`LIBVA_TRACE` at `/tmp` or `/dev/shm` produced no file (observed, not explained).
A run that has to stay small uses `--seccomp=off --landlock=off`; the audit mode
(`--seccomp=audit:<path>`) also works and adds the VM worker's strace.

## 2. Host-side view: cang's vrend already holds the broken stream

`CANG_VADUMP=/workspace/vadump17.log CANG_VADUMP_BIN=/workspace/vadump17.bin`,
10-frame `testsrc` H.264 then HEVC in one run (the shim logs one block per
`vaMapBuffer` in the VM worker):

```
CODEDMAP frame=0 buf=5
  seg[0] size=97   bit_offset=0 status=0x00000000 buf=0x6486c3db3000 head=000000010088841bf38362b61f32c0fcba4a6f65f752e377
CODEDMAP frame=1 buf=5
  seg[0] size=17   ... head=00000001009a0237e44000000300001810
CODEDMAP frame=4 buf=5
  seg[0] size=17   ... head=00000001009a0237e44000000300001810
CODEDMAP frame=5 buf=5
  seg[0] size=14226 ... head=00000001009a023790f30db55bfffc7d7ffa453b17c24819
CODEDMAP frame=10 buf=6 (HEVC)
  seg[0] size=94   ... head=000000012601ada0682bfff3d47ffecd64ff83da834848b3
CODEDMAP frame=11 buf=6
  seg[0] size=21   ... head=000000010201d035a0e7a0000003000003000037e0
```

and the 3-frame control run (`vadump14`, `e7.h264`):

```
guest file e7.h264 = 131 bytes, per-frame 97 17 17
vadump14.bin (frame 0 bytes the host's VA client received, 97 bytes) == e7.h264[0:97]   -> FRAME0 IDENTICAL
```

So **one coded-buffer segment per frame, and the guest's file is byte-identical
to what the host's VA client handed over**. Nothing is lost, truncated or
re-ordered in the coded-buffer read-back: the guest's mesa, the virtio-gpu
transfer and cang's vrend copy are all faithful. The same holds for HEVC
(guest `h10.h265` frame 0 = the host's HEVC frame 0).

## 3. What the host sends to radeonsi (libva trace of the VM worker)

`LIBVA_TRACE=/workspace/va-host13.log` in the launcher environment, 4 encodes
(black/testsrc/qp40 H.264 + testsrc HEVC), trace of the encode context 4:

- `vaCreateConfig(profile = 7 VAProfileH264High, entrypoint = 6 VAEntrypointEncSlice, num_attribs = 1, VAConfigAttribRTFormat = 0x101)`
- `vaCreateContext(640x368, flag = 0x1)`, `vaCreateBuffer(VAEncCodedBufferType, size = 471040)`
- per frame: `vaBeginPicture` → `vaRenderPicture(VAEncSequenceParameterBufferType)`,
  `vaRenderPicture(VAEncMiscParameterBufferType)` (rate control, frame rate) →
  `vaRenderPicture(VAEncPictureParameterBufferType)` →
  `vaRenderPicture(VAEncSliceParameterBufferType)` → `vaEndPicture`,
  `vaSyncSurface`, `vaMapBuffer(coded)`.

Buffer types *rendered* per frame, guest path vs host control:

| buffer type | cang's vrend (guest path) | host control (works) |
| --- | --- | --- |
| VAEncSequenceParameterBufferType | 4 | 1 |
| VAEncPictureParameterBufferType | 12 | 3 |
| VAEncSliceParameterBufferType | 12 | 3 |
| VAEncMiscParameterBufferType | 8 | 1 |
| VAEncPackedHeaderParameterBufferType | **0** | 5 |
| VAEncPackedHeaderDataBufferType | **0** | 5 |

So cang's vrend submits no packed headers, and the driver is left to synthesise
the parameter sets from the sequence parameter buffer. The guest's own client
*does* supply them - its libva trace (encode thread) shows
`4 x VAEncPackedHeaderParameterBufferType` + `4 x VAEncPackedHeaderDataBufferType`
per encode next to `3 x Slice`, `3 x Picture`, `1 x Sequence`, `1 x Misc` - but
they die at the virgl boundary: mesa's VA frontend parses them into the pipe
picture desc (`src/gallium/frontends/va/picture_h264_enc.c:801`
`vlVaHandleVAEncPackedHeaderDataBufferTypeH264` fills `desc.h264enc.seq.*`), and
the guest's virgl video driver has no wire field to forward either the parsed
values or the raw header bytes (`virgl_h264_enc_seq_param` / the picture desc in
`src/virtio/virtio-gpu/virgl_video_hw.h`; `grep -n 'raw_header\|packed_header'`
finds nothing in either copy). The information the host needs already exists in
the guest's process, one layer below the virtio-gpu boundary.

### The H.264 sequence parameter buffer is zero

| field | cang's vrend | host control |
| --- | --- | --- |
| level_idc | 0 | 30 |
| intra_period | 0 | 120 |
| intra_idr_period | 120 | 120 |
| ip_period | 0 | 3 |
| max_num_ref_frames | 16 | 2 |
| picture_width_in_mbs | **0** | 40 |
| picture_height_in_mbs | **0** | 23 |
| chroma_format_idc | **0** | 1 |
| frame_mbs_only_flag | **0** | 1 |
| direct_8x8_inference_flag | **0** | 1 |
| log2_max_frame_num_minus4 | **0** | 4 |
| pic_order_cnt_type | 2 | 0 |
| log2_max_pic_order_cnt_lsb_minus4 | **0** | 4 |
| bits_per_second | 0 | 0 |

while the **slice** parameter buffer from the same guest is correct:
`macroblock_address = 0`, `num_macroblocks = 920` (= 40 x 23, the real 640x368
picture), `slice_type = 2` (I) for frame 0 and `0` (P) afterwards. Sequence
parameters and slice parameters therefore describe different pictures, and the
driver's output reflects it: no parameter sets, no per-NAL segment table (so
`radeon_enc_get_feedback()` falls back to `codec_unit_metadata_count = 1` with
`offset = 0`), one "NAL" per frame whose byte after the start code is 0x00.

The same trace for HEVC (`VAEncSequenceParameterBufferHEVC`) is *not* zeroed
out: `general_level_idc = 63`, `pic_width_in_luma_samples = 640`,
`pic_height_in_luma_samples = 368`, `chroma_format_idc = 1`, `intra_period = 120`
- and the guest's HEVC output *is* structurally recognizable HEVC (`00 00 00 01
26 01 ...` = IDR_W_RADL, `00 00 00 01 02 01 ...` = TRAIL_R) but still has no
VPS/SPS/PPS, so software decode fails with `PPS id out of range: 0`.

## 4. Shape of the guest's output

30-frame and 600-frame H.264 runs (`/workspace/e4.h264`, `/workspace/e11.h264`):

- 600 start codes for 600 frames - exactly one per frame, every byte after the
  start code is `0x00`, no SPS/PPS anywhere in 1 864 352 bytes.
- per-frame sizes: `97, 17, 17, 17, 14226, 3511, 2762, 2908, ...` (~3 kB each
  afterwards, so the encoder *does* produce content-sized frames later).
- software decode: `missing picture in access unit with size 1864352`,
  `Unknown NAL code: 0` per frame (host ffmpeg and guest ffmpeg agree).

## 5. The first ~5 frames do not carry the input (separate defect)

3-frame `black` and `testsrc` encodes came out byte-identical (131 bytes both),
which looked like "the input never reaches the encoder". A 10-frame run refutes
that: from frame 6 the input does land.

| frames 1..10 | sizes (bytes) |
| --- | --- |
| black | 97, 17, 17, 17, 17, 1070, 17, 17, 17, 17 |
| white | 97, 17, 17, 17, 17, 1070, 17, 17, 17, 17 (same sizes, different bytes) |
| testsrc | 97, 17, 17, 17, 17, 14226, 3511, 2762, 2816, 3047 |

so frames 1-5 are encoded from a surface that does not yet hold the frame
(constant across inputs), and only later frames reflect it. The guest's *own*
surface is fine from frame 1: `hwupload,hwdownload` round-trips of testsrc,
black, white and testsrc2 give four different md5s (345600 bytes each), i.e. the
guest's VA surface contents are the input. The remaining candidate for the
warm-up is the host-side copy into the VA surface - `vrend_video.c:283
vrend_video_enocde_upload_picture` -> `:210 sync_video_buffer_to_dmabuf`, which
passes `EGL_DMA_BUF_PLANE0_*` attributes for every plane and never passes a
modifier, while the surface it imports was exported with a tiled modifier
(`vaExportSurfaceHandle ... modifier = 0x200000018601b04`, trace).

This is *not* why the stream is unreadable (the header damage is present in
frames 6+ too), but a fixed encoder would still need it.

## 6. Encode caps in the guest are empty

`mesa 26.1.8 src/gallium/drivers/virgl/virgl_screen.c:139-159`
(`virgl_get_video_param`) implements only `PIPE_VIDEO_CAP_SUPPORTED`,
`NPOT_TEXTURES`, `MAX_WIDTH`, `MAX_HEIGHT`, `PREFERRED_FORMAT`,
`SUPPORTS_PROGRESSIVE`, `MAX_LEVEL`, `STACKED_FRAMES`, `MAX_MACROBLOCKS`,
`MAX_TEMPORAL_LAYERS` and `default: return 0` - no `PIPE_VIDEO_CAP_ENC_*` at
all. So every encoder-attribute query in the guest is 0 (ffmpeg's "does not
advertise encoder features" / guessed `encoder block size` defaults), even
though the host's libva answers the same queries, and the wire caps carry no
encoder-attribute fields to forward them. Entrypoints *do* work: the same guest
fails a VP9 encode cleanly with `[vp9_vaapi] No usable encoding entrypoint found
for profile VAProfileVP9Profile0 (19).` because the host's VP9 has VLD only.

## 7. Reproduction recipe for the host-side view

`vadump.c` (compiled `gcc -shared -fPIC -ldl -Wl,-soname,libcang-vadump.so`):

```c
/* Interpose libva's vaMapBuffer in cang's VM worker to dump what the host-side
 * VA client (virglrenderer's vrend video path) actually receives from the
 * driver's coded buffer. Injected via patchelf --add-needed + RPATH. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

typedef void *VADisplay;
typedef uint32_t VABufferID;
typedef int32_t VAContextID;
typedef int VAStatus;
#define VA_STATUS_SUCCESS 0
#define VAEncCodedBufferType_ 21
#define VA_INVALID_ID_ 0xffffffffu

typedef struct _Seg {
    uint32_t size;
    uint32_t bit_offset;
    uint32_t status;
    uint32_t reserved;
    void *buf;
    void *next;
    uint32_t va_reserved[4];
} Seg;

typedef VAStatus (*create_fn)(VADisplay, VAContextID, int, unsigned int, unsigned int, void *, VABufferID *);
typedef VAStatus (*map_fn)(VADisplay, VABufferID, void **);
typedef VAStatus (*unmap_fn)(VADisplay, VABufferID);

static create_fn real_create;
static map_fn real_map;
static unmap_fn real_unmap;
static int resolved;
static uint32_t buf_type[4096];
static int map_count;

static void resolve(void)
{
    if (resolved) return;
    resolved = 1;
    real_create = (create_fn)dlsym(RTLD_NEXT, "vaCreateBuffer");
    real_map = (map_fn)dlsym(RTLD_NEXT, "vaMapBuffer");
    real_unmap = (unmap_fn)dlsym(RTLD_NEXT, "vaUnmapBuffer");
}

static FILE *open_log(void)
{
    const char *p = getenv("CANG_VADUMP");
    if (!p || !*p) p = "/dev/shm/cang-vadump.log";
    return fopen(p, "a");
}

VAStatus vaCreateBuffer(VADisplay dpy, VAContextID ctx, int type, unsigned int size,
                        unsigned int num_elements, void *data, VABufferID *buf_id)
{
    resolve();
    if (!real_create) return 1;
    VAStatus st = real_create(dpy, ctx, type, size, num_elements, data, buf_id);
    if (st == VA_STATUS_SUCCESS && buf_id && *buf_id < 4096)
        buf_type[*buf_id] = (uint32_t)type;
    return st;
}

VAStatus vaMapBuffer(VADisplay dpy, VABufferID buf_id, void **pbuf)
{
    resolve();
    if (!real_map) return 1;
    VAStatus st = real_map(dpy, buf_id, pbuf);
    if (st != VA_STATUS_SUCCESS || !pbuf || !*pbuf) return st;
    if (buf_id >= 4096 || buf_type[buf_id] != VAEncCodedBufferType_) return st;

    FILE *f = open_log();
    if (!f) return st;
    Seg *seg = (Seg *)*pbuf;
    int i = 0;
    fprintf(f, "CODEDMAP frame=%d buf=%u\n", map_count, buf_id);
    while (seg && i < 32) {
        fprintf(f, "  seg[%d] size=%u bit_offset=%u status=0x%08x buf=%p head=",
                i, seg->size, seg->bit_offset, seg->status, seg->buf);
        const unsigned char *b = (const unsigned char *)seg->buf;
        unsigned k;
        for (k = 0; k < 24 && k < seg->size; k++) fprintf(f, "%02x", b[k]);
        fprintf(f, "\n");
        seg = (Seg *)seg->next;
        i++;
    }
    fflush(f);
    fclose(f);

    /* first frame's raw bytes to a separate file for comparison */
    if (map_count == 0) {
        const char *bp = getenv("CANG_VADUMP_BIN");
        if (!bp || !*bp) bp = "/dev/shm/cang-vadump.bin";
        FILE *bf = fopen(bp, "wb");
        if (bf) {
            Seg *s = (Seg *)*pbuf;
            while (s) { fwrite(s->buf, 1, s->size, bf); s = (Seg *)s->next; }
            fclose(bf);
        }
    }
    map_count++;
    return st;
}

VAStatus vaUnmapBuffer(VADisplay dpy, VABufferID buf_id)
{
    resolve();
    if (!real_unmap) return 1;
    return real_unmap(dpy, buf_id);
}
```

Injection (safe-mode ignores `LD_PRELOAD`, so the shim is a `DT_NEEDED` of a
`patchelf`'d copy of cang; the prefix needs `share/`, `libexec/` and `lib/cang/`
symlinks back into the store, or the render server cannot find its seccomp
policy and `libkrunfw`):

```sh
D=<state root>
mkdir -p $D/inj/pfx/{bin,lib,libexec}
cp <cang>/bin/cang $D/inj/pfx/bin/cang
ln -s <cang>/share  $D/inj/pfx/share
ln -s <cang>/libexec/cang-helpers $D/inj/pfx/libexec/cang-helpers
ln -s <cang>/lib/cang $D/inj/pfx/lib/cang
gcc -shared -fPIC -O1 -o $D/inj/pfx/lib/libcang-vadump.so vadump.c -ldl -Wl,-soname,libcang-vadump.so
patchelf --force-rpath --set-rpath "$D/inj/pfx/lib:$D/inj/pfx/lib/cang:<glibc lib>:<virglrenderer lib>" $D/inj/pfx/bin/cang
patchelf --add-needed libcang-vadump.so $D/inj/pfx/bin/cang
CANG_VADUMP=$D/workspace/vadump.log CANG_VADUMP_BIN=$D/workspace/vadump.bin $D/inj/pfx/bin/cang --gpu=drm ...
```

`--force-rpath` matters: the VM worker runs with a changed uid, where glibc
ignores `LD_LIBRARY_PATH` and refuses `$ORIGIN` in `DT_RUNPATH`.

## 8. Code references

- `virglrenderer-1.3.0/src/vrend/virgl_video.c:1243` `h264_fill_enc_seq_param` -
  sets `level_idc` (from a level the guest never sends), `intra_idr_period`,
  `max_num_ref_frames`, `pic_order_cnt_type`, cropping and VUI; everything else
  is a `//` comment (1253-1277: `intra_period`, `ip_period`,
  `picture_width_in_mbs`, `picture_height_in_mbs`, `chroma_format_idc`,
  `frame_mbs_only_flag`, `direct_8x8_inference_flag`,
  `log2_max_frame_num_minus4`, `log2_max_pic_order_cnt_lsb_minus4`, ...).
- `virglrenderer-1.3.0/src/virgl_video_hw.h:150-171` (and mesa's copy
  `src/virtio/virtio-gpu/virgl_video_hw.h:150`) `struct virgl_h264_enc_seq_param`
  - 13 fields; no geometry, level or `log2` fields to carry them.
- `virglrenderer-1.3.0/src/vrend/virgl_video.c:1440-1570` `h264_encode_bitstream`
  - renders sequence/picture/slice/misc buffers, no `VAEncPackedHeader*`.
- `mesa-26.1.8/src/gallium/drivers/virgl/virgl_video.c:195-300`
  `fill_h264_enc_picture_desc` - copies exactly the fields the wire struct has.
- `mesa-26.1.8/src/gallium/frontends/va/picture_h264_enc.c:801`
  `vlVaHandleVAEncPackedHeaderDataBufferTypeH264` - where the guest's packed
  SPS/PPS are parsed into the pipe picture desc (the values the wire cannot
  carry).
- `mesa-26.1.8/src/gallium/drivers/virgl/virgl_screen.c:139-159`
  `virgl_get_video_param` - decode caps only, `default: return 0`.
- `virglrenderer-1.3.0/src/vrend/vrend_video.c:210` `sync_video_buffer_to_dmabuf`
  / `:283 vrend_video_enocde_upload_picture` - the host-side input copy (warm-up
  candidate, section 5).
- `virglrenderer-1.3.0/src/vrend/virgl_video.c:~714-730`
  `virgl_video_fill_caps` - the profile/entrypoint table that decides what the
  guest is told (the place to stop advertising `VAEntrypointEncSlice`, section 3
  of the ticket).

## 9. Fix and verification

The fix is a wire extension with one patch per side (full detail in the ticket's
Resolution): `virglrenderer-encode-raw-headers.patch` (host, ships with
`.#virglrenderer`) adds the raw-header fields to vrend's copy of the structs,
fills the H.264 sequence parameters and submits the client's headers as
`VAEncPackedHeader*` buffers; `mesa-virgl-encode-raw-headers.patch` (guest,
image-local through the `cang-va-runtime` driver directory) adds the same fields
to mesa's copy and copies the client's headers out of the pipe picture desc at
`virgl_video_encode_bitstream` time - the VA client only appends them after
`vaBeginPicture`, which is why a copy made when the picture desc is captured
(`virgl_video_begin_frame`) ships zero headers.

Verification run (2026-10-01, cang 0.11.2 with both patches, `--gpu=drm`,
10-frame `testsrc` 640x360, in-guest `ffmpeg`, `LIBVA_DRIVERS_PATH` pointed at
the patched driver through the shared workspace):

```
--- h264_vaapi -> enc18.h264
  exit=0 size=26527
287 18 18 18 18 14227 3512 2577 3118 2734     (per-frame coded sizes)
0000000 00 00 00 01 67 64 0c 1e ac 2b 40 50 17 fc b8 0b   (SPS)
0000032 00 00 01 68 ee 38 30                                (PPS)
0000044 00 00 01 06 05 8e 59 94 8b 28 11 ec 45 af 96 75      (SEI)
  in-guest software decode: decode-exit=0
--- hevc_vaapi -> enc18.h265
  exit=0 size=57765
179 22 22 22 22 28301 8841 7054 7015 6287
0000000 00 00 00 01 40 01 0c 01 ff ff 01 60 00 00 03 00   (VPS)
0000032 01 01 01 60 00 00 03 00 b0 00 00 03 00 00 03 00   (SPS payload)
  in-guest software decode: decode-exit=0
--- decode control (guest VA decode): va-decode-exit=0
```

The negative control is section 3: with the host patch alone (guest driver
unpatched) the same command produced the parameter-set-less stream, and with the
guest patch alone nothing would submit the headers - both halves are needed.

Two behaviours from section 5 and section 6 are unchanged and now charted as
their own tickets: the first ~5 frames of every encoder context encode a black
surface - decoding the run above shows `frame:0..4 pblack:100`, `frame:5..
pblack:12`, and the host control shows no such warm-up - and no
`PIPE_VIDEO_CAP_ENC_*` reaches the guest. See
[`tickets/03-first-frames-are-black.md`](../tickets/03-first-frames-are-black.md)
and
[`tickets/04-encode-attribute-queries-are-zero.md`](../tickets/04-encode-attribute-queries-are-zero.md).

### End-to-end through the built image

The strongest run used no test-time environment override at all: `nix build
.#container` (which includes the image's wrapper-contract and Nix-DB checks) plus
`nix build .#cang-musl` for the rebuilt guest-init, loaded into the hermetic
podman store, then the patched `.#cang` with `--gpu=drm` in a 4 GiB/--root guest.
The image carries `/usr/lib/cang-va-runtime` -> `/nix/store/iwlzmfz2m6dlad8h9fs6sjmn3m1h3367-cang-va-runtime`,
whose `dri/virtio_gpu_drv_video.so` -> `../lib/libgallium.so` ->
`/nix/store/1gfhk7h2zfffr7szvnav8xaq3gb1fz69-mesa-26.1.8/lib/libgallium-26.1.8.so`
(the patched source build).

```
=== VA-API encode 20: end to end through the image ===
# LIBVA_DRIVERS_PATH=/usr/lib/cang-va-runtime/dri:/usr/lib/cang-mesa-runtime/lib/dri
# driver resolves to: /nix/store/1gfhk7h2zfffr7szvnav8xaq3gb1fz69-mesa-26.1.8/lib/libgallium-26.1.8.so
--- h264_vaapi -> /workspace/enc20.h264
  exit=0 size=26517
287 18 18 18 18 14227 3064 2825 3169 2873
0000000 00 00 00 01 67 64 0c 1e ac 2b 40 50 17 fc b8 0b   (SPS)
0000032 00 00 01 68 ee 38 30                              (PPS)
  in-guest software decode: decode-exit=0
--- hevc_vaapi -> /workspace/enc20.h265
  exit=0 size=57362
179 22 22 22 22 28301 7420 7165 7213 6996
0000000 00 00 00 01 40 01 0c 01 ff ff 01 60 00 00 03 00   (VPS)
0000032 01 01 01 60 00 00 03 00                            (SPS payload)
  in-guest software decode: decode-exit=0
--- decode control (guest VA decode): va-decode-exit=0
```

Repo gates run alongside it: `cargo test -p cang-guest-init` (299 passed),
`cargo test -p cang-repository-tests` (48 passed), `cargo fmt --check`, and
`nix build .#container` (which runs the image's wrapper-contract and
Nix-DB-metadata checks).

## 10. The black opening frames, and the fix

Section 5's "the first frames do not carry the input" turned out to be a second,
independent host-side defect with a one-line fix, and it was found by contrasting
the guest stream against the same command on the host:

- guest, 10-frame testsrc 640x360 H.264: per-frame coded sizes
  `287, 18, 18, 18, 18, 14227, ...`, and
  `ffmpeg -vf blackframe=amount=0:threshold=32` reports `pblack:100` for frames
  0-4 (the opening I-frame is contentless) and `pblack:12` from frame 5;
- host control (same command, same render node, no vrend):
  `31, 8, 162, 5030, ...` with `pblack:12` on every frame;
- the pattern is per encoder context, not per guest: three encodes in one guest
  run (black, testsrc, white) share `97, 17, 17, 17, 17, ...`.

Mechanism: `virgl_video_begin_frame` calls `encode_upload_picture` and then
`vaBeginPicture` (`src/vrend/virgl_video.c:905`), and the upload
(`vrend_video.c:283` -> `:210`) only *blits* with GL - `glCopyTexSubImage2D` into
a buffer that the VA engine reads from. Nothing ordered the GL commands against
the VA submission (`grep glFinish\|glFlush src/vrend/vrend_video.c` finds none;
no `origin/main` commit after 1.3.0 touches the file), so the encoder read the
buffer before the copy had executed, and after a few frames the accumulated GL
work happened to be complete in time.

Fix - `nix/pkgs/patches/virglrenderer-encode-upload-fence.patch`, a `glFinish()`
at the end of `sync_video_buffer_to_dmabuf`. Same probe, same guest, patched
cang:

```
h264 per-frame: 14530 3081 3417 3084 2968 2970 3209 2929 3223 2999
h264 blackframe: frame:0 pblack:12 ... frame:5 pblack:12      (was pblack:100)
hevc per-frame: 29797 6753 6500 6207 6503 5862 6001 6568 5949 6351
in-guest software decode: exit 0 for both; guest VA decode: exit 0
```

The fix is host-side, so the image is unchanged. The missing B-frames are a
separate defect: the fence changed them not at all (the guest's stream is
`type:I`/`type:P`, the host control `type:B`).

