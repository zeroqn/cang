---
label: wayfinder:research
title: The guest's encoder attribute queries are all zero
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
- The consequence observed in the guest is a log line and guessed defaults:
  `ffmpeg` reports "driver does not advertise encoder features". Encoding itself
  works (ticket 02), so this is a completeness gap, not a blocker.
- Entrypoints are unaffected: they come from the host's profile/entrypoint table
  (`virglrenderer-1.3.0/src/vrend/virgl_video.c` `virgl_video_fill_caps`), which
  is why `EncSlice` is listed while the attributes are empty.

## Deliverable

- A decision: carry the encoder attributes over the virgl caps (which fields,
  which wire shape, how to keep an older guest or host working), or leave the
  queries at 0 and document that. In the carrying case, a measurement of what
  changes in the guest's behaviour.
