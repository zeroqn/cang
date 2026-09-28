# 03 - What does PR 822's payload look like on the current ABI-2 fork tip?

Ticket: `tickets/03-port-matrix-on-abi2.md`.
Redone 2026-09-28 against the ABI-2 fork tip `3d7af2c2` (`v2.0.0-cang.3`,
branch `cang-main-rebase`, == `deps/libkrun` submodule HEAD), not the old
`28e79624`/`cang`-pre-rebase tip the first matrix used.

Method: scratch clone `git clone --local --shared deps/libkrun` under `$SCRATCH`
(never the submodule worktree). Per-commit `git cherry-pick -x <sha>` onto a
branch at `3d7af2c2`, record rc + conflict paths, `--abort`; plus one forced-base
composite `git merge-tree --write-tree --merge-base=5849c08d 3d7af2c2 <range-tip>`
and one sequential ten-commit apply. Raw dumps: `03-raw-*.txt` beside this file.

## Verdict

**The ten-commit payload is not a cherry-pickable set on ABI 2. Five of ten
apply, and two of those five do not compile; four have no file to patch at all
(`src/rutabaga_gfx` was dropped in the ABI-2 rewrite) and one is a real content
conflict.** The port is a re-derivation, not a cherry-pick.

## 1. Per-commit matrix (onto `3d7af2c2`)

Range `5849c08d..3819ce5f` = 10 commits, `merge-base(3d7af2c2, 3819ce5f) = 0d75eb4b`
(the pre-ABI-rewrite base). Full dump:
`03-raw-per-commit-cherrypick-matrix.txt`.

| # | commit | subject | rc | conflict / note |
|---|--------|---------|----|-----------------|
| 1 | `4ef22a14` | vmm/builder: create file-backed memory for gpu udmabuf as well | 0 | applies to `src/libkrun/src/vmm/builder.rs`, but **does not compile** (inserts `vm_resources.gpu_virgl_flags`, see §3) |
| 2 | `a3962256` | vmm/builder: seal file-backed memory against size changes | 0 | clean, valid |
| 3 | `a8fd2784` | rutabaga_gfx: cross_domain: allow attach() of guest blobs with handles | 1 | modify/delete `src/rutabaga_gfx/src/cross_domain/mod.rs` |
| 4 | `ec9e5562` | rutabaga_gfx: add `RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE` | 1 | modify/delete `src/rutabaga_gfx/src/rutabaga_utils.rs` |
| 5 | `3391f3c5` | rutabaga_gfx: cross_domain: support creating guest resources with handles | 1 | modify/delete `src/rutabaga_gfx/src/cross_domain/mod.rs` |
| 6 | `16e116ae` | rutabaga_gfx: direct context-less CREATE_GUEST_HANDLE to cross-domain | 1 | modify/delete `src/rutabaga_gfx/src/rutabaga_core.rs` |
| 7 | `b3f9c114` | utils/linux: Add udmabuf driver wrapper | 0 | clean, valid |
| 8 | `230f2c55` | [XXX] update `VIRTIO_GPU_F_CREATE_GUEST_HANDLE` constant | 0 | applies textually; **must be dropped** (§4) |
| 9 | `6c51645c` | if udmabuf is present set F_GUEST_CREATE_HANDLE and pass driver | 1 | content conflict `src/devices/src/virtio/gpu/virtio_gpu.rs` (3 hunks); `device.rs`, `worker.rs` auto-merge |
| 10 | `3819ce5f` | use the udmabuf driver to implement CREATE_GUEST_HANDLE | 0 | applies cleanly **in isolation only**; it needs #9's `udmabuf_driver` field, so it is a dependent commit |

