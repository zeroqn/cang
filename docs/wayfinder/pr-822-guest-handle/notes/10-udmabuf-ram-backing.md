---
label: wayfinder:research
title: Does the udmabuf fast path need all guest RAM file-backed, or only the GPU shm window?
status: closed
blocked_by: []
claimed_by: pi research child-4 (2026-09-28)
---

# 10 - Does the udmabuf fast path need all guest RAM file-backed, or only the GPU shm window?

Ticket: `tickets/10-udmabuf-ram-backing.md`. Read-only on `deps/libkrun`,
`deps/libkrunfw` and `deps/wl-cross-domain-proxy`; no `git` mutation anywhere.
The only writes are this file and the two `10-raw-*` files beside it.

Sources read (identifiers are what I actually read, not a re-derivation):

| what | where |
|---|---|
| ABI-2 builder / guest-RAM creation | `deps/libkrun` @ `3d7af2c2`, `src/libkrun/src/vmm/builder.rs` |
| x86_64 guest-physical layout | `deps/libkrun/src/arch/src/x86_64/mod.rs` |
| shm windows | `deps/libkrun/src/libkrun/src/vmm/device_manager/shm.rs` |
| device requirements + GPU device | `deps/libkrun/src/libkrun/src/api/device_builders.rs` |
| PR 822 host payload | `../libkrun-main-rebase/notes/02-raw-pr822-gpu-payload.patch` |
| guest userland | `deps/wl-cross-domain-proxy` @ `cc64c65`, `src/source/channel/{wayland,mod}.rs`, `src/udmabuf.rs` |
| guest kernel (upstream, today) | `drivers/gpu/drm/virtio/virtgpu_prime.c`, `virtgpu_vq.c`, the 2026-09-11 revert on lkml (`10-raw-guest-kernel-prime-import.txt`) |
| vm-memory 0.18.0 | `~/.cargo/registry/src/.../vm-memory-0.18.0` (`10-raw-abi2-regions-and-mmap.txt`) |
| cang's GPU call site | `crates/cang/src/runtime/vm/libkrun/launcher.rs` |
| balloon | `deps/libkrun/src/devices/src/virtio/balloon/device.rs` |

## Verdict

**The fast path exports ordinary guest RAM, not the virtio-gpu shm window.**
The iovecs a `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE` blob carries are the
guest-physical addresses of the *imported dma-buf's* pages, and in cang that
dma-buf is the guest's own `wl_shm` pool memfd - i.e. tmpfs pages handed out by
the guest page allocator anywhere in main RAM. File-backing only
`shm_manager.regions()` therefore cannot satisfy `create_udmabuf`; the shm
window is a device BAR above the last RAM address, never in e820, and no blob's
`mem_entry` can name it.

Consequences:

* **The narrow variant the ticket hoped for does not exist.** "File-back only
  the shm region" breaks the fast path; "file-back only the RAM regions" (the
  shm windows left anonymous) is mechanically expressible on ABI 2 but saves
  nothing - the balloon's FRQ never covers the shm window, so the reclaim loss
  is entirely in the RAM regions that must stay file-backed.
* **A half-scoped file-backing fails silently, and wrongly.** With the flag set
  but a page in an anonymous region, `create_udmabuf` errors, PR 822's caller
  downgrades to `handle = None` with only a `warn!`, and the host still answers
  the guest's blob creation with success. The proxy has already committed to
  `ShmPoolHandler::ZeroCopy` from its *guest-kernel* probe and never copies on
  commit, so the guest renders a buffer the host never got.
* **Recommendation: the gate (item 4).** Carry 822's whole-RAM file-backing, but
  only under an explicit opt-in that *also* withholds
  `VIRTIO_GPU_F_CREATE_GUEST_HANDLE`, so a plain `--gpu` run keeps anonymous RAM
  and the balloon's `madvise(MADV_DONTNEED)` reclaim. Default it off until
  ticket 09 shows the fast path engaging (until then the file-backing is pure
  cost, because the pinned kernel cannot reach the arm at all).
* **Load-bearing for tickets 04/05:** the *guest kernel* must gate the blob flag
  (and the `VIRTGPU_PARAM` 10 probe the proxy reads) on the negotiated feature
  bit, not on the mere presence of the driver code. Otherwise withholding the
  host bit produces guest blobs with flag `0x8` that the host cannot serve -
  silent corruption instead of the copy path.

