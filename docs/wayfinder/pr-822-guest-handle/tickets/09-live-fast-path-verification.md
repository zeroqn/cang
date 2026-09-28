---
label: wayfinder:task
title: Show a cang guest taking the zero-copy guest-handle fast path
status: open
blocked_by: ["06-libkrunfw-release-and-pin", "08-fork-release-and-pin"]
---

## Question

Execute ticket 04's evidence design on the fully pinned stack: a cang guest,
running a Wayland `wl_shm` client through `deps/wl-cross-domain-proxy` on the GPU
path, must demonstrably take `CREATE_GUEST_HANDLE` rather than the memfd copy
path.

Record: host `/dev/udmabuf` and its seal contract, the guest's
`/dev/udmabuf`, the proxy's feature probe result, the udmabuf handles observed,
and the guest-side proof that no copy happened. Say explicitly which leg could
not be shown here, and whether the chromium GPU smoke still passes (regression
backstop: the previous map's venus regression was only caught by the smoke).

## Deliverable

`notes/09-live-fast-path-verification.md` with the run transcript and the verdict
- the fast path engages, or the specific gate that still blocks it.
