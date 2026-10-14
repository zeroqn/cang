---
label: wayfinder:research
title: Carrying upstream PR 889 breaks the guest's VA-API hardware decode
status: open
blocked_by: []
claimed_by: pi session (2026-10-14)
---

# 15 - PR 889 breaks the guest's VA-API hardware decode

## Question

The re-base onto upstream `b9f27258` folds in upstream PR 889 ("vsock: drain rxq
in muxer thread", cherry-picked as `b796f877`). Carried on purpose (bob asked for
the PR explicitly). Does it change guest-visible behaviour?

## Answer: yes - it regresses the guest's VA-API decode, on both bases

`tools/virglrenderer-cang-smoke` arm 2 (VA-API encode/decode in a `--gpu=drm`
guest) passes on both bases **without** the PR and fails on both **with** it. Same
guest image, guest-init, kernel (`libkrunfw-v5.6.2-cang.3`), virglrenderer and
hermetic podman store in every arm; the only variable is the libkrun tree compiled
into cang (verified by diffing each build's `cang-workspace-source`).

| cang built from | PR 889 | arm 2 |
| --- | --- | --- |
| fork tip `2855f4d1` (pre-rebase) | no | **PASS** y 46.633758 u 44.227307 v 44.408519 |
| fork tip `2855f4d1` + PR 889 (`f494ef63`) | yes | **FAIL** y 5.372522 u 9.994820 v 9.662058 |
| re-based tip `f8c18a1c` (b9f27258 + fork + PR 889) | yes | **FAIL** same numbers |
| re-based tip minus PR 889 (`95a653bf`) | no | **PASS** same numbers as `2855f4d1` |

The PR is therefore necessary and sufficient; neither the 60 upstream commits of
the re-base nor the fork's own commits are implicated.

Symptom: encode, the VA profile set, the decoder's frame counts and the host-side
attribution all pass; the **decoded picture** is a flat neutral frame - luma all
zero, both chroma planes 128, frame 0 entirely zero - i.e. the guest reads its own
cleared VA surfaces, never the picture the host decoded. Raw arms:
`notes/15-pr-889-vaapi-arms.md`.

## Exonerated

- `51367cfa` (virtio/gpu: offer `VIRTIO_GPU_F_BLOB_ALIGNMENT`): with the whole
  `src/devices/src/virtio/gpu` directory byte-identical to the pre-rebase tip the
  failure persists (`a1d33469`; and with the ACPI commit reverted as well,
  `a04ea97c`).
- `5bc7468a` (arch/x86_64/acpi: edge-triggered virtio-mmio IRQs): reverted alone
  (`ecbfaa7a`) and together with the GPU commit, the failure persists.
- Nothing on the guest side differs: guest-init, image, kernel, renderer, cang's
  runtime closure and the cargo vendor set are identical between the arms, and the
  embedded `krun-init` blob is byte-identical.
- The guest's clock is fine with the PR (guest `date` matches the host to the
  second), so timesync delivery is not what breaks.

## Mechanism: open

PR 889 is 31 lines in `src/devices/src/virtio/vsock/muxer_thread.rs` - a
`drain_rxq()` at the top of every muxer iteration plus `signal_used_queue()` - and
it is purely vsock, while a blank picture is a GPU-surface read-back, so the link
is indirect. Two candidates, neither measured:

1. **Shared rx queue.** The fork's timesync device (`vsock/timesync.rs`) shares the
   muxer's `Arc<Mutex<VirtQueue>>` and pushes its port-123 DGRAM through it, and
   `process_stream_rx()` on the event handler drains the same `rxq`; PR 889 adds a
   fourth user loop there. The clock check argues against this one.
2. **Host-side ordering/timing.** The decoded-surface read-back is the freshly
   landed `virglrenderer-decode-surface-readback.patch` +
   `virglrenderer-decode-surface-sync.patch` (guest-vaapi-video tickets 09/10), and
   a blank picture is exactly what a guest that retires its decode fence too early
   reads - the same class as ticket 03 (unsynchronised upload blit) and ticket 10
   (cached export). PR 889 adds a thread that wakes on every iteration of the
   muxer loop.

## Next step

Measure the link before choosing a fix: arm 2 against a failing build with (a) the
`timesync` cargo feature off on `crates/cang-libkrun`, and (b) the host otherwise
idle, sampling the host's per-process CPU while the decode runs. If (a) passes, the
fix belongs in the fork's shared queue; if the blank picture survives a slower
guest and a quiesced host, the read-back itself needs the sync.

## Deliverable

A decision on PR 889 with evidence: carried with a fix that keeps the VA-API decode
green, or dropped from the branch and the fork release re-cut, with the reason
recorded here and in `../../../guest-vaapi-video/MAP.md`.
