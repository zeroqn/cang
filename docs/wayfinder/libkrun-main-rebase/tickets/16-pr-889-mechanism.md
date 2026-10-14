---
label: wayfinder:research
title: Why do PR 889's rxq drain and its used-queue signal blank the guest's VA-API decode?
status: open
blocked_by: []
claimed_by: pi session (2026-10-14)
---

# 16 - why does PR 889 blank the guest's VA-API decode?

## Question

Upstream PR 889 returns with upstream main, so the fork cannot drop it for ever.
[Ticket 15](15-pr-889-breaks-vaapi-decode.md) established that carrying it blanks
the guest's hardware-decoded frames (luma 0, chroma 128 - the guest reads its own
cleared VA surfaces) and that the re-base itself does not. What is the mechanism,
and what has to change on cang's side so the PR can be carried with the VA-API
decode green?

## What is established

- **Both halves of the PR do it on their own**, on top of the branch tip:
  `drain_rxq()` with the signal removed, and `signal_used_queue()` each iteration
  with no drain. It is not one specific interaction between the two.
- **The result is binary and repeatable per build** (six FAIL runs, four PASS runs,
  identical md5s): `f22b572f…` fails, `3006d5a7…` passes for the same clip.
- **CPU load is not the lever**: three guest CPU hogs leave PR 889 failing, 28 host
  CPU hogs leave the pre-rebase tip passing.
- **The guest does the same work in both states**: identical software decode, 60
  decoded frames, no ffmpeg/libva error, no guest kernel message, identical
  negotiated virtio-gpu features, and debug logs of the same size whose only
  difference is where the `Map surface` lines sit relative to the slice decodes.
- **Timesync is not the link**: the guest's clock tracks the host in both states.

## Leading hypothesis

The decoded picture is read back before the host has put it there: the guest's
read-back (`Map surface`) is ordered differently against the decode, and PR 889's
extra vsock work moves the guest's view of the decode's completion deterministically.
On the host side the picture is written by the freshly landed
`virglrenderer-decode-surface-readback.patch` /
`virglrenderer-decode-surface-sync.patch` pair (guest-vaapi-video tickets 09/10), so
a decode the guest considers complete before that read-back has run returns the
guest's own cleared surface - exactly the failing picture.

For: the interleaving is the only guest-side difference, and the blank frame's exact
contents (luma cleared to 0, chroma to the neutral 128, the first surface untouched)
are the driver's cleared surfaces rather than a corrupt decode.
Against: a 28x host slowdown does not break the passing arm, so a plain CPU race is
not sufficient on its own.

## Next step

Instrument the host the way tickets 09/10 did: markers in the vrend read-back
(`virgl_video.c` / `vrend_video.c`), and if the ordering is not visible there,
interpose libva on the host to log every `vaSyncSurface` / `vaDeriveImage` /
`vaMapBuffer` against the decode's completion, then diff a PASS and a FAIL arm. The
fork's `virtio-gpu` fence path is the second place to look: the guest's fence must
not be retired before the read-back has written the guest's planes.

## Deliverable

A fix - in the host read-back, the fence path, or the guest driver - that keeps the
VA-API decode green with PR 889 carried, so the next re-base onto upstream main does
not have to drop it; or a demonstration that the PR must be paired with a specific
upstream change before cang can carry it.