## 1. What udmabuf needs, and which regions the iovecs can land in

### 1a. The host-side requirement is `region.file_offset()`

`UdmabufDriver::create_udmabuf(mem, iovecs)` (PR 822
`src/utils/src/linux/udmabuf.rs`; quoted in full in the ticket-03 raw dump) walks
the guest iovecs and, per entry:

```rust
let region = mem.find_region(addr).ok_or(UdmabufError::RegionNotFound)?;
let Some(file_offset) = region.file_offset() else {
    return Err(UdmabufError::RegionNotFileBacked);
};
let map_offset = addr.checked_sub(region.start_addr().0).ok_or(OutOfBounds)?;
...
list.push(UdmabufCreateItem { memfd: file_offset.file().as_raw_fd(),
                              offset: file_offset.start() + map_offset.0, size: len });
```

So the *only* thing the host needs from guest RAM is: every page a blob names
must live in a `GuestMemoryMmap` region that vm-memory constructed with a
`FileOffset`. `OutOfBounds`/`NotPageAligned` are per-blob validation, not
region properties; `RegionNotFound` cannot happen for a well-formed iovec.

### 1b. Where the iovecs come from: the imported dma-buf's sg list

The command's `nr_entries` + `virtio_gpu_mem_entry` array is parsed by the
device's worker for every `ResourceCreateBlob`
(`.../gpu/worker.rs:373-405`) and handed to
`VirtioGpu::resource_create_blob(..., vecs, mem)`
(`.../gpu/virtio_gpu.rs:866`, currently `panic!("GUEST_HANDLE unimplemented")`
at `:876`). On the guest side, those entries are what the *kernel* fills in when
the proxy does `prime_fd_to_buffer(dma_fd)` (upstream
`virtgpu_gem_prime_import()` -> `virtgpu_dma_buf_init_obj()` ->
`virtgpu_dma_buf_import_sgt()`, master `virtgpu_prime.c`):

```c
sgt = dma_buf_map_attachment(attach, DMA_BIDIRECTIONAL);
*nents = sgt->nents;
for_each_sgtable_dma_sg(sgt, sl, i)
        (*ents)[i].addr = cpu_to_le64(sg_dma_address(sl));
...
params.blob_mem = VIRTGPU_BLOB_MEM_GUEST;
params.blob_flags = VIRTGPU_BLOB_FLAG_USE_SHAREABLE;   /* re-send: + CREATE_GUEST_HANDLE */
virtio_gpu_cmd_resource_create_blob(vgdev, bo, &params, ents, nents);
```

and `virtio_gpu_cmd_resource_create_blob()` sends `nr_entries` plus the
`ents` array in the `VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB` payload. With cang's
IOMMU-less virtio transports `sg_dma_address()` is the guest physical address,
so the host's `(GuestAddress, usize)` pairs are guest PFNs of the dma-buf's pages.

### 1c. That dma-buf is the guest's `wl_shm` pool, i.e. guest tmpfs

In `deps/wl-cross-domain-proxy` the fast path is entered from
`handle_shm_create_pool` when `host_features.has_create_guest_handle` is true
(`wayland.rs:375`), via `import_memfd_via_udmabuf(drm, &self.udmabuf, &orig_fd,
size, page_size)` (`wayland.rs:292-341`): the `orig_fd` is the Wayland client's
`wl_shm` pool fd, which it seals (`F_SEAL_SHRINK`, refuses
`F_SEAL_WRITE`/`FUTURE_WRITE`), wraps in a *guest* `/dev/udmabuf`
(`src/udmabuf.rs:49`), and imports with `prime_fd_to_buffer` (`:335`). Its pages
are guest kernel shmem (tmpfs) pages - allocated on fault by the guest's page
allocator from the ordinary guest RAM ranges.

The two other blob-creating paths in the proxy are not the fast path:
`Ring::new` (`source/channel/mod.rs:204`) creates a one-page
`BlobMem::Guest | USE_MAPPABLE` ring blob for the cross-domain channel, and
`create_sharable_blob` (`wayland.rs:241`) creates a `HOST3D` blob when the
memfd path is unavailable. The proxy defines
`BlobFlags::CREATE_GUEST_HANDLE = 8` (`virtio_gpu/mod.rs:81`) but never passes
it; the flag originates in the guest kernel, exactly as ticket 02 recorded.

