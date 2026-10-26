---
label: wayfinder:research
title: Would a DAX window on the root share help a rustc build, and what does enabling it cost?
status: open
blocked_by: []
claimed_by: pi research child (2026-10-25)
---


> **Floor correction (2026-10-26).** Any "no-FUSE floor" quoted in this note as
> 232 s was a **lever-off** measurement, not a no-FUSE one: `/` is itself the
> virtio-fs share (`/proc/mounts`: `/dev/root / virtiofs`), so the guest-local arm
> launched 2401 `rustc`/`ld` binaries off the same connection and paid
> OPEN+RELEASE for each (OPEN 87,871-89,374). With zero-message open the same
> tmpfs arm runs 111.0-164.9 s, and the true no-FUSE floor is ~131 s. See
> `MAP.md` ("The governor fact that reorders everything") and note 23's
> independent re-measurement for the numbers and the method.
# 05 - DAX window viability on cang's single root share

Ticket: `tickets/05-dax-window-viability.md`.

Read-only: this note changed nothing in the tree, ran no build and booted no VM,
so every magnitude below is a mechanism argument with a code citation, not a
measurement. Ticket 02's harness is what turns any of it into numbers.

Sources (identifiers are what I actually read):

| what | where |
|---|---|
| the fork's virtio-fs + VMM | `deps/libkrun` @ `79228a72` (tag `v2.0.0-cang.10`): `src/devices/src/virtio/fs/{device,worker,server,fuse}.rs`, `.../fs/linux/passthrough.rs`, `src/devices/src/virtio/{mmio,pci}.rs`, `src/libkrun/src/api/device_builders.rs`, `src/libkrun/src/vmm/builder.rs`, `src/libkrun/src/vmm/device_manager/shm.rs`, `src/libkrun/src/vmm/linux/vstate.rs`, `src/arch/src/x86_64/mod.rs`, `include/libkrun.h` |
| cang's call sites | `crates/cang-libkrun/src/{api.rs,linked.rs,lib.rs}`, `crates/cang/src/runtime/vm/libkrun/launcher.rs`, `crates/cang/src/runtime/launch/...` |
| guest kernel | `v7.2` on `git.kernel.org`: `fs/fuse/{dax.c,virtio_fs.c,inode.c,file.c}`, `include/uapi/linux/fuse.h`, `init/do_mounts.c`, `drivers/virtio/virtio_mmio.c`, `mm/memremap.c`, `mm/memory_hotplug.c`, `fs/dax.c`, `arch/x86/{mm/init_64.c,include/asm/sparsemem.h}` |
| kernel as built by cang | `deps/libkrunfw/config-libkrunfw_x86_64-kvm{,-lto}` |
| the vm-memory that backs the window | `vm-memory 0.18.0` (the fork's `Cargo.lock`), `src/mmap/unix.rs` |

Line numbers are stable for those refs. **The ticket's own line citations are
stale**: `device_builders.rs:1841-1940` is the *GPU* device's `shm_size` field and
its `attach`, not the window plumbing; the fs window plumbing is
`device_builders.rs:659-666` / `:701-708` / `:711-734` plus
`vmm/device_manager/shm.rs` and `vmm/builder.rs:2036-2137`. Everything else the
ticket states about the wiring (guest and server halves exist, cang never sets
`shm_size`) checks out.

## Verdict

**Drop.** Not "later", and not behind the worker pool: DAX's unit of benefit is
*one 2 MiB-aligned region of one file, consulted repeatedly*, and cang's unit of
application is *the whole guest root, all files, every operation* - because
there is one share mounted as `/` and `dax=always` is per-connection, so it
covers `/nix` executables and shared libraries exactly as it covers `target/`.
That makes the first fault of every 2 MiB region of every mapped file (rustc
itself, every `cc`/`ld` invocation, every `.rlib` the linker maps) a
`FUSE_SETUPMAPPING` round trip on the fork's *single* `FsWorker` thread, with a
2 MiB slot consumed per file regardless of file size, and with **no fallback to
a normal read when the window
runs out** - the fault path waits, the read/write path waits killably
([§3.4](#34-what-happens-when-the-window-is-exhausted-no-fallback-only-a-wait)).
It also voids the one lever this map is going to ship: on a `S_DAX` inode the
read/write/mmap paths are replaced by DAX, so `WRITEBACK_CACHE` (ticket 03/04) no
longer applies to any file data on the share
([§3.5](#35-it-does-not-compose-with-the-levers-ticket-04-is-going-to-ship)).
The windows that would avoid thrash for a real build are multi-GiB on an 18 GiB
guest, and the failure modes below (mount-time `-EINVAL` when
`dax=always` meets no window, probe failure when the window is not aligned to the
guest's memory-block size, i.e. a guest that never finds its root) are
boot-bricking rather than degrading.

The one measurement that would reopen it is in [§5](#5-verdict-for-ticket-04).

## 0. The two framings the ticket got slightly wrong

1. **"Whole-share" is worse than it sounds: it is whole-*root*.** The ticket says
   the window is shared by `/nix`, `~/.cargo` and `target/`; true, but the
   consequence that matters is that DAX is selected per **connection**, not per
   file or per path, and cang's connection *is* the guest's `/`
   (`rootfstype=virtiofs` in the cmdline, `fs_device_new` at `linked.rs:317-326`,
   mounted as `/`).
   `dax=always` therefore sets `S_DAX` on **every inode of the guest root**
   (`dax.c:1296-1325`), including every binary under `/nix/store` that the guest
   `execve`s.
2. **`dax=inode` - the only mode that could scope it - cannot work here.** It
   needs the server to advertise `FUSE_HAS_INODE_DAX` and to set
   `FUSE_ATTR_DAX` in `GETATTR`; the fork does neither (it is not in
   `server.rs:897-908`, and `ATTR_DAX` does not appear anywhere in
   `deps/libkrun`). So the only reachable mode is all-or-nothing `dax=always`.

## 1. The guest mount path: the kernel mounts root itself, and only the cmdline can turn DAX on

### 1.1 Which code mounts the root, and with which source

`DEFAULT_KERNEL_CMDLINE` is `reboot=k panic=-1 panic_print=0 nomodule
console=hvc0 rootfstype=virtiofs rw quiet no-kvmapf`
(`deps/libkrun/src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:5-6`); there is no
`root=` and no `rootflags=`. `krun-init` is not involved in the mount: it is the
guest PID 1 *binary* injected into the share as `/init.krun` and appended to the
cmdline as `init=/init.krun` (`init/init-blob/src/config.rs:53`, `:132`) - the
kernel has already mounted root by the time it execs it.

The path is the generic one, not a virtiofs special case:

- `prepare_namespace()` (`init/do_mounts.c:457-495`) sets `ROOT_DEV` only
  `if (saved_root_name[0])` (`:476-477`); with no `root=` it stays 0.
- `mount_root("")` (`:381-403`) therefore takes `case 0:` - `root_device_name &&
  root_fs_names && mount_nodev_root(...)` (`:394-397`). `root_fs_names` is
  `virtiofs`, `root_device_name` is `""`, so `mount_nodev_root("")`
  (`:339-364`) calls `do_mount_root("", "virtiofs", ...)` -> `init_mount("",
  "/root", "virtiofs", flags, data)`, whose empty `dev_name` becomes
  `fc->source = ""` (`fs/fs_context.c:80-94`) - a source that matches no tag, so
  `virtio_fs_find_instance("")` fails and `virtio_fs_get_tree` returns `-EINVAL`
  (`virtio_fs.c:1695-1706`); `mount_nodev_root` returns non-zero.
- So it falls through to `mount_block_root("")` (`:399-401`, `:367-373`), which
  is `mount_root_generic("/dev/root", "", root_mountflags)` (`:175-197`) ->
  `do_mount_root("/dev/root", "virtiofs", flags, root_mount_data)`.
  **The source handed to virtiofs is the literal string `/dev/root`** - which
  matches only because cang registers its root device with that tag
  (`crates/cang-libkrun/src/lib.rs:25`, `CANG_FS_ROOT_TAG = "/dev/root"`, used at
  `launcher.rs:243`). This is also why the tag is not a free parameter.
- `root_mount_data` is `rootflags=`, parsed at `:99-104`/`:121`, and passed
  verbatim as the mount data of that `init_mount` (`:142-173`, `data_page` at
  `:146-157`). The `rw` in the cmdline clears `root_mountflags`, so the first
  attempt is read-write; `mount_root_generic` only forces `SB_RDONLY` on the
  retry (`:231-234`), so no `remount,rw` pass is involved.

### 1.2 The minimal change, and why it has to be the cmdline

`virtio_fs_context_ops` has **`.free`, `.parse_param`, `.get_tree` and no
`.reconfigure`** (`virtio_fs.c:1761-1765`), so `mount -o remount,dax=always` on
the mounted root fails with `-EINVAL`. A second mount of the same tag cannot help
either: `virtio_fs_get_tree` reaches `sget_fc(..., virtio_fs_test_super, ...)`
(`:1735`), whose match predicate is *the same device*
(`:1673-1680`), so a later `mount -t virtiofs /dev/root /mnt -o dax=always`
reuses the existing superblock and silently keeps the old (non-DAX mode)
connection. And the mode must be fixed before `FUSE_INIT`, because
`fc->dax_mode` is consumed by `fuse_dax_conn_alloc` inside
`fuse_fill_super_common` (`inode.c:1750-1754`) and drives the per-inode
`S_DAX` decision (`dax.c:1311-1312`).

So the one and only lever is the kernel cmdline: **`rootflags=dax=always`**
(`dax` alone is the same thing - `fsparam_flag("dax", OPT_DAX)` ->
`FUSE_DAX_ALWAYS`, `virtio_fs.c:117`, `:134-135`).

That must be conditional: `virtio_fs_fill_super` returns `-EINVAL` when
`dax=always` meets a device with no dax window (`virtio_fs.c:1596-1602`), and
`mount_root_generic` treats `-EINVAL` as "try the next fs", exhausts the list and
**panics** (`do_mounts.c:198-242`). A boot with the flag and no window does not
degrade - it panics before `/init.krun` runs.

Two places can carry the conditional, both already exercised:

- **Guest-side (recommended for the experiment, no fork change):** cang's
  launcher appends the fragment through the API it already uses for its profile
  cmdline - `LibkrunApi::payload_append_cmdline` (`crates/cang-libkrun/src/api.rs:28`,
  `linked.rs:227-232`), called from `launcher.rs:283-285`. Append
  `"rootflags=dax=always"` there, gated on cang having asked for the window.
- **Fork-side (upstream-shaped):** `FsDevice::attach` already receives both the
  resolved region and the cmdline - `ctx.resolved_shm_region()`
  (`device_builders.rs:191-204`, used at `:728-730`) and
  `ctx.append_kernel_cmdline()` (`:260-265`), with the vsock `tsi_hijack`
  fragment as the precedent (`:1416-1427`). Appending there is 4 lines at
  `device_builders.rs:711-734`, and it lands because `attach_all` runs at
  `vmm/builder.rs:1397-1408` and `load_cmdline` only later at `:1440`. It should
  be gated on the device being the root share (tag `"/dev/root"`), because
  `rootflags=` describes the *root* mount, not an arbitrary export.

Guest side needs no other change: no `cang-guest-init` edit, no remount, no mount
helper.

## 2. The VMM half: where the window lives, what it costs, and what the guest accepts

### 2.1 The plumbing, end to end (all present today)

1. `FsDevice::set_dax_window_size(bytes)` sets `shm_size`
   (`device_builders.rs:659-666`), exported to C as
   `krun_fs_device_set_dax_window_size` (`include/libkrun.h:250`).
2. `DeviceRequirements { shm_size }` (`:60-62`, `:701-708`) is collected before
   guest memory exists (`vmm/builder.rs:784`, `:808`).
3. `ShmManager::create_fs_region(index, size)` carves the region out of one
   sequential arena starting at `ArchMemoryInfo::shm_start_addr`, page-aligning
   the size (`vmm/device_manager/shm.rs:63-76`, `:106-112`); the GPU's window
   comes from the same arena and the same counter (`create_gpu_region`, `:88-96`).
4. The window is appended to the guest's memory ranges *before* `GuestMemoryMmap`
   is built (`vmm/builder.rs:2061`), so it becomes one more
   `GuestRegionMmap`/KVM memslot (`vmm/linux/vstate.rs:621-623`, `:654-676`).
5. `resolved_shm_region()` hands the device `{host_addr, guest_addr, size}`
   (`device_builders.rs:191-204`) and `FsDevice::attach` calls
   `fs.set_shm_region(...)` (`:728-730`); the `Fs` keeps it
   (`fs/device.rs:53`, `:103`, `:119-121`) and passes it to the worker
   (`:228`), which passes it to every `handle_message` (`fs/worker.rs:244-249`).
6. The guest finds it the same way every virtio shared-memory region is found:
   the MMIO transport answers `VIRTIO_MMIO_SHM_SEL` (0xac) select 0 and the
   64-bit len/base at 0xb0..0xbc (`virtio/mmio.rs:57-62`, `:290-310`, `:383`),
   which is exactly what the driver's `vm_get_shm_region` reads
   (`drivers/virtio/virtio_mmio.c:492-520`), and
   `virtio_fs_setup_dax` (`virtio_fs.c:1076-1140`) turns it into a ZONE_DEVICE
   `dev_pagemap` + `dax_device` at probe time (`:1163`).
7. `FUSE_SETUPMAPPING`/`FUSE_REMOVEMAPPING` are gated on `shm_region.is_some()`
   (`server.rs:150-158`, `:166-174`); the handler decodes a struct that matches
   the kernel's to the byte (`fuse.rs:1287-1295` vs `fuse.h:1171-1182`) and the
   passthrough implementation really `mmap`s the host file into the window:
   `MAP_SHARED | MAP_FIXED` at `host_shm_base + moffset` with `foffset`
   (`passthrough.rs:2045-2095`), and `removemapping` re-`mmap`s the range
   `PROT_NONE`/anonymous (`:2097-2126`).

The mechanism that makes the guest see those host file pages: the window is an
ordinary `GuestRegionMmap` in the VMM's address space whose `userspace_addr` is
the KVM memslot (`vstate.rs:660-666`), and KVM resolves guest-physical to host
page through that userspace mapping - so replacing the VMM's PTEs with the
exported file's page-cache pages with `MAP_FIXED` is visible to the guest's
second-level mapping, and KVM's mmu notifier drops the sptes when
`removemapping` blows the mapping away. This is the crosvm/virtiofsd design, not
something the fork invented.

### 2.2 The region is outside guest RAM and outside e820

- `arch_memory_regions` computes `shm_start_addr` as
  `((ram_last_addr / 1 GiB) + 1) * 1 GiB` (case 2) or `4 GiB` (case 1)
  (`arch/src/x86_64/mod.rs:154-195`), i.e. **above** RAM on a 1 GiB boundary.
- e820 is written from `ram_last_addr` only (`mod.rs:494-524`), so the window is
  not SYSTEM_RAM: that is required by `pagemap_range`'s "attempted on ram region"
  check (`mm/memremap.c:170-178`) and it also means the window costs nothing out
  of `--mem`.

### 2.3 It does not compete with the GPU window (but shares its arena)

`FsDevice` and `GpuDevice` each declare their own `shm_size`/`gpu_shm`
(`device_builders.rs:60-66`, `:701-708`, `:1841`, `:1939-1941`) and each device
reports its own region on its own MMIO transport (`fs/device.rs:251-253`,
`gpu/device.rs:279-282`). Both regions are allocated from the same sequential
`ShmManager` arena - fs regions first, then the GPU's
(`vmm/builder.rs:2040-2053`) - so adding a window merely *shifts* the GPU window
up; nothing depends on its absolute address (it is discovered via MMIO). The only
real coupling is transport-level: a device with a `shm_region` is rejected on PCI
(`virtio/pci.rs:377-379`) and so is any `shm_size` requirement when PCI is on
(`vmm/builder.rs:789-807`). cang is x86_64 + `MmioDeviceManager`
(`launcher.rs:220-223`), so this is currently a non-issue - but it does mean a
future aarch64/PCI path and DAX are mutually exclusive in this fork, and it is
the one place where "does it compete with the GPU" has a real answer.

### 2.4 `map_alignment`: 0 is fine, and the guest never even checks it

The fork replies `map_alignment: 0` and does not set `FUSE_MAP_ALIGNMENT`
(`server.rs:940-953`); the bit exists as a bare constant but is **not** in the
`FsOptions` bitflags (`fuse.rs:153-157` vs `:432`), so the fork physically cannot
echo the kernel's offer. The kernel only validates alignment when the reply sets
the bit (`inode.c:1353-1357`), and the check itself only rejects a value greater
than `FUSE_DAX_SHIFT` = 21 (`dax.c:1336-1343`). Since the guest's own range
granularity is 2 MiB (`dax.c:20-22`) and *it* chooses both `foffset`
(`start_idx << 21`, `:189`) and `moffset` (`i * FUSE_DAX_SZ`, `:1231`), 2 MiB
alignment is satisfied on both sides by construction; the host's `mmap` only
needs page alignment, and `host_shm_base` is 1 GiB-aligned. **No change needed.**

Two hard constraints do exist, and neither is in the ticket:

- **The window must be a multiple of the guest's memory-block size and its guest
  physical base must be too.** `devm_memremap_pages` ->
  `mhp_range_allowed` -> `check_hotplug_memory_range`
  (`mm/memremap.c:193`, `mm/memory_hotplug.c:1325-1336`) rejects anything else
  with `-EINVAL`, which fails `virtio_fs_setup_dax`, which fails the device
  probe, which means the tag never appears and **the root mount panics**. On
  x86_64 the block size is `memory_block_size_bytes()`
  (`arch/x86/mm/init_64.c:1474-1522`): 128 MiB (`SECTION_SIZE_BITS` 27,
  `arch/x86/include/asm/sparsemem.h`) whenever `max_pfn << PAGE_SHIFT < 64 GiB`,
  but up to 2 GiB for bigger guests, halved until it divides the end of RAM. A
  sub-64-GiB guest is therefore fine with any 128 MiB multiple at a 1 GiB-aligned
  base; a `--mem 64+` guest can silently require a 1 or 2 GiB multiple, and
  libkrun's base is only 1 GiB-aligned, so both base and size may then be
  unaligned.
- **The window must be at least 2 MiB and effectively a multiple of it.** The
  guest derives its free-list from `window_len / FUSE_DAX_PAGES`
  (`dax.c:1208-1239` against `virtio_fs.c:1030-1043`); `nr_ranges == 0` means
  the free list is empty for ever, and both allocation paths wait on
  `range_waitq` ([§3.4](#34-what-happens-when-the-window-is-exhausted-no-fallback-only-a-wait)),
  i.e. the guest hangs on the first file read rather than booting without DAX.

### 2.5 Memory cost on a cang default guest

A default guest takes `0.8 x MemTotal`
(`crates/cang/src/runtime/launch/config/components/resources.rs:53-67`); on this
host (24,662,976 kB) that is **18 GiB**, which also fixes the layout: case 2 in
`arch_memory_regions`, `ram_last_addr` = 19 GiB, `shm_start_addr` = **20 GiB**
(`arch/src/x86_64/mod.rs:154-157`), so the window sits at 20 GiB and (with
`--gpu=drm`) the 256 MiB GPU window (`launcher.rs:52`) follows it.

Cost of a window of `W` bytes, per component:

| component | size | note |
|---|---|---|
| guest RAM taken from `--mem` | **0** | the window is not in e820 (`mod.rs:494-524`) and the balloon never covers it |
| guest vmemmap (`struct page` per 4 KiB of window, ~64 B each) | `W / 64` | `devm_memremap_pages` populates struct pages for the ZONE_DEVICE range |
| guest page tables for the direct map | ~`W / 262144` | mapped with large pages by `devm_memremap_pages` |
| guest physical address space | `W` | reserved above RAM at a 1 GiB boundary |
| host address space | `W` | one more anonymous `MAP_NORESERVE` region (`vm-memory 0.18.0 src/mmap/unix.rs:239-244`), zero RSS until touched |
| host KVM memslots | +1 | `vmm/linux/vstate.rs:654-676` |
| host page cache made unevictable | up to `W` | the mapped pages are the exported files' page-cache pages, pinned in the guest's second-level mapping while their dmap is live |

| `W` | 2 MiB slots (`W / 2 MiB`) | guest vmemmap | distinct file regions it can hold at once |
|---|---|---|---|
| 128 MiB | 64 | 2 MiB | 64 (a *small* file still consumes a whole slot) |
| **256 MiB** | **128** | **4 MiB** | **128** |
| 512 MiB | 256 | 8 MiB | 256 |
| 1 GiB | 512 | 16 MiB | 512 |
| 4 GiB | 2048 | 64 MiB | 2048 |

**Recommended size, if it ever ships on this map: 256 MiB.** It is the smallest
window worth trying (128 MiB passes the block-alignment rule but holds only 64
regions), holds 128 concurrent file regions, and costs 4 MiB of an 18 GiB guest's RAM plus 256 MiB of reserved (not
resident) host address space. Anything larger is paying guest RAM for slots that
[§3](#3-does-the-workload-benefit) says a rustc build cannot keep warm anyway; a
1 GiB window (512 slots, 16 MiB vmemmap) is the most that is worth an experiment,
and it is already misaligned for any guest whose block size is 2 GiB.

## 3. Does the workload benefit?

### 3.1 What DAX replaces

`dax=always` puts every inode of the share in `S_DAX` (`dax.c:1296-1325`), and
then all three data paths divert:

| operation | non-DAX today | with `S_DAX` |
|---|---|---|
| `read(2)` | `fuse_cache_read_iter` -> page cache -> `FUSE_READ` (up to 1 MiB, `server.rs:35`,`:933`) | `fuse_dax_read_iter` (`file.c:1846-1847`) -> `dax_iomap_rw` -> `iomap_begin` -> `SETUPMAPPING` for the whole 2 MiB region, then a memcpy out of the window (`dax.c:676-693`, `:561-633`) |
| `write(2)`, not extending | page cache -> writeback/`FUSE_WRITE` | `fuse_dax_write_iter` (`file.c:1867-1868`) -> `dax_iomap_rw` on the window (`dax.c:716-751`) |
| `write(2)`, extending | page cache -> `FUSE_WRITE` | `fuse_dax_direct_write` -> `fuse_direct_io(FUSE_DIO_WRITE)`, i.e. **today's** synchronous path (`dax.c:695-714`, `:740-741`) |
| `mmap` | page-cache pages, `fuse_readpage`/readahead | `fuse_dax_mmap` (`dax.c:823-829`), fault -> `SETUPMAPPING` (`:753-794`) |

So the price of admission is **one synchronous `SETUPMAPPING` round trip per
2 MiB-aligned file region, on the single `FsWorker` thread**
(`fs/worker.rs:234-268`), and the host side of that round trip is an extra
`openat` (`open_inode`, `passthrough.rs:2077`) plus an `mmap` syscall
(`:2080-2089`).

### 3.2 Who wins

Only a region that is *touched more than once while its dmap is live*:

- the linker (`rust-lld`) mapping and re-reading object files, and the final
  binary it writes;
- `rustc` mmapping a large `.rlib`/`.rmeta` and consulting pages repeatedly;
- any large file that is read, partially re-read, or `mmap`ed twice.

Everything else is a loss or a wash:

- a file smaller than 2 MiB costs exactly one `SETUPMAPPING` (comparable to the
  one `FUSE_READ` it replaces) **and occupies a whole 2 MiB slot** - a 4 KiB
  `.rmeta` wastes 512x its size, so slot count, not bytes, is the scarce
  resource: a 256 MiB window holds 128 distinct file regions;
- files read once (most of a build's inputs, all of `Cargo.toml`s, `.d` files,
  fingerprints) get no reuse at all and pay the round trip;
- file-extending writes - i.e. almost all of `target/`, which is created fresh -
  take the unchanged direct-write path, so the write half of a build sees **no
  benefit**;
- every `execve` of a guest binary maps an `S_DAX` inode, so the guest's own
  tool chain (rustc, `cc`, `ld`, `sh`, every `/nix/store` library) turns its text
  faults into window traffic, and reclaim *zaps those PTEs*
  (`dax_layout_busy_page_range` -> `unmap_mapping_pages`,
  `fs/dax.c:821`) so a running process refaults through `SETUPMAPPING` again.

Against a single serial worker, that last point is the killer: DAX does not
remove requests, it moves the data path's requests *into the same queue* that the
worker pool (ticket 01's lever 2, ticket 04's item 2) is meant to unblock, and it
adds a new class of them (page faults) that cannot be batched.

### 3.3 Window sizing for this workload

The number that matters is *distinct concurrently-live 2 MiB regions*, and for a
rustc-shaped build it is bounded below by the number of files touched in a
reclaim window (seconds), not by bytes:

- the map's pinned small proxy crate tree: a few hundred files, so 128-512 slots
  (256 MiB - 1 GiB) covers one compile unit's working set, which is the most that
  can be said for it;
- the databend arm: tens of thousands of files and a multi-GiB `target/`, with
  `lld` mapping hundreds of MiB of inputs at once. **No defensible window exists
  for it on an 18 GiB guest** - the working set is the same order as the guest's
  RAM, and every slot costs 2 MiB of it whether the file is 4 KiB or 2 MiB.

### 3.4 What happens when the window is exhausted: no fallback, only a wait

There is no "fall back to a normal read" path anywhere in `fs/fuse/dax.c`:

- **fault path:** `fuse_setup_new_dax_mapping` returns `-EAGAIN` when no range is
  free (`dax.c:454-457`), and `__fuse_dax_fault` loops: it
  `wait_event(fcd->range_waitq, ...)` - uninterruptible, no timeout - until the
  reclaim worker frees one (`:767-784`).
- **read/write path:** `alloc_dax_mapping_reclaim` first reclaims inline from the
  same inode, then `wait_event_killable_exclusive(fcd->range_waitq, ...)`
  (`dax.c:987-1033`); killable, so `SIGKILL` can break it, but a build just
  stalls.
- **reclaim can refuse to make progress:** only ranges with `refcnt == 1` are
  candidates (`:1053`, `:1124`), and each one is freed under an exclusive
  `filemap_invalidate_lock` after `dax_break_layout` (`:1084-1093`) — which
  unmaps the range from every process and waits for GUP pins (`fs/dax.c:977-1002`).
  The worker frees at most `FUSE_DAX_RECLAIM_CHUNK = 10` ranges per pass
  (`dax.c:24-25`, `:1100-1156`) and is only kicked when free ranges fall below
  20% (`:113-117`), so a hot window degenerates into a steady state of
  faults -> reclaim -> `REMOVEMAPPING` -> refault, all through one worker.
- **a failing host request is an error, not a downgrade:** `setupmapping` returns
  `EINVAL` for an out-of-bounds `moffset` (`passthrough.rs:2069-2071`) and the
  raw `mmap`/`openat` errno otherwise (`:2077-2092`), which surfaces to the
  application as `EIO` on the read or `SIGBUS` on the fault. (`reclaim_one_dmap_locked`
  then logs-but-frees on a failed `REMOVEMAPPING`, `dax.c:877-881`, so a host
  error there leaks a host mapping silently.)

### 3.5 It does not compose with the levers ticket 04 is going to ship

On a `S_DAX` inode the address_space aops are `noop_direct_IO`/`noop_dirty_folio`
(`dax.c:1291-1294`) and every data operation is diverted before the page cache
(`file.c:1846-1847`, `:1867-1868`, `:2404-2406`). Consequences for the map:

- **`WRITEBACK_CACHE` (ticket 01/03/04, first ship) stops applying to file data
  on the share.** The writeback win is "writes become page-cache writes flushed
  as <=1 MiB `FUSE_WRITE_CACHE` requests"; DAX has no page cache for data, and
  its non-extending writes go straight to `dax_iomap_rw`.
- **A longer/`Always` cache policy stops applying** for the same reason
  (`FOPEN_KEEP_CACHE` is page-cache semantics; `file.c:296-297` is not reached
  for DAX data).
- The worker pool and DAX do not conflict structurally, but DAX without the pool
  is strictly worse than today's read path for a one-shot read workload.

## 4. What this note does not settle

- No numbers, by design (no VM, no build). The claim "one `SETUPMAPPING` per file
  region is as expensive as the `FUSE_READ` it replaces" is a mechanism
  argument for a small file and an obvious win for a re-read large one; the
  crossover is `SETUPMAPPING` cost vs `FUSE_READ` cost, which only ticket 02's
  harness could measure - and it would have to instrument FUSE opcode counts,
  which ticket 02 was not asked for.
- Whether a *data-only* DAX share would pay. That question is different (files
  are >= 2 MiB and re-read) and it is blocked on a share split this map
  explicitly excludes.
- Exact host cost of the pinned page cache: `W` is the ceiling (every slot
  mapped and dirty), the steady state depends on the file set.

## 5. Verdict for ticket 04

**Drop - do not ship, and do not queue it behind the worker pool.** Reasons, in
order of weight:

1. **Scope mismatch.** DAX is selected per connection (`virtio_fs.c:104-145`,
   `:1596-1603`, no `.reconfigure` at `:1761-1765`) and cang has one connection,
   which is `/`. `dax=always` on it makes every inode of the guest root
   `S_DAX`, so the guest's own tool chain and `/nix` libraries enter the window.
   The only scoped mode, `dax=inode`, is unimplementable in this fork (no
   `FUSE_HAS_INODE_DAX`, no `FUSE_ATTR_DAX`).
2. **Granularity mismatch.** 2 MiB slots (`dax.c:20-22`) against a workload of
   mostly-sub-2-MiB files read once: slot count, not bandwidth, becomes the
   constraint, and a useful slot count on an 18 GiB guest is either not
   affordable or, at 1 GiB, still 512 regions.
3. **No fallback.** Exhaustion is a wait on `range_waitq`
   (`dax.c:767-784`, `:1026-1030`), not a downgrade to the normal read path, and
   the worst case is an uninterruptible stall inside a page fault.
4. **Boot-bricking failure modes.** `dax=always` without a window panics at root
   mount (`virtio_fs.c:1596-1602` + `do_mounts.c:198-242`); a window that is not
   a multiple of the guest's memory-block size fails the device probe
   (`mm/memory_hotplug.c:1325-1336`) and the guest cannot mount its root at all.
5. **It voids the map's first ship.** `WRITEBACK_CACHE` (ticket 03/04) and any
   cache-policy work stop applying to file data under DAX
   ([§3.5](#35-it-does-not-compose-with-the-levers-ticket-04-is-going-to-ship)).

**The one measurement that would reopen it** (i.e. the experiment a future ticket
should run instead of shipping): with the worker pool landed and ticket 02's
harness green, A/B the cold full-build arm with `rootflags=dax=always` and a
256 MiB window against the no-DAX baseline, logging per-run counts of
`FUSE_SETUPMAPPING` / `FUSE_REMOVEMAPPING` / `FUSE_READ` plus the wall clock. DAX
deserves a re-hearing only if that shows both a build-time win and a
`SETUPMAPPING` count far below the file count (i.e. reuse actually happening), on
a workload whose DAX-able files are >= 2 MiB. Until then the ticket's place in
the ordering is not "5th" - it is out, and the honest reason is that a whole-root
window cannot be aimed at `/workspace` without the share split this map excludes.