Textual-clean != usable: after applying `3819ce5f` alone the tree has zero
declarations of `udmabuf_driver` (only its single use) and no `RutabagaHandle`
import (`03-raw-textual-vs-semantic.txt`); after `6c51645c` alone it references
`utils::linux::udmabuf`, which `b3f9c114` (#7) creates. Order-dependence is the
whole game here.

**Composite.** Forced-base merge-tree over just the range
(`--merge-base=5849c08d`, rc=1, `03-raw-composite-merge-tree.txt`) conflicts only
in `virtio_gpu.rs` (content) plus the three rutabaga files (modify/delete);
`builder.rs`, `device.rs`, `worker.rs`, `utils/*` auto-merge. Do **not** read the
two-tip `merge-tree 3d7af2c2 3819ce5f` (`03-raw-two-tip-merge-tree.txt`) as the
payload: because the merge-base is `0d75eb4b`, it also drags in the branch's
`vm-memory 0.18`/`imago` commits and shows 16 conflict files (`Cargo.lock`,
`src/devices/Cargo.toml`, `block/device.rs`, `libkrun/Cargo.toml`,
`linux/vstate.rs`, `utils/Cargo.toml`, `vmm/Cargo.toml`, ...) that are not
PR 822's.

## 2. Collisions with the fork's own ABI-2 GPU work

Fork GPU commits on the ABI-2 base: `d578e4e2` (docs+CI only, no code),
`29312733` (render-server fd + fence retirement), `3d7af2c2` (venus/DRM capset
fix + native DRM path). Full hunks: `03-raw-fork-gpu-hunks.txt`.

* **`virtio_gpu.rs` is the only content conflict**, three hunks against
  `6c51645c` (`03-raw-conflicts-and-memfd-hunks.txt`):
  1. `std::os::fd` import: the fork already has `IntoRawFd`/`OwnedFd` (from
     `29312733`); the PR hunk rewrites `AsRawFd` -> `{AsRawFd, IntoRawFd}`.
     Resolution: keep the fork's imports.
  2. the DMA-BUF handle-type import + cfg. PR base:
     `#[cfg(all(feature = "virgl_resource_map2", target_os = "linux"))]
     use rutabaga_gfx::RUTABAGA_MEM_HANDLE_TYPE_DMABUF;`. ABI-2 renames it to
     `RUTABAGA_HANDLE_TYPE_MEM_DMABUF` and gates it
     `#[cfg(any(all(target_os="linux", feature="virgl_resource_map2"), target_os="macos"))]`,
     falling back to `..._MEM_OPAQUE_FD` when `virgl_resource_map2` is off.
     udmabuf always yields a **DMABUF** handle, so that import has to become
     unconditional on Linux; this hunk cannot be taken verbatim.
  3. the second `use rutabaga_gfx::{...}` block: PR adds
     `RutabagaDescriptor, RutabagaFromRawDescriptor, RutabagaHandle`; ABI-2 has
     reorganised it and already imports `RutabagaDescriptor` /
     `RutabagaFromRawDescriptor` (from `29312733`). Union.
* **`device.rs` / `worker.rs` auto-merge but are semantically shared**: both the
  PR (`udmabuf_driver: Option<UdmabufDriver>`) and the fork
  (`render_server_fd: Option<OwnedFd>`) add a field + constructor param + a
  `take()` in `activate`. Git merges the text; the merged constructor call site
  (used by `GpuDevice::attach` at `api/device_builders.rs:1696`) must carry both.
* No collision in `VIRTIO_GPU_F_*` between the two: the fork does not touch
  `mod.rs` constants; `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6` is already there
  (§4).

## 3. The two memfd-backed guest-RAM commits on today's builder

Dump: `03-raw-abi2-builder-memfd.txt`, `03-raw-abi2-gpu-wiring-and-requirements.txt`.

`create_guest_memory` on ABI 2 lives at `src/libkrun/src/vmm/builder.rs:1835` and
takes `use_vhost_user: bool`, `gpu_shm_size: Option<usize>` - **not**
`&VmResources`. It has the same memfd branch the PR edits (memfd_create
`MFD_CLOEXEC` + ftruncate + `GuestMemoryMmap::from_ranges_with_files`).

* **`4ef22a14`'s condition is dead on ABI 2.** It inserts
  `let use_gpu_udmabuf = vm_resources.gpu_virgl_flags.is_some();`. `gpu_virgl_flags`
  exists nowhere in the ABI-2 tree (grep empty; it was a v1 `VmResources` field
  at `5849c08d`) and `vm_resources` is not in scope in `create_guest_memory`.
  Textual cherry-pick rc=0, guaranteed `E0425`/`E0609`.
  **Correct condition on today's builder:** GPU presence is a *device
  requirement*, so
  ```rust
  #[cfg(all(feature = "gpu", target_os = "linux"))]
  let use_gpu_udmabuf = gpu_shm_size.is_some();      // == requirements.iter().any(|r| r.gpu_shm.is_some())
  #[cfg(not(all(feature = "gpu", target_os = "linux")))]
  let use_gpu_udmabuf = false;
  ```
  `GpuDevice::requirements()` (`api/device_builders.rs:1685`) always sets
  `gpu_shm: Some(self.shm_size)` with `DEFAULT_SHM_SIZE = 1 << 33`, and cang's
  launcher registers the GPU through `krun_mmio_device_manager_new` +
  `krun_vmm_builder_devices` (`crates/cang/src/runtime/vm/libkrun/launcher.rs`
  `configure_gpu`), so any `--gpu=drm` run hits it.
* **`a3962256`** (`MFD_ALLOW_SEALING | memfd_create`, then
  `F_ADD_SEALS(F_SEAL_GROW | F_SEAL_SHRINK)`) applies cleanly and is right for
  udmabuf: `drivers/dma-buf/udmabuf.c` has
  `SEALS_WANTED (F_SEAL_SHRINK)`, `SEALS_DENIED (F_SEAL_WRITE|F_SEAL_FUTURE_WRITE)`
  and `check_memfd_seals()` requires `shmem_file()`; grow+shrink is exactly the
  accepted set.
* **The fast path changes the RAM backing entirely.** vm-memory 0.18 maps
  file-backed ranges `MAP_NORESERVE | MAP_SHARED` (`mmap/unix.rs:256`) and
  anonymous ranges `MAP_ANONYMOUS | MAP_NORESERVE | MAP_PRIVATE`
  (`mmap/unix.rs:242`). So with a GPU device present, all of `create_guest_memory`'s
  regions - the main RAM, not only the GPU shmem window - become a MAP_SHARED
  sealed memfd. The lazy-`MAP_NORESERVE` property is kept; the private/anonymous
  part is not.
* **Balloon FRQ over a sealed memfd.** Sealing is not the problem (GROW/SHRINK
  does not block writes or `madvise`). The problem is that the mapping is
  `MAP_SHARED` shmem: `madvise(2)` says of `MADV_DONTNEED` that for "shared file
  mappings ... subsequent accesses ... repopulate the memory contents from the
  up-to-date contents of the underlying mapped file", and "when applied to
  shared mappings, MADV_DONTNEED might not lead to immediate freeing of the
  pages in the range. ... The RSS of the calling process will be immediately
  reduced however." The balloon's `process_frq` `libc::madvise(.., MADV_DONTNEED)`
  (`src/devices/src/virtio/balloon/device.rs:100-105`) therefore still drops the
  VMM's RSS but the pages stay resident in the tmpfs/memfd page cache; the guest
  RAM memfd has no backing store, so without host swap they are unevictable and
  the host gets no memory back. cang's balloon value (project memory #20: VM
  worker `RssAnon` 3563 -> 585 MiB after a `dd`/`rm`) is an *anonymous-map*
  effect and **does not carry over to the memfd path**; on this host only zram
  swap exists (`SwapTotal 17375844 kB`, zram), so even partial eviction is
  compressed-in-RAM. This is a real, cang-visible regression of the udmabuf path,
  not a build problem.