### 1d. The shm window is not RAM, so it cannot be the answer

`ShmManager::new` starts allocating at `ArchMemoryInfo::shm_start_addr`
(`shm.rs:35`), and on x86_64 that address is derived from `ram_last_addr`
(`arch/src/x86_64/mod.rs:110-199`), i.e. it begins *past the end of RAM*. For
cang's default `--mem 4` (4 GiB = `0x1_0000_0000`): with the 768 MiB 32-bit
MMIO gap (`layout.rs:81`, `MMIO_MEM_START = 0xD000_0000`) RAM is
`(0, 0xD000_0000)` + `(0x1_0000_0000, 0x3000_0000)`, `ram_last_addr` is
`0x1_3000_0000`, and the GPU shm window lands at
`0x1_4000_0000` (cang asks for 256 MiB,
`launcher.rs:47` -> `krun_gpu_device_new(..., GPU_SHM_SIZE_BYTES, ...)`).

That window is a virtio shared-memory PCI BAR. It is a `GuestMemoryMmap` region
on the host (so PR 822 would file-back it too) but it is not in the guest's
e820 map, so `alloc_pages` can never return its PFNs and the guest can only
`mmap` it through the DRM node; `virtgpu_map` puts *host3d* blobs there. No
guest blob's `mem_entry` (a dma-buf page's guest-physical address) can point
into it.

**Therefore: on cang's topology the exported pages live in the RAM regions - the
same regions the balloon reclaims from.** Not the shm window.

## 2. What breaks if only the GPU shm region (or any subset without RAM) is file-backed

Call path, in order:

1. Guest: proxy `import_memfd_via_udmabuf` computes `dma_fd`, then
   `prime_fd_to_buffer` -> guest kernel creates the guest blob with the
   imported dma-buf's sg list and gets `OK`.
2. Guest: `ShmPoolHandler::ZeroCopy` is installed and `handle_commit`
   (`wayland.rs:94-110`) returns `Ok(())` without copying - the proxy has
   *already* stopped copying.
3. Host: the virtio-gpu device parses the mem_entries and calls
   `create_udmabuf(mem, &vecs)`; the first entry in an anonymous region returns
   `Err(RegionNotFileBacked)`, so the whole list is abandoned.
4. Host: PR 822's `resource_create_blob` arm does
   `.inspect_err(|err| warn!("Failed to create udmabuf: {err}")).ok()` and passes
   `handle = None` to `rutabaga.resource_create_blob(..., handle)?` - which
   **succeeds** (the `ErrUnspec` in that arm is only for *no udmabuf driver at
   all*, not for a non-file-backed page).
5. Guest: blob creation succeeded, the pool is zero-copy, and the host has a
   guest blob with iovecs but no dma-buf handle. The `a8fd2784`/`3391f3c5`
   rutabaga changes that make `attach()` keep a guest blob's handle have nothing
   to keep, so the cross-domain export/attach that the compositor needs cannot
   happen.

Net effect: **silent wrong pixels on the compositor plus a persistent
zero-copy/copy mismatch**, diagnosed only by a host-side `warn!`. There is no
clean `RegionNotFileBacked` in the guest-visible path, and no fallback to the
copy path, because the proxy decided zero-copy from the guest kernel alone.

That is the strongest argument for making the *feature bit* the single gate
(§4): a host that cannot serve the udmabuf must never negotiate the feature, so
the guest never leaves the copy path.

## 3. Is a narrower change possible on ABI 2?

**Mechanically, yes; usefully, no.**

* ABI 2 already creates **one memfd per region** in the vhost-user branch
  (`builder.rs:1938-1997`: `arch_mem_regions.iter().map(|(addr, size)| ...)`),
  and vm-memory 0.18 `GuestMemoryMmap::from_ranges_with_files` takes an
  `Option<FileOffset>` *per region* (`mmap/mod.rs:216`). PR 822's ported
  condition `use_vhost_user || use_gpu_udmabuf` would rewrite that branch to
  file-back every region (RAM *and* `shm_manager.regions()`, appended at
  `builder.rs:1941`). Leaving the shm windows anonymous is a two-line change;
  leaving any RAM region anonymous is not an option (§1, §2).
