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
