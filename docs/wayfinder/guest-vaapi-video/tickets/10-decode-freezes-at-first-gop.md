---
label: wayfinder:research
title: The guest's decoded output freezes at the first GOP boundary
status: open
blocked_by: []
claimed_by: pi session (2026-10-13)
---

# 10 - the guest's decoded output freezes at the first GOP boundary

Found while verifying ticket 09's chroma fix with `tools/virglrenderer-cang-smoke`.
It is a *second*, independent defect in the decode path: the plane reconciliation
works, and everything after the first GOP is the same frame over and over.

Measured 2026-10-13, cang 0.12.0 + ticket 09's patches, image
`localhost/cang:latest` (ffmpeg 8.1.2, guest VA driver = cang's patched mesa
26.1.8), 640x360 x 60 frames of lavfi `testsrc2`, `-c:v libx264 -qp 26 -bf 1 -g 30`
(so the second IDR is frame 30), one `--gpu=drm` VM. Both decodes are written by
the same guest run, so the comparison is decode-against-decode, not against the
encoder's loss.

```
mse(hardware decode, software decode) of the same clip, per frame, per plane:

  n= 0 .. n=29   mse_y 0.000   mse_u 0.000   mse_v 0.000     <- bit-exact
  n=30           mse_y 541.093 mse_u ...     mse_v ...       <- the second IDR
  n=31 .. n=59   mse_y ~360-550 ...

after the break the guest repeats one frame:
  mse(hw[n], hw[n+1])    = 0.0   for every n >= 31   (software decode: 167-209)
  mse(hw[n], hw[29])     = 0.0   for n = 31, 32, 40, 59
```

So from the second IDR on, the guest's decoder output is frozen on the last frame
it produced before it. The defect is not new - the pre-fix runs of the same arm had
the identical shape (frame 1 `mse_y 0.00`, frame 60 `mse_y 379.97`, and a *constant*
`mse_v 22540` because that stale frame's Cr plane was still the zeroed one ticket 09
fixed) - it was simply hidden behind ticket 09's much larger error.

## What it is not (measured 2026-10-13, same clip, one VM per probe)

It is **frame 31 exactly**, and not the GOP boundary the ticket was first named
after. Four encodings of the same 60-frame source, decoded hardware-against-software
in the same VM:

| variant | first frame whose mse > 1 | last mse_y |
| --- | --- | --- |
| `-bf 1 -g 30` | 31 | 369.79 |
| `-bf 1 -g 60` (a single IDR) | **never** (all 60 frames `mse 0.000`) | 0.00 |
| `-bf 1 -g 15` (IDRs at 1, 16, 31, 46) | 31 | 389.79 |
| `-bf 0 -g 30` | 31 | 385.76 |

So the IDR at frame 16 does *not* freeze it and the IDR at frame 31 does; with a
single IDR nothing freezes at all. B-frames are irrelevant.

Five decodes of the *same* `-g 30` clip, each with one client-side knob:

| knob | first frame whose mse > 1 |
| --- | --- |
| none | 31 |
| `-threads 1` | 31 |
| `-extra_hw_frames 8` (a bigger guest surface pool) | 31 |
| `-flags +low_delay` (no reordering) | 31 |

All four freeze at 31 with the *same* frozen content (last `mse_y` 369.79), so it is
not the guest's decoder pool size, frame threading or the reorder logic. It is a
count boundary on the host/vrend side that only bites once the stream has had a
second IDR - which is what points at vrend's per-buffer state (a buffer recycled
after 30 frames carrying a stale `dmabuf`, `egl_image` or `va_sfc`) rather than at
the guest's client.

## What the host still does after the freeze (instrumented, 40-frame clip)

A rooted diagnostic `virglrenderer` logging every decode callback
(`vrend_video_decode_completed`: guest buffer handle, plane resource handles,
dmabuf fd, cached EGL image; `virgl_video.c:decode_completed`: buffer and `va_sfc`),
40 frames, `-bf 1 -g 30`:

```
decode callbacks: 39        create_buffer calls: 18        virgl decode_completed: 39

create_buffer handle=11 .. handle=80       <- the guest's ffmpeg grows a pool, one
                                              new surface per frame
decode#0..#17   handle 11,16,20,24,...,80  one new buffer each time
decode#18       handle=20  fd0=1633  egl0=0x...   <- first recycle: pool of 18
decode#29       handle=48  res=(51,52,53)         <- this is frame 30 (1-based)
decode#38       handle=11  res=(3,4,5)
```

Every frame in the 40-frame clip got a decode callback and a copy, with the same
per-buffer dmabuf fd and EGL image as on the buffer's first use, and the guest
received all 40 frames (13 824 000 bytes = 40 x 640x360 nv12). The frozen bytes the
guest reads are *surface 48's* content - the surface frame 30 (1-based) was decoded
into. So the host keeps decoding and copying; the guest's read-back is what stops
tracking the surface it just submitted a picture to, and keeps reading one fixed
surface.

Reconciled with the knob sweep, the trigger is **an IDR that arrives after the
guest's surface pool has started recycling**: the pool grows for the first 18
frames, so the IDR at frame 16 lands while every surface is still fresh (no freeze)
while the IDRs at frame 31 (`-g 30`, and `-g 15` with IDRs at 1/16/31) land after
recycling has started, and a single IDR (`-g 60`) never freezes. That points at the
guest's VA driver surface bookkeeping across an IDR (mesa's virgl VA frontend and its
`vaCreateSurfaces`/`vaSyncSurface`/buffer-pool handling) rather than at vrend's copy,
so the remaining work is guest-side.

## Where it can be

The decode path, in order: the guest's ffmpeg submits a picture to its VA driver
(mesa's virgl driver) -> vrend forwards it to the host's VA decoder -> the host's
decoder writes a host surface -> `vrend_video_decode_completed` exports that surface
and `sync_dmabuf_to_video_buffer` copies it into the guest's video buffer resources
-> the guest's ffmpeg reads those resources back (`-vf format=nv12`).

Candidates, none measured yet:

- **Stale per-plane EGL images in vrend.** `vrend_video_plane.egl_image` is created
  once (`if (EGL_NO_IMAGE_KHR == plane->egl_image)`) and reused for every later
  frame of that buffer, and `virgl_video_buffer.dmabuf` is likewise exported once
  per buffer. If the host's decode for the second GOP lands in a *different* host
  surface (or the same buffer is re-exported with different memory), the cached
  image keeps copying the old memory and the guest sees the frame it last got.
- **Buffer identity across the pool change.** The guest's ffmpeg holds a surface
  pool; at an IDR it may allocate or rotate buffers, and the copy could be writing
  the guest buffer the *previous* frame used.
- **The guest's readback**, if its own copy-out is the thing that freezes.

## Next step

One instrumented run of the smoke's decode arm with a rooted diagnostic
`virglrenderer` (the recipe that produced ticket 09's evidence: add
`nix/pkgs/patches/virglrenderer-diag-decode.patch`-style markers to
`vrend_video_decode_completed` and `sync_dmabuf_to_video_buffer`, log
`(frame, buf pointer, guest res_handle, host surface id, dmabuf fd, plane
egl_image)`, then compare what the copy targets per frame with what the guest reads
from). That distinguishes the three candidates in one run.
