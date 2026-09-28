---
label: wayfinder:grilling
title: Decide the design, the gate and the feature-bit map the fork carries
status: closed
blocked_by: ["01-rutabaga-delta", "02-kernel-patch-set", "03-port-matrix-on-abi2", "10-udmabuf-ram-backing"]
claimed_by: pi+bob (2026-09-28)
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
- **The gate (ticket 10's answer, to confirm).** 822's whole-RAM file-backing
  cannot be narrowed (the path needs ordinary RAM, and a narrowed variant fails
  *silently*), so confirm: one opt-in bool resolved in `DeviceRequirements`,
  mirroring `process_shareable_memory` -> `use_vhost_user`, consumed by **both**
  `create_guest_memory` (`use_gpu_udmabuf`) and `Gpu::avail_features`, default
  **off** until ticket 09 proves engagement. Record explicitly that the balloon
  is inert on fast-path runs (a documented, accepted trade because the gate makes
  it opt-in) and name where the bool is turned on from cang.
- **The evidence design for ticket 09.** How the map will *prove* the guest took
  the fast path rather than the copy path (proxy log line, udmabuf counter,
  tracepoint, timing delta), and where that instrumentation goes.

## Deliverable

The decision recorded here, precise enough for tickets 05 and 07 to implement
without re-deciding, plus the observability plan ticket 09 executes.

## Resolution (2026-09-28, bob + pi, two grilling rounds)

**Route - the `ctx_id` route (bit 7), 822's component route rejected.** The guest
kernel passes the current ctx_id (`VIRTIO_GPU_F_BLOB_CTX_ID_FIX`, 7) and
magma-gpu main's already-merged `context_create_blob` Case 2 does the rest, so
**822's three rutabaga hunks are dropped, not carried**, and the dependency stays
a rev-pinned git dep on upstream main. Consequence to verify in ticket 09: the
guest's ctx must be the **CrossDomain** context or main falls through to the
default (VirglRenderer) component and silently drops the handle.

**Feature-bit map - frozen:**

| bit | value | owner |
|---|---|---|
| 5 | `VIRTIO_GPU_F_FENCE_PASSING` | cang's kernel (`patches/0018`) - **never renumber, never import upstream `BLOB_ALIGNMENT = 5`** |
| 6 | `VIRTIO_GPU_F_CREATE_GUEST_HANDLE` | upstream's number; the fork advertises it only when the gate is on |
| 7 | `VIRTIO_GPU_F_BLOB_CTX_ID_FIX` | upstream's number; the fork advertises it with 6 (the kernel gates `ctx_id` on it) |

The blob flag is a separate namespace: `0x0008`, already present in the fork's
`protocol.rs`; 822's `230f2c55` (6 -> 5) stays excluded.

**Gate - cang's `--zero-copy-shm`, default off.** A companion flag to
`--gpu=drm` (not a new `GpuMode` value; erroring if given without `--gpu=drm`).
One bool resolved in the fork's `DeviceRequirements`, mirroring how
`process_shareable_memory` becomes `use_vhost_user`, consumed by **both**
`create_guest_memory` (`use_gpu_udmabuf`) **and** `Gpu::avail_features`, so
withholding the bits and keeping anonymous RAM are the same decision. Default off
until ticket 09 proves engagement. **The balloon is inert on fast-path runs - an
accepted, documented trade** (project memory #20's reclaim is an anonymous-map
effect; the memfd path is `MAP_SHARED`). Plain `--gpu=drm` keeps anonymous RAM
and the reclaim.

**When the stack cannot serve it: warn and keep the copy path.** If
`/dev/udmabuf` is absent, the kernel cannot do it, or the proxy's probe fails,
cang logs one clear reason and the guest runs the copy path. The opt-in is a
request, not a guarantee - and the negotiation design already makes an unserving
host safe. Silent pass-through (822's own behaviour) is rejected: silence is how
ticket 10's wrong-pixels mode arises.

**Fork-side guard: log the silent drop, keep serving.** When a
`CREATE_GUEST_HANDLE` blob would fall through to a component that ignores handles
(its ctx is not CrossDomain), the fork warns with the `ctx_id`. Guest-visible
behaviour is unchanged; the failure becomes diagnosable.

**Evidence design (ticket 09):** the proxy's own zero-copy mode line, an observed
`udmabuf_create` for the shm pool, the negotiated bits on both sides, and **a
measured A/B delta** - a small guest-side `wl_shm` client blitting at a fixed rate
with `--zero-copy-shm` on and off (frames/s and guest CPU), since no wl_shm
client exists in the image today (weston and waypipe are host-side in the smoke).
Chromium's GPU smoke stays the regression backstop.
