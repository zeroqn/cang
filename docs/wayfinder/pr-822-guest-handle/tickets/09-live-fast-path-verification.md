---
label: wayfinder:task
title: Show a cang guest taking the zero-copy guest-handle fast path
status: closed
claimed_by: pi session (2026-09-29)
blocked_by: ["06-libkrunfw-release-and-pin", "07-fork-vmm-port"]
---

## Question

Execute ticket 04's evidence design on the fully pinned stack: a cang guest,
running a Wayland `wl_shm` client through `deps/wl-cross-domain-proxy` on the GPU
path, must demonstrably take `CREATE_GUEST_HANDLE` rather than the memfd copy
path.

Record: host `/dev/udmabuf` and its seal contract, the guest's
`/dev/udmabuf`, the proxy's feature probe result, the negotiated bits on both
sides, an observed `udmabuf_create` for the shm pool, the proxy's own zero-copy
mode line, and the guest-side proof that no copy happened. **Add a measured A/B
delta** (ticket 04): a small guest-side `wl_shm` client blitting to a pool at a
fixed rate, run with `--zero-copy-shm` on and off, reporting frames/s and guest
CPU - no such client exists in the image today, so it has to be written and its
home decided (a `tools/` sibling of `virgl-guest-probe`, or an image layer). Say explicitly which leg could
not be shown here, and whether the chromium GPU smoke still passes (regression
backstop: the previous map's venus regression was only caught by the smoke).

## Deliverable

`notes/09-live-fast-path-verification.md` with the run transcript and the verdict
- the fast path engages, or the specific gate that still blocks it.

## Progress (2026-09-29, pi)

The guest side of the fast path **engages** - a udmabuf is created for the
`wl_shm` pool and the kernel imports it as a guest-handle blob (no
`PRIME_HANDLE_TO_FD`, no copy) - and the run then **stalls in the guest proxy**:
it never answers the client's first `wl_display.sync` and spins on
`VIRTGPU_EXECBUFFER`, so the benchmark loop runs but no A/B can be read out of
it. Transcript, the strace evidence chain, the two permission gates found (host
`/dev/udmabuf` mode, guest devtmpfs node) and the passing Chromium GPU smoke are
in [`../notes/09-live-fast-path-verification.md`](../notes/09-live-fast-path-verification.md).

The stall is ticketed separately as
[`12-guest-proxy-fast-path-stall.md`](12-guest-proxy-fast-path-stall.md), which
now blocks this ticket. The A/B client it needs exists
(`tools/wl-shm-bench/`, validated against a host weston); only the completed run
is missing.

## Resolution (2026-09-29, pi)

**The fast path is live end to end and measurably cheaper.** Full transcript,
strace counts, the measured A/B, the host driver's limits and the two permission
gates are in [`../notes/09-live-fast-path-verification.md`](../notes/09-live-fast-path-verification.md).

- A `wl_shm` client through `wl-cross-domain-proxy` in a `--gpu=drm --wayland
  --zero-copy-shm` guest: param 10 = 1, virtio-gpu bits 6/7 negotiated, the proxy
  issues `UDMABUF_CREATE` for the pool and `PRIME_FD_TO_HANDLE` for the import,
  and `PRIME_HANDLE_TO_FD` (the copy path) appears **zero** times - exactly
  mirrored by the flag-off control run.
- Measured A/B at 1920x1080x120 fps: guest busy CPU 2.800 s (46.7%) copy vs
  **2.650 s (44.2%)** fast, i.e. -5.4%, with per-frame cost 3888.9 -> 3680.6 us.
  The delta is the copy handler's `memcpy`; the proxy's cross-domain submit
  dominates either way, which is why the measurement was required.
- The Chromium GPU smoke still passes on the same tree (hardware venus), so the
  copy path and the venus renderer are undisturbed.

The A/B client is `tools/wl-shm-bench/` (validated against a host weston). What
blocked the first run was ticket 12's udmabuf `list_limit`; the two permission
gates (host node mode, guest devtmpfs node) are recorded with the harness.
