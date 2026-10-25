---
label: wayfinder:research
title: Would a DAX window on the root share actually help a rustc build, and what does enabling it cost?
status: closed
blocked_by: []
claimed_by: pi research child-2 (2026-10-25)
---

## Question

Ticket 01 settled the wiring: the fork's server half exists
(`setupmapping`/`removemapping`, `passthrough.rs:2045-2100`), the pinned kernel's
guest half exists (`virtio_fs_setup_dax`, `virtio_fs.c:1076-1140`, `FUSE_DAX_SZ`
= 2 MiB, `dax.c:20-22`), DAX is selected by a **guest mount option**
(`dax=always|never|inode`, `virtio_fs.c:104-140`) and `dax=always` without a
device fails the mount (`:1596-1603`); it reaches `/workspace` because there is
one share, but the window is **whole-share** (2 MiB slots shared by `/nix`,
`~/.cargo` and `target/`). cang never sets `set_dax_window_size`
(`linked.rs:317`), so `shm_size` is `None`. This ticket answers whether it is
worth enabling.

1. **The guest mount path.** `DEFAULT_KERNEL_CMDLINE` is `rootfstype=virtiofs rw
   quiet no-kvmapf` with no `rootflags`. Read `init/init-binary` and the kernel's
   virtio_fs mount path: is the root mounted by the kernel from the cmdline
   (`rootflags=dax=always`) or re-mounted by `krun-init`, and what is the exact
   minimal change that adds `dax=always` only when a window exists? Cite the
   code that would change.
2. **The VMM half.** Window size (`DeviceRequirements.shm_size`), where the
   region is allocated (guest RAM vs a device BAR, `device_builders.rs:1841-1940`,
   `ResolvedShmRegion`), whether it competes with the GPU's own `shm_region`,
   what `map_alignment` the reply must carry (`server.rs:950` replies 0) and
   whether the guest accepts it, and the memory cost of a useful window on a
   cang default guest.
3. **Does the workload benefit?** rustc reads many small files, often once, and
   `mmap`s `rlib`s; DAX pays on repeated mmap of the same pages. Say which part
   of a build benefits, which part *loses* (`setupmapping`/`removemapping` churn
   on a shared 2 MiB-slot window, and what happens when the window is exhausted -
   does it fall back to normal reads or fail?), and how big a window the
   workload would need.
4. **Verdict for ticket 04:** ship, defer pending the pool's numbers, or drop -
   with the reason, and the one measurement that would settle it if deferred.

## Deliverable

`notes/05-dax-window-viability.md`: the answer per item with citations, the exact
code sites that would change on both sides, and the ship/defer/drop verdict.

## Resolution (2026-10-25, pi research child-2)

**Drop DAX - not "later", and not behind the worker pool.** Deliverable:
`../notes/05-dax-window-viability.md` (all four items, code sites on both sides,
a recommended 256 MiB window at 20 GiB with its cost table). The reasons:

- **It is whole-*root*, not whole-share.** DAX is selected per *connection*
  (`dax=always`), and cang's connection is the guest's `/`, so `S_DAX` lands on
  **every** inode including `/nix/store` binaries and shared libraries
  (`dax.c:1296-1325`). `dax=inode`, the one mode that could scope it, needs
  `FUSE_HAS_INODE_DAX` + `ATTR_DAX`, which the fork advertises nowhere.
- **It makes the single worker thread worse, not better.** Every first fault of
  every 2 MiB region of every mapped file becomes a `SETUPMAPPING` round trip on
  the one `FsWorker`, and a slot is consumed per file regardless of size.
- **There is no fallback when the window runs out.** The fault path waits
  uninterruptibly (`dax.c:767-784`), the read/write path waits killably
  (`:987-1033`), reclaim frees at most 10 ranges per pass and only `refcnt == 1`
  ones, so a hot window degenerates into fault -> reclaim -> `REMOVEMAPPING` ->
  refault on one thread.
- **The failure modes are boot-bricking, not degrading:** `dax=always` with no
  window fails the mount (`virtio_fs.c:1596-1603`), and a window not aligned to
  the guest's memory-block size fails the dax probe - a guest that never finds
  its root.
- The windows that would avoid thrash for a real build are multi-GiB on an 18 GiB
  guest, while rustc's own mix (small files read once, `mmap`ped rlibs) is the
  workload DAX is worst at.

**One correction to the note's framing:** it names `WRITEBACK_CACHE` as the lever
"ticket 04 is about to ship". Ticket 03 has since ruled write caching out
entirely (exact host visibility is a stated promise), so that particular
incompatibility is moot - the drop stands on its own reasons above, and it
removes the last caching/`mmap` candidate from the map.

**What would reopen it:** a *data-only*, large-file, re-read share (i.e. the
share split this map rules out of scope), plus FUSE-opcode counts to show the
`SETUPMAPPING`/`FUSE_READ` crossover for this workload.
