---
label: wayfinder:research
title: The guest's encoder attribute queries are all zero (and cost B-frames)
status: open
blocked_by: []
claimed_by:
---

## Question

`vainfo` in the guest advertises `VAEntrypointEncSlice` for H.264/HEVC, but every
encoder-attribute query behind it answers 0, so a client picks its encoder
settings blind. Is there a route for the host's answers to reach the guest, and
does anything in the guest actually depend on them?

## Evidence already gathered

- `mesa-26.1.8/src/gallium/drivers/virgl/virgl_screen.c:139-159`
  (`virgl_get_video_param`) implements ten decode caps and `default: return 0`;
  there is no `PIPE_VIDEO_CAP_ENC_*` case at all.
- The virgl caps wire structure carries no encoder-attribute fields, so the host's
  answers cannot be forwarded even though the host's libva has them.
- It is not only a log line: it costs B-frames. ffmpeg's VAAPI encoder decides
  its GOP from `vaGetConfigAttributes(VAConfigAttribEncMaxRefFrames)`
  (`libavcodec/vaapi_encode.c:1638-1665`, `ref_l0 = value & 0xffff`,
  `ref_l1 = value >> 16`), mesa's VA frontend answers that attribute from
  `PIPE_VIDEO_CAP_ENC_MAX_REFERENCES_PER_FRAME` and falls back to `value = 1`
  when the cap is `<= 0` (`src/gallium/frontends/va/config.c:320-329`, i.e. past
  references only, no future references), and virgl answers every encoder cap
  with 0 - so the guest gets `ref_l0 = 1, ref_l1 = 0` while the host gets
  `1 / 1`. The two runs say it outright:

  ```
  guest: [h264_vaapi] Using intra and P-frames (supported references: 1 / 0).
  host:  [h264_vaapi] Using intra, P- and B-frames (supported references: 1 / 1).
  ```

  The guest's stream is therefore `type:I`/`type:P` where the host control emits
  `type:B` (measured both before and after ticket 03's fence, which changes
  nothing about it), i.e. the silent cost is B-frames' bitrate/quality win.
- Encoding itself works (ticket 02), so this is a completeness gap rather than a
  blocker - but a gap with a measurable price, not just a missing log line.
- Entrypoints are unaffected: they come from the host's profile/entrypoint table
  (`virglrenderer-1.3.0/src/vrend/virgl_video.c` `virgl_video_fill_caps`), which
  is why `EncSlice` is listed while the attributes are empty.

## Deliverable

- The encoder attributes carried over the virgl caps, which is the natural place
  for them: the caps are already a per-(profile, entrypoint) array
  (`virgl_video_caps` in `src/virgl_video_hw.h`, filled by
  `virgl_video_fill_caps`, `src/vrend/virgl_video.c:692-730`), so the host can
  fill `max_references_per_frame` (and whatever else the VA frontend consumes)
  from `vaGetConfigAttributes` per entry and the guest's `virgl_get_video_param`
  can return it for `PIPE_VIDEO_CAP_ENC_*` - the same two-sided patch shape as
  ticket 02, in the host-to-guest direction, with no synchronous query needed.
