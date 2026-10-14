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

## Second measurement, 2026-10-14 (after v0.13.0)

Both arms were rebuilt from the pinned fork (`421247f6` = v2.0.0-cang.7, PASS and
`f8c18a1c` = that tree plus PR 889, FAIL) and the guest was left to decode the same
staged H.264 clip with the same guest-init, image, kernel, renderer and store. The
failure reproduces byte-for-byte (`y:5.372522 u:9.994820 v:9.662058` against
`y:46.633758 u:44.227307 v:44.408519`), and the guest's own view localises it:

| arm | hardware decode | software decode, same VM |
| --- | --- | --- |
| `421247f6` | md5 `3006d5a7881626c4c1ca1ff2b7b4eb91` | md5 `3006d5a7881626c4c1ca1ff2b7b4eb91` |
| `f8c18a1c` (+ PR 889) | md5 `f22b572fb3967b0e4377a04088a546c0`, first 8 Y bytes all zero | md5 `3006d5a7881626c4c1ca1ff2b7b4eb91` |

So the clip, the guest userland, the file I/O and the software decode are
identical in both arms, and only the hardware path returns a cleared picture -
with the useful side effect that a working arm's `hw` decode is bit-identical to
its `sw` decode, which is worth keeping as the guard.

Tracing, so far:

- the pinned prebuilt `libvirglrenderer` *does* carry `VIRGL_LOG_LEVEL`,
  `VIRGL_LOG_FILE` and `VREND_DEBUG` (an earlier "no knobs" reading was a missing
  `strings` binary), and the render server's `/proc/<pid>/environ` shows those
  variables arriving - but no log file appears even with `--seccomp=off
  --landlock=off`, so that build's file logger needs another trigger;
- the guest side's `VIRGL_DEBUG`/`MESA_DEBUG` (both present in the image's
  `libgallium`) produce logs of identical shape in both arms (705 lines, the same
  counts), so the guest driver's trace does not separate them.

Next: trace the DRM ioctls of the render helper (`strace -f -e trace=ioctl`) in
both arms and compare the `VIRTGPU_EXECBUFFER` / `RESOURCE_CREATE` /
`TRANSFER_FROM_HOST` sequence the decode and its read-back produce.
