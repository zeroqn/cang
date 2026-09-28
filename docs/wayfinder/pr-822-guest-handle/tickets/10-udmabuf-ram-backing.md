---
label: wayfinder:research
title: Does the udmabuf fast path need all guest RAM file-backed, or only the GPU shm window?
status: open
blocked_by: []
claimed_by: pi research child-4 (2026-09-28)
---

## Question

Ticket 03 found that PR 822's `4ef22a14` makes `create_guest_memory` file-backed
for **every** region when a GPU device is present (`use_vhost_user ||
use_gpu_udmabuf`), and vm-memory maps file-backed ranges `MAP_SHARED`. On the
ABI-2 builder, "a GPU device is present" is `gpu_shm_size.is_some()`, which is
true for every `--gpu` run.

That trades away cang's balloon reclaim: the balloon's `MADV_DONTNEED` drops the
VMM's RSS but the pages stay resident in the sealed memfd page cache, and this
host has no swap (zram only). Project memory #20's measured `RssAnon 3563 -> 585
MiB` reclaim is an anonymous-map effect that does not survive the change.

Answer:

1. **Does udmabuf actually need the whole of guest RAM file-backed?** Read
   `UdmabufDriver::create_udmabuf` (`src/utils/src/linux/udmabuf.rs` in the PR
   payload) and the PR's `virtio_gpu.rs` caller. It needs
   `guest_mem_region.file_offset()` for each page range it exports. Which
   regions can a `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE` blob's iovecs
   actually land in on cang's topology - only the virtio-gpu shm window
   (`shm_manager.regions()`, `gpu_shm_size`), or arbitrary guest RAM?
2. **What breaks if only the GPU shm region is file-backed?** Enumerate the
   call paths that would then fail (`RegionNotFileBacked`), and say whether the
   guest's `wl_shm` pool pages (the actual fast-path payload in cang) live in the
   shm window or in ordinary RAM. Check what `deps/wl-cross-domain-proxy` does
   with the blob it imports.
3. **Is a narrower change possible on ABI 2** - file-back only the shm regions,
   or only when the shm window is created - and does the balloon still work over
   that window (the balloon FRQ covers *all* guest RAM, so a memfd-backed shm
   window would only lose reclaim for the window itself)?
4. **Or is a gate the answer** - keep 822's whole-RAM behaviour but only when
   the fast path is explicitly requested, so a plain `--gpu` run keeps anonymous
   RAM and the balloon reclaim? Say what the gate would key on and whether the
   udmabuf feature advertisement can follow it.

## Deliverable

`notes/10-udmabuf-ram-backing.md`: the answer per item, with the primary-source
reads (libkrun, vm-memory, udmabuf.c, the proxy) cited, and a recommendation the
ticket 04 grilling can adopt or reject.