- Verification that B-frames then actually work end to end, and the size of the
  win: the acceptance evidence is the guest's own log line switching to
  "intra, P- and B-frames" plus a same-quality bitrate comparison against the
  host control. If the loss of B-frames turns out to be deeper than the
  attribute (for instance the wire's DPB/`ref_pic_list` handling cannot express
  a B-frame's references), that belongs in its own ticket.
- Whatever is not carried stays documented, with the reason.

## First attempt, 2026-10-01: the attribute is not the whole loss

The caps extension was built and tested (host `virglrenderer-encode-caps.patch`:
fills `max_past_references` / `max_future_references` per caps entry from
`vaGetConfigAttributes(VAConfigAttribEncMaxRefFrames)`; guest
`mesa-virgl-encode-caps.patch`: returns them from
`PIPE_VIDEO_CAP_ENC_MAX_REFERENCES_PER_FRAME`, both in the `reserved:20` space of
`struct virgl_video_caps` so the struct size does not change). The guest's
encoder immediately agreed: `Using intra, P- and B-frames (supported references:
1 / 1)` - but the stream got *worse*, not better:

| 10-frame testsrc 640x360, QP 20 | sizes | file |
| --- | --- | --- |
| before the caps change | `287, 18, 18, 18, 18, 14227, 3064, ...` | 26 517 B |
| after (B-frames enabled) | `14531, 3896, 14144, 13916, 13881, 13959, 13885, ...` | 129 821 B |
| host control (native, same command) | `31, 8, 162, 5030, 828, 193, ...` | 8 723 B |

Still decodable (exit 0) and the guest's VA decode is unaffected, but ~5x the
host's size and ~1.5x the guest's *previous* size: the client now codes
B-frames, and the price it pays is that a B-frame's references do not survive
the trip. The picture type crosses the wire, the reference *lists* a B-frame
needs (`ref_pic_list0/1`, the DPB state, the reordering) do not - so the host's
encoder codes each frame almost as if it were intra.

So the fix is two-layered: this ticket's attribute forwarding is the *enabler*
(kept, but not shippable on its own), and the reference/DPB plumbing is the part
that makes it pay - a follow-up of the same kind as ticket 02, to be found by
diffing what the host's VA submission carries for a B frame against what vrend
submits (the method that found the packed headers). Until that lands, the
attribute should not be advertised: forwarding it alone makes a guest encode
1.5x bigger.

## First implementation attempt (2026-10-03): caps land, the DPB fill hangs the encoder

The reverted caps patches were recovered from scratch and a third patch written for the piece the
caps alone cannot deliver: vrend leaves every `VAEncPictureParameterBufferH264.ReferenceFrames`
entry invalid, so a driver has no DPB to mark and a B-frame's future reference cannot be predicted
from - the reason a caps-only build made streams *larger* instead of smaller. The new host patch
(`nix/pkgs/patches/virglrenderer-encode-reference-frames.patch`) tracks each ring slot's
picture-order count (`ref_pic_poc[32]`, set when that frame is encoded) and fills `ReferenceFrames`
from the client's own `ref_idx_l0_list`/`ref_idx_l1_list` plus the active counts.

Measured, guest `--gpu=drm`, 6 frames of testsrc 320x240 (45 s cap per arm):

| build | `-bf 0` | `-bf 2` |
| --- | --- | --- |
| caps + reference-frames | **hangs** (rc=124; a 20 s 640x360 encode never finished either) | - |
| caps only | 4659 B in 2 s, rc=0 | 16895 B in 1 s, rc=0 |

So the caps forwarding is sound (and the encoder stays healthy), while the DPB fill as written
stalls the encoder outright - even for `-bf 0`, which never asks for a future reference. The
patch is therefore **not wired** in `nix/lib/systems.nix` (it stays in-tree, unwired, with the
reason recorded here), and the caps patch is.

Next attempt, in order: (1) fill `ReferenceFrames` only for references whose POC vrend actually
recorded (a `known` flag per ring slot) and never with a stale/zero POC; (2) set `frame_idx`
alongside `picture_id`, since VA's DPB entry carries both; (3) if the stall persists, check
whether the *guest* client's `ref_idx_*_list` values are frame numbers or surface ids - vrend
treats them as frame numbers (`get_enc_ref_pic(codec, frame_num)`), and a mismatch there would
create surfaces per reference and confuse the driver's DPB.

Measurement still to take once a non-stalling DPB exists: the real-content bitrate against the
host control on the same command (`~/cang` clip, 640x360, QP 26) - the host's own numbers are
1 671 174 B at `-bf 0` and 1 123 666 B with B-frames, i.e. the 33% prize this ticket is after.

## Second measurement round (2026-10-03): the hangs, and which patch causes them

All verification was moved into bounded probes: 6 frames of testsrc 320x240 (`tiny`) versus the 20 s
640x360 clip (`real`), 45-240 s caps per arm, one `PROBE` line per arm so a stall still yields data,
and the whole run delegated to a sub-agent so the parent stays responsive.

| build | tiny `-bf 0` / `-bf 2` | real `-bf 0` |
| --- | --- | --- |
| caps + DPB (`virglrenderer-encode-reference-frames.patch`) | **hangs** | - |
| caps only | 4659 B / 16895 B, rc=0, 2 s / 1 s | **hangs at `frame= 0`** |
| **no caps, no DPB (old cang) + the same new image** | - | **hangs at `frame= 0`** |

The third row is the decisive one: the *unmodified* cang binary plus the image built in this round
stalls exactly the same way, so the guest-side half in the image (`mesa-virgl-encode-caps.patch`) is
implicated independently of the host patches. The guest's own `ffmpeg` log for the stalled arm shows
the encoder accepted the configuration and then sat at `frame= 0` for two and a half minutes - the
first picture never completes - and the stall is uninterruptible (the arm's own `timeout` never fires,
the outer one kills the VM). The 6-frame testsrc arms pass, so it is the material (640x360 H.264
source), not the frame count.

A second, structural defect in the guest half as written: against an **unpatched** host it reads
`vcaps->max_past_references`/`max_future_references`, which on that host are still `reserved` bits -
i.e. whatever the host happened to leave there. A wire field that can be read as garbage by a patched
peer talking to an unpatched one is the wrong design, and it is exactly the old-cang case measured
above.

**State after this round:** all three patches (host caps, guest caps, host DPB) are unwired in
`nix/lib/systems.nix` and kept in-tree with these reasons; cang and the image are being rebuilt
without them and the real-content encode re-verified, so the repository returns to the known-good
baseline. The B-frame work needs a different shape: the caps must be negotiated so an unpatched peer
cannot misread them (a new caps field with a version/flag, or the value carried in the existing
`reserved` bits only when a feature bit says so), and the DPB fill must use only picture-order counts
vrend actually recorded, with `frame_idx` set alongside `picture_id`.
