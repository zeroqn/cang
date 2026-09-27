---
label: wayfinder:research
title: Is cherry-picked PR 822 inert without the guest side?
status: open
blocked_by: []
claimed_by: unclaimed
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

## Deliverable

`notes/02-pr-822-inertness.md`: a verdict - *inert*, *needs a fork-local gate*,
or *unsafe without the guest side* - plus the evidence for each numbered item
and the list of conflicting files. This is the input to ticket 03.
