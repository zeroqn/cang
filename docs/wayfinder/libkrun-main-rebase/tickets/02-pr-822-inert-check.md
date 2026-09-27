---
label: wayfinder:research
title: Is cherry-picked PR 822 inert without the guest side?
status: closed
blocked_by: []
claimed_by: pi research child-2 (2026-09-27)
---

## Question

PR 822 ("virtio/gpu: implement CREATE_GUEST_HANDLE for zero-copy shared memory
buffers", `valpackett`, head `3819ce5`, +372/-148 over 36 files, base `main`,
**draft**, `mergeable=false`) is the VMM side of the guest-handle fast path
whose guest userland side we already merged into
`deps/wl-cross-domain-proxy` (upstream PR #24). It is unmerged and explicitly
WIP: its own description says the kernel-side API is not upstreamed yet.

Answer, from the PR's diff and this repo's pinned kernel:

1. Does the patched device **advertise** `VIRTIO_GPU_F_CREATE_GUEST_HANDLE`
   unconditionally, or behind a flag/config? Compare with today's tree
   (`deps/libkrun/src/devices/src/virtio/gpu/device.rs` advertises only
   VIRGL/EDID/RESOURCE_UUID/RESOURCE_BLOB/CONTEXT_INIT, and
   `virtio_gpu.rs:760` still `panic!`s if the blob flag is set).
2. With our pinned kernel (`libkrunfw` `v5.6.2-cang.1`, 6.12.109-hardened1) -
   which has no `VIRTGPU_PARAM` 10 / guest-handle virtio-gpu support - does any
   guest process ever set `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE`? If it
   cannot, is the cherry-pick therefore **inert**? If it can (a guest could set
   the flag without negotiation), what happens: the host device panics, returns
   an error, or handles it?
3. What exactly does the *guest* need for the fast path to engage
   (kernel param 10, `udmabuf` availability on the host and/or guest, the
   proxy's `has_create_guest_handle` probe), so that the map records the real
   prerequisite rather than a guess?
4. What is the **conflict surface** for cherry-picking it onto a rebased `main`
   (`mergeable=false`)? Which files and which upstream changes does it expect?
   Note the dates: the PR was created **2026-08-27** and last updated
   2026-08-27, i.e. *before* main's 2026-09-11 ABI rewrite (`a3d31822`), so
   establish whether its diff targets the pre-rewrite device code and what of it
   still applies to main's current `src/libkrun`/`src/devices` layout (see
   `notes/01-upstream-main-delta.md`).

## Deliverable

`notes/02-pr-822-inertness.md`: a verdict - *inert*, *needs a fork-local gate*,
or *unsafe without the guest side* - plus the evidence for each numbered item
and the list of conflicting files. This is the input to ticket 03.

## Resolution

**Resolved by a tool-capable research child (2026-09-27).** Deliverable:
`../notes/02-pr-822-inertness.md` plus eight raw evidence files in the same
directory (refs/diffstat, per-file status, per-commit cherry-pick matrices onto
`upstream/main` and onto the fork tip, composite `merge-tree` conflicts, the
payload patch `5849c08d..3819ce5f`, pinned-kernel/guest-requirement greps, and
kernel + crates.io primary-source extracts).

**Verdict: the fast path is inert on the pinned stack, the patch as published is
not.** Charting session independently verified the two load-bearing facts:

1. **The guest cannot ask.** With `libkrunfw v5.6.2-cang.1` the kernel rejects
   blob flag `0x8` in `verify_blob()` (outside `VIRTGPU_BLOB_FLAG_USE_MASK=0x7`)
   before it reaches the device, and `getparam` param 10 returns `-EINVAL`
   (v6.12.109 handles 1..8), so the proxy's `has_create_guest_handle` probe is
   false and it takes the copy path. Verified: `CONFIG_UDMABUF is not set` in
   every pinned config (`config-libkrunfw_x86_64{,-lto,-kvm,-kvm-lto}`,
   `_aarch64`, `_riscv64`).
2. **But applying it verbatim would break the guest.** PR commit `230f2c55`
   *"[XXX] virtio/gpu: update VIRTIO_GPU_F_CREATE_GUEST_HANDLE constant"* moves
   the constant **6 -> 5**; our pinned kernel's `patches/0018-drm-virtio-Support-fence-passing-feature.patch`
   defines `VIRTIO_GPU_F_FENCE_PASSING 5`. Applied as-is, the VMM would advertise
   bit 5 whenever the host has `/dev/udmabuf` and the guest kernel would ack
   fence passing. So the carry must **exclude `230f2c55`** (constant stays 6),
   and the fork-local gate is that exclusion.
3. **Not unsafe:** the PR replaces the (today-unreachable) `panic!` with
   `ErrUnspec` when `/dev/udmabuf` is absent, and validates region bounds,
   file-backedness and page alignment otherwise. Advertisement is a runtime
   condition (`Gpu::new` opens `/dev/udmabuf`), not a cargo/env knob.
4. **The rutabaga half cannot be cherry-picked at all.** `merge-base(pr-822,
   main)` = `0d75eb4b` (2026-08-26, pre-rewrite); two of the twelve commits are
   already in main, so the payload is the ten-commit `5849c08d..3819ce5f` diff
   (+372/-148, 36 files). Thirteen paths conflict compositely; the four
   `src/rutabaga_gfx` hunks are unrunnable because main deleted that crate and
   **crates.io `rutabaga_gfx 0.1.85` has no guest-blob-handle support** (no
   `RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE`; `CrossDomainContext::attach` still
   inserts `handle: None`, `component.rs:88-105`). The `builder.rs` hunk
   auto-merges but names `vm_resources.gpu_virgl_flags`, which main's rewritten
   builder no longer has (nearest signal `gpu_shm_size`, `builder.rs:723`) - a
   one-line port. On the fork tip the matrix differs (fork-specific divergence),
   while the rutabaga commits apply cleanly there.

**Prerequisites for the path to ever run** (recorded in the map's Out of scope):
libkrunfw must gain `CONFIG_UDMABUF=y` *and* the non-upstream virtio-gpu
param-10 + blob-flag-8 kernel patch; the host needs `/dev/udmabuf` with the seal
contract the PR expects. Ticket 08's GPU smoke cannot exercise this fast path on
the current pin.
