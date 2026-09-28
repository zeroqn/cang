---
label: wayfinder:grilling
title: Backport the PRIME-import prerequisite onto 6.12.109, or move the libkrunfw kernel base to 6.14+?
status: open
blocked_by: []
---

## Question

Ticket 02 found that `CREATE_GUEST_HANDLE` rests on Vivek Kasireddy's
`[PATCH v5 0/5] drm/virtio: Import scanout buffers from other devices`, which
first shipped in **v6.14** and is absent from the pinned `linux-6.12.109`. Two
ways to get it, and they are not equivalent. Verified against the kernel trees
(2026-09-28):

| line | `virtgpu_dma_buf_init_obj()` (the prerequisite) | import gate | `VIRTIO_GPU_F_*` bits |
|---|---|---|---|
| v6.12.109 / v6.12.111 | **absent** | plain `drm_gem_prime_import()` | 0..4 |
| v6.13 | **absent** | plain | 0..4 |
| v6.14 - v6.18 | **present** | `!has_resource_blob \|\| has_virgl_3d` (the **reverted** form) | 0..4 |
| torvalds HEAD | present | reverted | + `BLOB_ALIGNMENT = 5` |

- A **bump does not remove the unlock work**: every stable line from 6.14 to
  6.18 still refuses the import when `has_virgl_3d`, so the conditional
  `!has_create_guest_handle` form still has to be authored (ticket 02, piece 9).
- A stable bump does **not** collide with cang's `VIRTIO_GPU_F_FENCE_PASSING = 5`
  (stable is still bits 0..4); only torvalds HEAD has taken 5 for
  `BLOB_ALIGNMENT`. So the collision is a future-bump concern, not a 6.14/6.18 one.
- 6.12.y will never get it (a feature, not a fix) - `v6.12.111` is as empty as
  `v6.12.109`.
- `linux-hardened` publishes for both candidate bases:
  `v6.12.111-hardened1` and `v6.18.53-hardened1` (2026-09-23).

**Option A - backport onto 6.12.109.** Six patches of nontrivial virtio-gpu
(four of six hunks apply clean, `virtgpu_plane.c` re-authored; ticket 02's dry
runs). Keeps linux-hardened's `v6.12.109-hardened1`, the existing 36 kernel
patches, the LTO/KVM seeds and the whole release pipeline untouched.

**Option B - move the base to 6.14+ (e.g. 6.18.53).** The prerequisite comes for
free and stays maintained. Costs a libkrunfw rebase: a new
`linux-hardened` pin, regenerated `-lto`/`-kvm` seeds from the new plain config,
re-verification of all 36 fork patches (including `0018`'s `FENCE_PASSING = 5`),
the config deltas (landlock, nftables/TPROXY, zram LZO), per-arch CI, and the
hours-long `-lto` build bob has to schedule.

Decide which, with the cost accepted explicitly. This blocks ticket 05, and note
that the *route* decision in ticket 04 is independent of it: the `ctx_id` route
needs bit 7 either way.

## Deliverable

The decision recorded here, with the base (if any) and what it moves.
