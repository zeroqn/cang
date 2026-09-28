---
label: wayfinder:grilling
title: Decide the design, the gate and the feature-bit map the fork carries
status: open
blocked_by: ["01-rutabaga-delta", "02-kernel-patch-set", "03-port-matrix-on-abi2", "10-udmabuf-ram-backing"]
---

## Question

With tickets 01-03 in hand, decide the shape ticket 07 implements:

- **Which design is carried.** (a) **822 as written** - the guest kernel does
  *not* pass `ctx_id` for a PRIME import, so `rutabaga_core` must route
  `ctx_id == 0` + `CREATE_GUEST_HANDLE` to the CrossDomain **component**, and the
  fork patches that into the git dependency. (b) **Upstream's newer route** -
  the guest passes `ctx_id` (`BLOB_CTX_ID_FIX`, bit 7), which needs that kernel
  behaviour but rides magma-gpu main's already-merged context path, with no
  component patch at all.
- **The advertisement gate.** The PR advertises the feature purely on a runtime
  `/dev/udmabuf` probe. Decide whether the fork keeps that, or adds a cang-side
  gate (cargo feature / env / config) so no cang build reaches the path by
  accident - and say who can turn it on.
- **The feature-bit map.** `VIRTIO_GPU_F_FENCE_PASSING = 5` (cang's kernel),
  `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`, `VIRTIO_GPU_F_BLOB_CTX_ID_FIX = 7`,
  and main's still-present `VIRTIO_GPU_F_RESOURCE_SYNC = 5`. State the exact
  values the fork advertises and how they are kept from colliding, both in
  libkrun's `src/devices/src/virtio/gpu/mod.rs` and in
  `deps/libkrunfw/patches/0018`.
- **What happens to the balloon.** Ticket 03 showed 822's memfd-backed RAM
  turns every guest region `MAP_SHARED` whenever a GPU is present, which costs
  cang's `MADV_DONTNEED` host-memory reclaim. Take ticket 10's answer: carry it
  as-is, narrow the file-backing, or gate the whole fast path behind an opt-in
  so a plain `--gpu` run keeps anonymous RAM.
- **The evidence design for ticket 09.** How the map will *prove* the guest took
  the fast path rather than the copy path (proxy log line, udmabuf counter,
  tracepoint, timing delta), and where that instrumentation goes.

## Deliverable

The decision recorded here, precise enough for tickets 05 and 07 to implement
without re-deciding, plus the observability plan ticket 09 executes.
