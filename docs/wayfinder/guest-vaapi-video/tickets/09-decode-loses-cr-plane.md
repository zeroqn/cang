---
label: wayfinder:research
title: The guest's VA-API decode loses the Cr plane
status: open
blocked_by: []
claimed_by: pi session (2026-10-13)
---

# 09 - the guest's VA-API decode loses the Cr plane

Found by the new live smoke `tools/virglrenderer-cang-smoke` (arm 2, VA-API
encode/decode in a `--gpu=drm` guest). Encode, the VA profile set, the hwframe
decodes' frame counts and the host-side attribution all pass; the **decoded
picture** is what fails: compared with the source, luma and Cb are *bit-exact*
and Cr is zero.

Measured 2026-10-13, cang 0.12.0 (`a253e0b`), image `localhost/cang:latest`
(ffmpeg 8.1.2, guest VA driver = cang's patched mesa 26.1.8), 640x360 x 60 frames
of lavfi `testsrc2`, H.264 High, qp 26, `-bf 1`, one `--gpu=drm` VM:

| arm | y | u | v |
| --- | --- | --- | --- |
| software decode of the same clip (control) | 46.64 | 44.15 | 44.36 |
| hardware decode, copied out by the decoder (`-hwaccel vaapi -vf format=nv12 -pix_fmt nv12`) | **5.35** | 4.61 | **4.62** |
| hardware encode -> software decode (the encode arm) | 46.54 | 43.63 | 42.91 |
| hardware encode -> hardware decode (round trip) | **5.37** | 4.61 | **4.62** |

Per frame (`ffmpeg -lavfi psnr=stats_file=…`, `evidence/vaapi-psnr-*-perframe.log`):

```
n:1  mse_y:0.00 mse_u:0.00 mse_v:22540.62  psnr_y:inf psnr_u:inf psnr_v:4.60
n:60 mse_y:379.97 mse_u:691.35 mse_v:22392.52  psnr_y:22.33 psnr_u:19.73 psnr_v:4.63
```

so luma and Cb are byte-identical to the software decode for the opening frames
(the later luma/Cb drift is the damaged Cr feeding the decoder's references), and
Cr is not merely noisy: byte-wise it is **all zero** (`V mean 0, stddev 0`;
decoded chroma row `[83, 0, 94, 0, 91, 0, …]` against the source's
`[90, 240, 90, 240, …]`).

## Mechanism

Instrumented `virglrenderer` (`fprintf` markers in `virgl_video.c` /
`vrend_video.c`, built as a rooted throwaway `.#cang`, run under
`--gpu=drm`), decode of the same clip:

```
create_buffer: pipe_format=165 640x368 planes=3 va_format=0x1     <- the guest's decode buffer
create_buffer: pipe_format=166 640x368 planes=2 va_format=0x1     <- the guest's encode buffer
descriptor fourcc=0x3231564e (NV12) 640x368 objects=1 layers=2
  layer[0] format=0x20203852 (R8)   planes=1 off0=0      pitch0=768
  layer[1] format=0x38385247 (GR88) planes=1 off0=282624 pitch0=768
sync_dmabuf_to_video_buffer: dmabuf 640x368 planes=2 buf planes=3
  plane[0] fourcc=R8   off=0      pitch=768 egl=640x368 dst_res=3 640x368
  plane[1] fourcc=GR88 off=282624 pitch=768 egl=320x184 dst_res=4 320x184
```

`vrend_video.c`'s `sync_dmabuf_to_video_buffer()` walks
`for (i = 0; i < dmabuf->num_planes && i < buf->num_planes; i++)` and copies host
plane *i* into the guest's plane *i* through a per-plane EGL image plus
`glCopyTexSubImage2D`. The two sides describe the *same* `VA_RT_FORMAT_YUV420`
surface differently:

- the host exported it as **NV12**: plane 0 luma (R8), plane 1 chroma (GR88,
  interleaved CbCr);
- the guest's video buffer has **three** planes - luma, Cb, Cr, which is what a
  `VA_RT_FORMAT_YUV420` surface means to the guest's VA frontend.

The loop therefore stops after two planes: the guest's luma gets the luma plane
(bit-exact), the guest's **Cb** gets the *R* channel of the interleaved chroma
plane (bit-exact), and the guest's **Cr** plane is never written - it keeps its
zero-initialised contents, which is exactly what the guest reads back as V.

`va_format_from_pipe()` maps every 8-bit 4:2:0 pipe format to the generic
`VA_RT_FORMAT_YUV420`, so nothing in the ABI pins NV12 vs I420; the host's driver
(radeonsi) chooses NV12. The *encode* direction has the same class of mismatch but
happens to line up today: the guest's encode buffer has two planes (166, NV12), so
the copy is 1:1 - which is why the encode arm passes while the decode arm does not.

Upstream parity: `sync_dmabuf_to_video_buffer()` in `virglrenderer` **main** is
byte-identical to the pinned revision and to 1.3.0, so this is upstream's
assumption, not cang's regression, and not a consequence of moving the pin to
`c8e3fc3`.

## Attempted, and what it rules out

- **Requesting a matching surface:** `VASurfaceAttribPixelFormat = VA_FOURCC_I420`
  for a three-plane guest buffer (plus, in the same experiment, per-plane fourccs in
  `fill_video_dma_buf` and chroma dimensions of half size instead of `width/(i+1)`).
  The host's driver ignores the attribute: the exported descriptor stays
  `fourcc=NV12 … layers=2`, and the measured PSNR is unchanged. So the VA surface's
  layout cannot be pinned from this side with `vaCreateSurfaces` alone.
- **The explicit `hwdownload` form** (`-hwaccel_output_format vaapi -vf
  hwdownload,format=nv12`) fails separately and is *not* part of this ticket: the
  filter gets `EINVAL` from the derive/map path and segfaults when a format filter
  follows (rc 139). The smoke records it as unscored `INFO`.

## Candidate fixes

1. **Host-side de-interleave.** Keep the driver's NV12 surface and fill the guest's
   extra chroma planes from the interleaved plane's *G* channel. A plain copy cannot
   select a channel, so this needs a sampled blit (texture swizzle `R←G`, or a
   small shader) into the guest's Cb/Cr resources - `glCopyTexSubImage2D` from a
   GR88 attachment only ever yields R.
2. **Guest-side layout.** Make the guest describe the decode buffer as NV12 (two
   planes) so the 1:1 copy holds, i.e. change what the guest's VA frontend chooses
   for a `VA_RT_FORMAT_YUV420` decode surface (mesa's VA frontend / virgl driver,
   which the image already builds with cang's patches).
3. **Upstream report.** Either way the ABI assumption is worth reporting: the copy
   silently ignores the guest's extra planes rather than failing.

## Deliverable

The guest's hardware decode matching the software decode per plane (luma and both
chroma planes at the floors the smoke scores), with the smoke's
`vaapi-psnr-decode` / `vaapi-psnr-roundtrip` green; the fix carries a note in the
ticket and, if it lands in cang, a patch in `nix/pkgs/patches/` plus the
repository-test patch list.