* What is left to gain? Nothing on the RAM side, and nothing on the window side:
  the balloon's free-page-reporting queue is fed by the guest balloon driver,
  which can only offer pages it owns (e820 RAM), so the shm window is never
  ballooned. A memfd-backed shm window has no balloon cost *and* no balloon
  benefit.
* The balloon semantics are unchanged by the seals. `a3962256` adds
  `MFD_ALLOW_SEALING` + `F_SEAL_GROW|F_SEAL_SHRINK`, which is exactly what
  `drivers/dma-buf/udmabuf.c` wants (`SEALS_WANTED (F_SEAL_SHRINK)`,
  `SEALS_DENIED (F_SEAL_WRITE|F_SEAL_FUTURE_WRITE)`); seals do not block
  `madvise`. The reclaim loss is the **`MAP_SHARED`** mapping itself:
  `MmapRegion::new()` is `MAP_ANONYMOUS|MAP_NORESERVE|MAP_PRIVATE`
  (`unix.rs:234-244`) while `MmapRegion::from_file()` is
  `MAP_NORESERVE|MAP_SHARED` (`:249-259`). `process_frq` still calls
  `MADV_DONTNEED` (`balloon/device.rs:100`), which over a shared file mapping
  drops the VMM's RSS but leaves the pages resident in the memfd page cache;
  with no host swap (zram only) those pages are unevictable, so ticket 03's
  measured `RssAnon 3563 -> 585 MiB` (an anonymous-map effect, project memory
  #20) does not carry over. Ranges that are currently exported to a live
  udmabuf are additionally pinned by the driver's sg table, but that is a
  subset of the fast-path case.
* **Idea worth recording, not recommending:** keep the file-backing and make
  the balloon reclaim again by having `process_frq` *punch holes*
  (`FALLOC_FL_PUNCH_HOLE|FALLOC_FL_KEEP_SIZE`) in the region's memfd over the
  reported-free ranges instead of / in addition to `MADV_DONTNEED`. `F_SEAL_SHRINK`
  does not forbid hole-punching and shmem hole-punch frees page-cache pages, so
  in principle the host gets the memory back. But a dma-buf's pinned pages are
  exactly the pages the guest is handing to another process, and FRQ is a
  best-effort "these are free" hint; getting the lifetime wrong is silent data
  corruption. That is its own ticket if ticket 04 ever wants it.

## 4. Or is the gate the answer?

**Yes - and it must gate the feature bit, not only the RAM backing.**

* What the gate keys on: an explicit, per-run opt-in for cang's GPU path. cang
  already resolves `--gpu=drm` into
  `krun_gpu_device_new(virgl_flags, GPU_SHM_SIZE_BYTES, render_server_fd)`
  (`launcher.rs:294-317`), so the natural shape is one more cang-level field
  (CLI flag and/or `CANG_*` env, like the existing `CANG_RENDER_SERVER_FD`
  convention) carried into the fork.
* Where it must land on ABI 2: **`DeviceRequirements` is the seam that already
  exists.** It carries `shm_size`, `gpu_shm` and `process_shareable_memory`
  (`api/device_builders.rs:49-58`), and `build_microvm` derives the guest-RAM
  decision from it:
  `let use_vhost_user = requirements.iter().any(|r| r.process_shareable_memory);`
  (`builder.rs:727-729`), then passes `gpu_shm_size` and `use_vhost_user` into
  `create_guest_memory` (`:768`, `:1835`). Adding `gpu_udmabuf: bool` and
  `let use_gpu_udmabuf = requirements.iter().any(|r| r.gpu_udmabuf);` mirrors
  that exactly; `GpuDevice::requirements()` (`:1683-1690`) sets it from
  `gate && UdmabufDriver::new().is_ok()`.
* **Why this matters rather than "just probe /dev/udmabuf twice":** PR 822
  decides the two halves in two different places - the *bit* inside `Gpu::new`
  (`device.rs`, from an `open("/dev/udmabuf")` probe at attach time) and the
  *RAM backing* inside `create_guest_memory` (from `gpu_shm_size.is_some()`,
  which is true for every `--gpu` run). As ported, a plain `--gpu` run would
  pay the memfd cost even on hosts where the probe fails and no bit is
  advertised. Resolving one bool in `DeviceRequirements` and reading it in both
  places fixes the mismatch.
* **Can the advertisement follow it?** Yes, trivially - the PR already writes
  `avail_features | if udmabuf_driver.is_some() { 1 << VIRTIO_GPU_F_CREATE_GUEST_HANDLE } else { 0 }`;
  replace the probe with the shared bool (still ignoring the bit when the
  device has no driver to hand the handle to).
* **Default:** off (or at least: not until ticket 09 proves engagement). On the
  current pin the arm is unreachable (ticket 02), so an on-by-default gate buys
  nothing and costs the balloon on every `--gpu` run; and even after the kernel
  lands, the trade is workload-dependent (a wayland/SHM-heavy session wants
  zero copy; a memory-hungry build wants reclaim), so the knob should stay.
* **Ticket 05 requirement this creates.** The guest kernel patch must derive both
  `VIRTGPU_PARAM` 10 (what the proxy probes, `source/channel/mod.rs:93-96`) and
  the blob flag `0x8` from the negotiated
  `VIRTIO_GPU_F_CREATE_GUEST_HANDLE` feature, not from the presence of its code.
  Otherwise a host that withholds the bit still gets flag-`0x8` blobs it cannot
  serve (§2's silent-corruption path). Upstream's re-send is described as
  feature-gated, but the fork's patch must be read for this before it is
  tagged.
* **Project-memory consequence:** with the gate on, the virtio-balloon is
  effectively inert for main RAM (project memory #20 must say so, and no cang
  default should assume reclaim on a `--gpu` run with the fast path enabled).

## Recommendation for ticket 04

1. Carry PR 822's RAM file-backing as written (**all** `arch_mem_regions`,
   including the shm windows) - narrowing it is either impossible (RAM) or
   pointless (shm window).
2. Make the file-backing decision and the feature advertisement one bool,
   resolved in `DeviceRequirements` from an explicit cang-side opt-in and the
   `/dev/udmabuf` probe; `create_guest_memory` consumes it as
   `use_gpu_udmabuf`, `Gpu`'s `avail_features` consumes the same value.
3. Default the opt-in off; flip it (per run) only once ticket 09 shows the fast
   path engaging end to end.
4. Record the balloon trade explicitly (guest RAM becomes `MAP_SHARED` memfd;
   FRQ `MADV_DONTNEED` no longer returns host memory) and do not rely on
   reclaim in fast-path runs. Do not attempt the balloon hole-punch without its
   own ticket.
5. Pass the "kernel gates on the feature, not on the code" requirement to
   tickets 05 and 07.

## Limits / confidence

* Everything about the host side (`builder.rs`, `arch`, `shm.rs`, vm-memory,
  the balloon, the proxy, cang's launcher) is a source read of the pinned tree,
  not a live run: no cang VM was booted for this ticket, and on the current pin
  the fast path is unreachable anyway (ticket 02).
* The guest-kernel half is read from upstream master's reverted-and-to-be-re-sent
  `virtgpu_prime.c` (raw dump beside this note), not from a tree cang can boot;
  the pinned 6.12.109 kernel has no attempt at `virtgpu_gem_prime_import`
  (ticket 02). The exact blob flags the re-send will set (today's merge used
  `USE_SHAREABLE` alone) are still moving, and the host arm keys on
  `VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE`; ticket 05 must reconcile that.
  The guest-side claim that the entries are guest-physical addresses assumes no
  IOMMU translation on the virtio-gpu device (cang's transports are plain
  virtio-pci/mmio, no virtio-iommu), which is inherent to the design.
* "Guest tmpfs pages are allocated from the RAM regions" is an e820/allocator
  argument plus the guest kernel's `alloc_pages` behaviour, not a live
  measurement.

## Raw evidence in this directory

| file | contents |
|---|---|
| `10-raw-guest-kernel-prime-import.txt` | master `virtgpu_prime.c` `virtgpu_dma_buf_import_sgt()`/`_init_obj()`/`virtgpu_gem_prime_import()`, `virtgpu_vq.c` `virtio_gpu_cmd_resource_create_blob()` (nr_entries + mem_entry array), and the 2026-09-11 revert body + `Fixes: df4dc947c46b` diff |
| `10-raw-abi2-regions-and-mmap.txt` | `builder.rs:1938-1997` memfd-per-region loop, x86_64 `shm_start_addr`, `ShmManager`, vm-memory 0.18 `from_ranges_with_files` + mmap flags, balloon `process_frq`, cang's 256 MiB shm size |
