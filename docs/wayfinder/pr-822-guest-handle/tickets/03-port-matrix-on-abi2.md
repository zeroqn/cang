---
label: wayfinder:research
title: What does PR 822's payload look like on the current ABI-2 fork tip?
status: closed
blocked_by: []
claimed_by: pi research child-3 (2026-09-28)
---

## Question

PR 822's head `3819ce5` (2026-08-27) is based on `0d75eb4b`, i.e. **before**
upstream's 2026-09-11 ABI-2 rewrite, and the fork has since rebased onto ABI 2
(`deps/libkrun` HEAD `3d7af2c2` = `v2.0.0-cang.3`). The old matrix
(`../libkrun-main-rebase/notes/02-raw-pr822-cherrypick-matrix*.txt`) was taken
against the *old* fork tip, so it must be redone.

1. Re-run the per-commit 3-way cherry-pick matrix for the ten-commit payload
   (`5849c08d..3819ce5f`, saved as
   `../libkrun-main-rebase/notes/02-raw-pr822-gpu-payload.patch`) onto **today's**
   `cang` tip, in a scratch clone - never in the `deps/libkrun` worktree. Which
   commits apply, which conflict, and in which hunks.
2. Interaction with the fork's own ABI-2 GPU work: the fork re-added
   `krun_gpu_device_set_render_server_fd` (`RutabagaBuilder::set_server_descriptor`),
   fence retirement, and the venus/DRM-capset fix (`d578e4e2`, `29312733`,
   `3d7af2c2`). Name the collisions in
   `src/devices/src/virtio/gpu/{device,worker,virtio_gpu}.rs` and in the
   `VIRTIO_GPU_F_*` constants.
3. The two **memfd-backed guest RAM** commits (`4ef22a14` "create file-backed
   memory when a GPU device is present as well", `a3962256`
   `MFD_ALLOW_SEALING` + `F_ADD_SEALS(F_SEAL_GROW|F_SEAL_SHRINK)`) touch
   `src/libkrun/src/vmm/builder.rs`. That file is ABI-2 rewritten and is where
   cang's guest RAM is configured (`--mem`, a lazy `MAP_NORESERVE|MAP_PRIVATE`
   mmap, the virtio-balloon registered last in the MMIO manager). Say exactly
   what those commits require of the ABI-2 builder, what the correct condition
   is on today's builder, and whether the balloon's free-page-reporting
   (`madvise MADV_DONTNEED`) still works over a sealed memfd.
4. The constant commit `230f2c55` (bit 6 -> 5) must be excluded or adapted; say
   which value the remaining commits assume.

## Deliverable

`notes/03-port-matrix-on-abi2.md` plus the raw matrix dumps in the same directory.

## Resolution (2026-09-28, pi research child-3)

**The payload is not a cherry-pickable set on ABI 2 - it is a re-derivation.**
Of the ten commits, **five apply** (`4ef22a14`, `a3962256`, `b3f9c114`,
`230f2c55`, `3819ce5f`), **two of those do not compile or are wrong in
isolation**, **four have no file to patch** (`src/rutabaga_gfx` was dropped in
the ABI-2 rewrite), and one (`6c51645c`) is a real content conflict.

- **Matrix** (onto `3d7af2c2`, scratch clone): the four rutabaga commits
  (`a8fd2784`, `ec9e5562`, `3391f3c5`, `16e116ae`) all conflict as
  *modify/delete*; `6c51645c` conflicts in `virtio_gpu.rs` (3 hunks, all
  `use`/cfg, not logic); `4ef22a14` applies textually but inserts
  `vm_resources.gpu_virgl_flags`, which does not exist on ABI 2; `a3962256` and
  `b3f9c114` are clean; `230f2c55` must be dropped; `3819ce5f` applies cleanly
  only because it depends on `6c51645c`'s new field.
- **Fork collisions** are confined to `virtio_gpu.rs`'s import block (the fork
  already imports `IntoRawFd`/`OwnedFd` and `RutabagaDescriptor` from its own
  `29312733`), plus a semantic merge in `device.rs`/`worker.rs` where the PR adds
  `udmabuf_driver` and the fork adds `render_server_fd` - both must survive in
  the merged constructor. The rename
  `RUTABAGA_MEM_HANDLE_TYPE_DMABUF` -> `RUTABAGA_HANDLE_TYPE_MEM_DMABUF` and its
  `virgl_resource_map2` fallback make the PR's import hunk unusable verbatim.
- **Correct ABI-2 condition for `4ef22a14`:** `gpu_shm_size.is_some()`
  (`create_guest_memory` receives `gpu_shm_size`, not `&VmResources`; every
  `GpuDevice::requirements()` sets `gpu_shm: Some(DEFAULT_SHM_SIZE)`), so any
  `--gpu` run takes the file-backed branch.
- **The constant stays 6.** The ABI-2 tip already has
  `VIRTIO_GPU_F_RESOURCE_SYNC = 5` and `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`;
  `230f2c55` collides with both it and cang's kernel's
  `VIRTIO_GPU_F_FENCE_PASSING = 5`. The remaining commits use the symbolic
  constant and the separate `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE = 0x0008`
  (`protocol.rs:86`, already present).
- **New finding - the fast path changes all guest RAM.** `create_guest_memory`
  chooses file-backed memory for *every* region when `use_vhost_user ||
  use_gpu_udmabuf`, and vm-memory maps file-backed ranges `MAP_SHARED`. So with a
  GPU device present the main RAM stops being anonymous and becomes a sealed
  memfd. The balloon's `MADV_DONTNEED` still drops the VMM's RSS but the pages
  stay in the memfd page cache, and with no host swap they are unevictable -
  cang's measured `RssAnon 3563 -> 585 MiB` reclaim is an anonymous-map effect
  that **does not carry over**. Filed as ticket **10**; it is an input to the
  design decision in ticket 04.

Evidence: `../notes/03-port-matrix-on-abi2.md` and nine `03-raw-*.txt` files
beside it. Limits: the matrix is git-level; the "does not compile" claims are
read off symbol absence, not a `cargo check`.