## 4. The constant commit `230f2c55` (bit 6 -> 5)

Exclude it. The ABI-2 tip **already** defines
`VIRTIO_GPU_F_RESOURCE_SYNC = 5` and `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`
(`src/devices/src/virtio/gpu/mod.rs:28-29`, present since the 2023 import
`5aef4073`). `230f2c55` comments out `RESOURCE_SYNC` and renumbers
CREATE_GUEST_HANDLE to 5, i.e. it collides with both `RESOURCE_SYNC` and cang's
kernel `VIRTIO_GPU_F_FENCE_PASSING = 5` (`deps/libkrunfw` patches/0018), and it
is what the MAP's Out-of-scope records. Cherry-picking it applies textually and
is still wrong.

The remaining commits do **not** assume 5: `6c51645c` uses
`1u64 << uapi::VIRTIO_GPU_F_CREATE_GUEST_HANDLE` (symbolic) and `3819ce5f` tests
`VIRTIO_GPU_BLOB_FLAG_CREATE_GUEST_HANDLE`, a *different* constant
(`src/devices/src/virtio/gpu/protocol.rs:86 = 0x0008`) that already exists at
`3d7af2c2`. The value stays **6**.

## 5. What the port actually is

* `a3962256` and `b3f9c114` carry as-is; `4ef22a14` must be rewritten to the
  `gpu_shm_size.is_some()` condition above.
* `6c51645c` + `3819ce5f` forward-port onto the fork's `29312733`/`3d7af2c2`
  (`virtio_gpu.rs` import/cfg resolution in §2; keep `render_server_fd`).
* `a8fd2784`/`ec9e5562`/`3391f3c5`/`16e116ae` cannot cherry-pick: ABI-2 has no
  `src/rutabaga_gfx` (crates.io `rutabaga_gfx 0.1.85`). Re-derive them against
  the rev-pinned rutabaga git dependency the map's destination requires
  (`03-raw-rutabaga-0.1.85-api.txt`): 0.1.85 already has `RutabagaHandle`,
  `RutabagaFromRawDescriptor`, `resource_create_blob(..., Option<RutabagaHandle>)`
  and `set_server_descriptor`, but **lacks** the `0x0008` blob flag,
  component-level handle retention (`cross_domain/component.rs` takes
  `_handle_opt` and ignores it; it rejects any flag other than
  `RUTABAGA_BLOB_FLAG_USE_MAPPABLE`) and the `ctx_id == 0` -> cross-domain
  component routing (`rutabaga_core.rs:950` sends `ctx_id > 0` to a
  cross-domain *context* and everything else to the default component).
* `230f2c55` dropped.

## Confidence / limits

The matrix is a git-level measurement; the "does not compile" claims for
`4ef22a14` (and for `3819ce5f`/`6c51645c` in isolation) are read off symbol
absence - no `cargo check` was run on the patched tree.
