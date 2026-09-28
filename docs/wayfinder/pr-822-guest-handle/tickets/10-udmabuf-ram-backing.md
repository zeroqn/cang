---
label: wayfinder:research
title: Does the udmabuf fast path need all guest RAM file-backed, or only the GPU shm window?
status: closed
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

## Resolution (2026-09-28, pi research child-4)

**The fast path needs ordinary guest RAM, not the GPU shm window - so the
file-backing cannot be narrowed. It must be gated instead.** Deliverable:
`../notes/10-udmabuf-ram-backing.md` + `10-raw-guest-kernel-prime-import.txt`,
`10-raw-abi2-regions-and-mmap.txt`.

- **Where the blob's pages live.** The `mem_entry` array a
  `CREATE_GUEST_HANDLE` blob carries is the *imported dma-buf's* sg addresses
  (`virtgpu_dma_buf_import_sgt()` -> `sg_dma_address()` ->
  `virtio_gpu_cmd_resource_create_blob`). In cang that dma-buf is the proxy's
  guest `/dev/udmabuf` over the `wl_shm` pool memfd
  (`deps/wl-cross-domain-proxy` `wayland.rs:292-341`, `prime_fd_to_buffer` at
  `:335`) - i.e. **guest tmpfs pages, allocated anywhere in main RAM**.
- **The shm window cannot hold them.** `ShmManager` starts at `shm_start_addr`,
  derived *above* `ram_last_addr`; at `--mem 4` it is `0x1_4000_0000`, a 256 MiB
  device BAR that is never in e820, so no `mem_entry` can name it.
- **Narrowing to the shm window fails *silently*:** `create_udmabuf` returns
  `RegionNotFileBacked`, 822's arm logs a warning and passes `handle = None`, the
  blob-create still *succeeds* - but the proxy has already committed to the
  zero-copy mode and stopped copying (`wayland.rs:94-110`): wrong pixels, no
  fallback. **This is the strongest argument for gating on the feature bit: a
  host that cannot serve must never negotiate it.**
- **The reclaim loss is `MAP_SHARED`, not the seals.** A narrower change on
  ABI 2 is mechanically possible (one memfd per region;
  `from_ranges_with_files` takes `Option<FileOffset>` per region) but usefully
  pointless: excluding the shm windows saves nothing (the balloon's FRQ never
  covers them) and excluding RAM breaks the path.
- **Recommendation (the gate):** carry 822's whole-RAM file-backing, but only
  under **one explicit opt-in bool** - resolved in `DeviceRequirements`,
  mirroring how `process_shareable_memory` becomes `use_vhost_user`
  (`builder.rs:727`/`768`), consumed by *both* `create_guest_memory`
  (`use_gpu_udmabuf`) **and** `Gpu::avail_features` - so withholding the bit and
  keeping anonymous RAM are the same decision. Default **off** until ticket 09
  proves engagement (until then the file-backing is pure cost, since the pinned
  kernel cannot reach the arm). Document the balloon as inert on fast-path runs.
- **Load-bearing for tickets 04/05:** the *guest kernel* must gate the blob flag
  (and the `VIRTGPU_PARAM` 10 probe the proxy reads) on the negotiated feature
  bit, not on the presence of the driver code - otherwise withholding the host
  bit produces guest blobs with flag `0x8` the host cannot serve, i.e. silent
  corruption instead of the copy path.
- **Limits:** no live VM; the guest-kernel half was read from upstream master +
  the 2026-09-11 revert, not a bootable tree, and the re-send's exact blob flags
  are still moving.
