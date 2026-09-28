---
label: wayfinder:research
title: What does PR 822's payload look like on the current ABI-2 fork tip?
status: open
blocked_by: []
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
