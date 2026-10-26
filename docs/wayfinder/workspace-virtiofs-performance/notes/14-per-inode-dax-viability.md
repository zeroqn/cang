# 14 - per-inode DAX viability (CORRECTED 2026-10-26)

> **Correction (2026-10-26, note 18).** This note's *kernel reading* is right but
> its *verdict framing* is wrong. Per-inode DAX does **not** require the host
> filesystem to support DAX: the server `mmap`s the host file's **page-cache
> pages** into the shared window, and the guest sets `S_DAX` purely from the
> server's `FUSE_ATTR_DAX` (`fs/fuse/dax.c:1296-1316` consults only the mount
> option, `fc->dax`, `fc->inode_dax` and the reply flag). So "no file on this
> machine can ever report DAX" and the `statx(STATX_ATTR_DAX)` "deciding check"
> below are both false; bob's recollection of a **size-based** policy
> (`dax=filesize`, `st_size > 32 KiB`, Jeffle Xu's 2021 series) is a real design.
> The disposition stands - **don't ship** - but on benefit, not capability: the
> page cache already wins the reuse case, DAX addresses only first-touch read
> latency, and the ceiling is a quarter-busy worker. See
> [18-per-inode-dax-host-requirement](18-per-inode-dax-host-requirement.md).

---
label: wayfinder:research
title: Is per-inode DAX achievable on cang's root share, and what would "let virtiofs decide per inode" actually require?
status: open
blocked_by: []
claimed_by: pi research child (2026-10-26)
---

# 14 - Per-inode DAX viability

Reopens the question [note 05](05-dax-window-viability.md) closed with "the only
reachable mode is all-or-nothing `dax=always`". That sentence is wrong about the
kernel and only half right about the fork, so this note answers the *per-inode*
form on its own terms: what the guest kernel requires, what the fork would have
to do, what the **host** filesystem has to support, what virtiofsd does, whether
a useful approximation exists, and whether it is worth doing.

Read-only: no source edits, no build, no VM. Every magnitude is a mechanism
argument with a citation, plus three host capability probes.

Sources (identifiers are what I actually read):

| what | where |
|---|---|
| guest kernel | `v7.2` on `git.kernel.org`: `fs/fuse/{dax.c,inode.c,file.c,virtio_fs.c,fuse_i.h}`, `include/uapi/linux/fuse.h`, `fs/dax.c`, `fs/ext4/{super.c,inode.c}`, `fs/btrfs/super.c`, `Documentation/filesystems/dax.rst` |
| kernel as built by cang | `deps/libkrunfw/config-libkrunfw_x86_64-kvm{,-lto}` (linux-7.2.10) |
| the fork | `deps/libkrun` @ `7160f48f` (`v2.0.0-cang.10-2`): `src/devices/src/virtio/fs/{fuse,server,filesystem}.rs`, `.../fs/linux/passthrough.rs`, git history |
| virtiofsd (Rust) | `gitlab.com/virtio-fs/virtiofsd` @ `main`: `src/fuse.rs`, `src/passthrough/mod.rs`, `src/server.rs`, `README.md`, repo tree via the GitLab API |
| virtiofsd (C, QEMU) | `gitlab.com/qemu-project/qemu` @ `v7.2.0`: `tools/virtiofsd/{passthrough_ll.c,helper.c}`; `v8.2.0` has no `tools/virtiofsd` at all |
| upstream per-inode design | kernel patch series v8 (LWN 877014, merged), virtiofsd patch series v6/v7 (`msg04349`), `Documentation/filesystems/dax.rst` "Enabling DAX on virtiofs" |
| this host | `findmnt`, `/proc/mounts`, `/sys/bus/dax`, `/sys/block/*/device/driver`, `/sys/module`, `/proc/modules` |

## Verdict

**No - not worth doing on this host, and the host capability is the *least*
important reason.**

1. **The upstream per-inode policy is dead here on capability grounds, flatly.**
   "Let virtiofs decide per inode" as upstream designed it means: the server
   answers `FUSE_ATTR_DAX` from the **host file's** `STATX_ATTR_DAX` flag. On this
   machine *no file on any mount can ever report DAX* - not one file, not
   per-inode: `/home` and `/nix/store` are ext4 on a SCSI disk (`/dev/sdb2`), the
   other data disk is btrfs, `/sys/bus/dax/devices/` is empty, there is no
   `/dev/pmem*`, no `/dev/dax*`, no pmem/nvdimm/virtio-pmem module, and ext4
   refuses `dax=always` with `"DAX unsupported by block device."`
   ([§5](#5-the-host-side-what-the-host-filesystem-must-support-and-whether-it-can)).
   So the `dax=attr` policy enables DAX on **zero** inodes here.
2. **The mechanism does not need host DAX, so capability alone does not close
   the question.** The window maps the host **page cache**
   (`passthrough.rs:2080-2089`), and the kernel doc leaves the algorithm to the
   server (`dax.rst:171-179`). A size/path policy would work on this box. What
   closes it is the benefit, in [§7](#7-does-the-workload-want-it-no-the-page-cache-already-does-this-job).
3. **The benefit is zero for this workload.** DAX's only edge is the *first*
   touch of a region the guest page cache cannot retain. The guest here has
   18 GiB and the pinned cargo tree fits in it (the `/dev/shm` arm proves it:
   232 s), so repeats are already free page-cache faults with no FUSE traffic at
   all. Meanwhile the cold path *gains* a synchronous, unpipelined round trip per
   2 MiB region on the faulting thread, where today reads are pipelined under
   `ASYNC_READ` + readahead. The map's own decisive number says the same thing
   from the other side: the `fs_worker` is 88% idle, and DAX's unit of progress
   *is* a round trip.
4. **It is cheap to implement and still not worth it.** ~8 files in the fork
   (§3), a window cang must allocate anyway, and a boot-time mount option. Being
   cheap does not make a lever useful; it makes it tempting, which is why the
   cost is sized here.

**The one check that decides the upstream form:** `statx(2)` with
`STATX_ATTR_DAX` on a `/workspace` file on the host - it can only be 0 here, for
the reasons in §5, so `dax=attr` is empty. **The one measurement that would
reopen the mechanism:** per-opcode FUSE accounting in one cold build (FUSE_READ
count/bytes vs page-cache hits, plus the read latency histogram): DAX deserves a
hearing only if the guest page cache is genuinely failing to retain file data,
which the 18 GiB guest says it is not.

## 0. What note 05 got wrong, and what stands

Note 05 §0.2 says "`dax=inode` - the only mode that could scope it - cannot work
here … the fork does neither (it is not in `server.rs:897-908`, and `ATTR_DAX`
does not appear anywhere in `deps/libkrun`)". The conclusion (the fork cannot do
per-inode DAX today) is right; the reasoning is wrong in a way that matters,
because it hid a cheap option:

- `FUSE_HAS_INODE_DAX` **is** in `deps/libkrun`: `fuse.rs:178` (`const
  HAS_INODE_DAX: u64 = 1 << 33;`) and `fuse.rs:432` in the `FsOptions` bitflags.
  It arrived in `9fc024e4 "virtio/fs: add support for extended initial"`
  (Sergio Lopez, 2024-02-12) as part of a mechanical `u32`->`u64` import of
  virtiofsd's flag constants - defined, never used. `ATTR_DAX` genuinely is
  absent (the fork's only attr flag is `ATTR_SUBMOUNT`, `fuse.rs:532`).
- So the missing pieces are *advertisement* (`server.rs:898-908`) and
  *policy + attr flag*, not the bit definition.

Note 05's verdict stands, on benefit grounds, and §7 re-derives it for the
per-inode form. Its boot-bricking argument, however, does **not** carry over:
`dax=inode` without a window is explicitly legal
([§1.2](#12-daxinode-does-not-panic-the-boot-and-silently-means-never)). Its
window-geometry argument does carry over unchanged.

## 1. Kernel side: what `dax=inode` requires

### 1.1 The contract, end to end

| step | code | what it says |
|---|---|---|
| protocol version | `fuse.h:196`, `:486` | `FUSE_HAS_INODE_DAX (1ULL << 33)` and `FUSE_ATTR_DAX` were added in 7.36 |
| the file flag | `fuse.h:286-303`, `:583` | `struct fuse_attr` already carries `__u32 flags`; `FUSE_ATTR_DAX (1 << 1)` lives in it, next to `FUSE_ATTR_SUBMOUNT (1 << 0)` |
| kernel offers the capability | `inode.c:1469-1470` | `if (fuse_is_inode_dax_mode(fm->fc->dax_mode)) flags \|= FUSE_HAS_INODE_DAX` - i.e. the offer is driven by the **mount option**, not by the window |
| ... and `MAP_ALIGNMENT` | `inode.c:1467-1468` | offered only `if (fm->fc->dax)` (a window exists) |
| server echoes it | `inode.c:1353-1359` | `if (flags & FUSE_HAS_INODE_DAX) fc->inode_dax = 1;`. Note `flags2` (where bit 33 lives) is honoured iff the reply set `FUSE_INIT_EXT` (`inode.c:1288-1289`), **not** on the minor version |
| the mode enum | `fuse_i.h:367-376` | `FUSE_DAX_INODE_DEFAULT`, `FUSE_DAX_ALWAYS`, `FUSE_DAX_NEVER`, `FUSE_DAX_INODE_USER`; `fuse_is_inode_dax_mode()` = DEFAULT or INODE_USER |
| mount option | `virtio_fs.c:104-140` | `dax=always\|never\|inode` (`fuse_dax_mode`), legacy `dax` flag = ALWAYS |
| the decision | `dax.c:1296-1316` | `fuse_should_enable_dax()`: NEVER -> false; **`if (!fc->dax) return false;`** (comment: in inode mode a device without DAX *silently* falls back to never); ALWAYS -> true; else `fc->inode_dax && (flags & FUSE_ATTR_DAX)` |
| the effect | `dax.c:1318-1325` | `inode->i_flags \|= S_DAX; inode->i_data.a_ops = &fuse_dax_file_aops;` |
| when it is applied | `file.c:3115-3134` via `inode.c:406-433`, `:496-501` | for `S_ISREG` only, at **inode instantiation** (`fuse_init_file_inode(inode, attr->flags)` on `I_NEW`) |
| a changed mind | `inode.c:388-389` -> `dax.c:1327-1334` | `fuse_dax_dontcache()`: in inode mode, `IS_DAX(inode) != (flags & FUSE_ATTR_DAX)` -> `d_mark_dontcache(inode)` |

Two consequences that shape any implementation:

- **The window is still mandatory.** `fuse_should_enable_dax()` returns false
  whenever `fc->dax == NULL` (`dax.c:1308`). Per-inode DAX is therefore not "DAX
  without a window"; it is "the same window, applied to fewer inodes". Every cost
  in note 05 §2.5 (window size, memory-block alignment, ZONE_DEVICE probe) is
  paid in full.
- **It is a creation-time property.** `fuse_init_file_inode()` runs once per
  inode; a later `GETATTR` cannot flip `S_DAX`. When the hint changes, the kernel
  marks the dentry `DONT_CACHE` and waits for the inode to be evicted at last
  put (this is the design the kernel patchset's own note describes: "the DAX
  state of the file won't be updated until this file is closed and reopened
  later"). A policy must therefore be **stable**; a policy that oscillates is a
  dentry/inode churn generator.

### 1.2 `dax=inode` does not panic the boot, and silently means never

`virtio_fs.c:1596-1604`:

```
if (ctx->dax_mode != FUSE_DAX_NEVER) {
	if (ctx->dax_mode == FUSE_DAX_ALWAYS && !fs->dax_dev) {
		err = -EINVAL;  pr_err("virtio-fs: dax can't be enabled ...");
```

Only `always` is an error without a window (`dax_dev == NULL`). For `inode` the
mount proceeds with `ctx->dax_dev = NULL`, `fc->dax == NULL`, and no inode ever
gets `S_DAX` - the documented silent fallback (`dax.c:1304-1309`). So note 05
§1.2's conclusion ("`rootflags=dax=always` + no window => `-EINVAL` at root mount
=> panic") does not extend to `dax=inode`: an inode-mode mount is *safe to ship
dark*, and a server that then declines every inode costs nothing.

What is unchanged: `virtio_fs_probe()` calls `virtio_fs_setup_dax()` on every
boot (`virtio_fs.c:1163-1165`), independent of the mount option, and that
function returns early when the device has no cache window
(`virtio_fs.c:1092-1098`). So a window that exists but is *misaligned* still
fails the device probe, the tag never appears, and the root mount panics
(note 05 §2.4). The per-inode form buys mount-mode safety, not geometry safety.

### 1.3 What the guest kernel and the fork negotiate today

- Guest: `CONFIG_FUSE_DAX=y`, `CONFIG_DAX=y`, `CONFIG_FS_DAX=y`,
  `CONFIG_ZONE_DEVICE=y` in both `config-libkrunfw_x86_64-kvm` and `-kvm-lto`
  (lines 1025-1028, 2809-2812, 2871-2874, 2886-2892). The guest is not the gap.
- Fork: it does **not** advertise `HAS_INODE_DAX` (absent from
  `server.rs:898-908`) and has no `ATTR_DAX` constant, so the kernel would
  report `fc->inode_dax == 0` even on a `dax=inode` mount, and no inode would be
  `S_DAX`. The protocol machinery on the server side of DAX (`SETUPMAPPING` /
  `REMOVEMAPPING`, gated on `shm_region`) exists and works: `server.rs:150-174`,
  `passthrough.rs:2045-2126`.
- Version quirk worth knowing before anyone reaches for a version bump: the fork
  replies `KERNEL_MINOR_VERSION = 27` (`fuse.rs:15`) yet negotiates `INIT_EXT`
  (`server.rs:908`) and parses `flags2` (`server.rs:862`). Because the kernel
  gates `flags2` on the `INIT_EXT` reply bit (`inode.c:1288-1289`), a bit-33
  capability would be delivered to it as-is. No minor bump is needed.

## 2. Server side: what the fork would have to do

### 2.1 The three edits

1. **Advertise**, only when it can honour the promise. `server.rs:898-908`'s
   `supported` mask is server-level, so the natural place is
   `let mut supported = ... | FsOptions::INIT_EXT;` plus
   `if shm_region.is_some() { supported |= FsOptions::HAS_INODE_DAX; }` - the
   gate matters: with no window the kernel would set `fc->inode_dax = 1` and
   every reply carrying `ATTR_DAX` would then diverge from `IS_DAX`, i.e. a
   `d_mark_dontcache()` on every attribute refresh (`dax.c:1331-1333`), forever.
2. **Add the flag**: `pub const ATTR_DAX: u32 = 1 << 1;` next to
   `ATTR_SUBMOUNT` (`fuse.rs:532`), and set it in the lookup reply's
   `attr_flags` (`linux/passthrough.rs:917-975`, the same block that sets
   `ATTR_SUBMOUNT` at `:971`).
3. **Teach `GETATTR` to carry it**. This is the part that is not obvious and the
   part note 05 never reached: `Entry` has `attr_flags` (`filesystem.rs:49`) and
   the entry reply renders it (`filesystem.rs:70`,
   `Attr::with_flags(entry.attr, entry.attr_flags)`), but `FileSystem::getattr`
   returns `(stat64, Duration)` only (`filesystem.rs:450-457`), so
   `server.rs:238-241` builds the attr reply with `st.into()` ->
   `Attr::with_flags(st, 0)` (`fuse.rs:564`): **flags are always 0 on GETATTR.**
   A fork that sets `ATTR_DAX` at lookup and not at getattr hands the kernel a
   permanent "the hint changed" signal and `d_mark_dontcache()`s every inode it
   just marked DAX.

### 2.2 What that costs, measured against the last time the fork did this

`cff116e6 "virtio/fs: add FUSE ATTR_SUBMOUNT support"` is the exact template: it
touched 5 files, +22/-3 (`filesystem.rs`, `fuse.rs`, both passthroughs,
`server.rs`) to introduce a per-inode attr flag. `ATTR_DAX` needs the same 5 plus
the `getattr` signature, which ripples to every implementation of the trait:
5 `impl FileSystem` files (`linux/`, `macos/`, `windows/` passthrough,
`read_only.rs`, `null_fs.rs`) plus the `augment_fs.rs` wrapper. Call it ~8 files,
~50 lines, all mechanical - small enough that "cost" will never be the reason to
refuse it. Then cang must do its half:

- allocate a window (`krun_fs_device_set_dax_window_size`, note 05 §2.1) - the
  expensive, boot-risky part,
- append `rootflags=dax=inode` to the guest cmdline (note 05 §1.2; the only
  lever, since `virtio_fs_context_ops` has no `.reconfigure`),
- and add the policy, which is the actual design work (§2.3).

### 2.3 The policy is where the design lives, and it is the interesting answer

"Let virtiofs decide per inode" is not a flag; it is a **policy engine in the
VMM**, consulted where `passthrough.rs` builds `attr_flags`, with the freedom the
kernel explicitly grants the server (`dax.rst:173-179`: "whether DAX shall be
enabled or not is completely determined by virtiofs server, while virtiofs server
itself may deploy various algorithm making this decision"). What a correct policy
has to satisfy:

- **stable over an inode's life** (§1.1), so it must be a pure function of things
  that do not change under it (path, extension, size class) - not, say, a read
  counter;
- **identical at lookup and getattr**, or it thrashes dentries;
- **bounded by the window**: a live `S_DAX` region consumes one 2 MiB slot, the
  slot pool has no fallback (`note 05` §3.4), and a fault on an exhausted window
  is an *uninterruptible* wait. With a 256 MiB window that is 128 concurrent
  regions for the whole guest; a policy that marks every file in a 16k-file tree
  DAX turns a stale-mapping inconvenience into a stage-wide stall;
- **never set when no window was advertised** (see §2.1 item 1).

Upstream's own two policies are instructive precisely because one of them is
host-dependent and the other is not (§4).

## 3. What virtiofsd does: nothing, in either daemon

Q3's premise ("is `--dax` mount-wide?") does not survive reading the code.

- **Rust virtiofsd** (`gitlab.com/virtio-fs/virtiofsd` @ `main`): **no DAX of any
  kind.** `src/fuse.rs` carries the protocol constants - `HAS_INODE_DAX` at
  `:197-198`/`:455` and `pub const ATTR_DAX: u32 = 1 << 1` at `:563-564` - and
  the crate has **zero users** of either: `do_lookup()`'s `attr_flags` sets only
  `fuse::ATTR_SUBMOUNT` (`passthrough/mod.rs:919-926`); there is no
  `MAP_ALIGNMENT`, no `SETUPMAPPING`/`REMOVEMAPPING` in `passthrough/mod.rs` or
  `server.rs`; tallies of `dax` across `passthrough/mod.rs` (2794 lines),
  `server.rs` (1825 lines) and `README.md` (682 lines) are 0.
- **QEMU's C virtiofsd** (last in-tree at `v7.2.0`; gone by `v8.2.0` - the path
  404s): also zero - 0 `dax` occurrences in `passthrough_ll.c` (121610 bytes) and
  in `helper.c`. There was never a `--dax` option to be mount-wide.
- **The only per-inode DAX server ever posted** is Jeffle Xu's patchset against
  the C virtiofsd (`[PATCH v7 0/6] virtiofsd: support per inode DAX`, msg04349):
  it added `-o dax=[server|attr]` (v5) and then `-o dax=inode|filesize` (v7), with
  two policies - **xflag** (`statx`'s `STATX_ATTR_DAX` on the host file) and
  **file size** (DAX disabled below 32 KiB, justified by the 32 KiB of `struct
  page` overhead a 2 MiB window chunk costs in the guest). It was never merged,
  and the daemon it patched no longer exists.
- The kernel half **was** merged (that is why the code in §1 exists), together
  with `dax.rst`'s "Enabling DAX on virtiofs".

So a per-inode policy in cang's fork would be the first shipping implementation
of a six-year-old kernel feature. DAX windows in shipped VMMs are a
crosvm/libkrun-lineage feature; the fork inherited the window half from crosvm
and inherited the `HAS_INODE_DAX` *bit* from a header sync, and nobody has ever
connected them.

## 4. What a server-side per-inode policy would actually look like here

Three candidate policies, in increasing order of host dependence:

| policy | rule | on this host | note |
|---|---|---|---|
| `attr` (upstream) | host file has `STATX_ATTR_DAX` | **enables 0 inodes** | requires host DAX; §5 |
| `filesize` (upstream) | `st_size >= 32 KiB` | enables most of the tree | ignores slot count; with a 256 MiB window, 128 regions against 16k files => reclaim storm, no fallback |
| cang-specific | e.g. regular files under one configured subtree, `>= N MiB`, extension-based | expressible | needs a config surface and a stable, coarse rule |

The third is the only one with a defensible shape, and it is exactly the "share
split" that this map already excludes, done at inode granularity instead of mount
granularity. That is a real and elegant point in per-inode DAX's favour: it scopes
DAX to `/workspace/target` (or to `*.rlib`) without a second virtio-fs device, a
second mount, and a second `virtio-fs` queue pair. It is also strictly cheaper
than the share split for the same effect. It just does not help (§6).

## 5. The host side: what the host filesystem must support, and whether it can

### 5.1 The mechanism does not need host DAX - plainly, the *policy* does

This is the distinction that decides how to read bob's question. The kernel's DAX
on virtiofs is **page-cache sharing**, not host-device DAX: the server `mmap`s the
host file `MAP_SHARED | MAP_FIXED` into the window at `host_shm_base + moffset`
(`linux/passthrough.rs:2077-2092`), so the guest's second-level mapping points at
the host's page-cache pages for that file. Nothing in that path asks the host
block device for anything. The kernel doc says so from the other direction:
`dax.rst:173-179` - the server "may deploy various algorithm", *e.g.* "depending
on the persistent `FS_XFLAG_DAX` flag on the host". The host flag is an example
of a policy input, not a requirement of the mechanism.

So: **host DAX is required only by the `dax=attr` policy**, which is upstream's
choice, not the kernel's.

### 5.2 And on this machine, host DAX is impossible for any file

Probes (all read-only, this session):

```
findmnt -T /home/dev/cang/cang   -> /dev/sdb2[/home]  ext4  rw,nosuid,nodev,noatime
/proc/mounts                     -> /dev/sdb2 on / ext4; /nix/store ext4 ro; /home/dev/disp btrfs
/sys/block/sdb/device/driver     -> /sys/bus/scsi/drivers/sd        (sda likewise; both plain SCSI disks)
ls /sys/bus/dax/devices/         -> (empty)
ls /sys/class/dax, /dev/pmem*, /dev/dax*  -> absent / not found
/sys/module, /proc/modules       -> no pmem, nvdimm, virtio_pmem or nd_pmem
```

`/sys/bus/dax` exists only because the kernel is built `CONFIG_DAX=y`; with
`CONFIG_DEV_DAX` unset and no device, nothing is registered on it.

The filesystem side is gated on the **block device**, in both filesystems
candidates:

- ext4: `ext4_should_enable_dax()` requires `EXT4_FLAGS_BDEV_IS_DAX`
  (`fs/ext4/inode.c:5087-5103`); that bit is set only when
  `fs_dax_get_by_bdev()` returned a `dax_device` (`fs/ext4/super.c:4370-4372`,
  `:4788-4793`); `dax=always` on a non-DAX block device is refused outright:
  `"DAX unsupported by block device."` -> `-EINVAL` (`super.c:4795-4805`). With
  no dax device on `/dev/sdb2`, that bit can never be set, so **no inode on
  `/workspace` can ever have `S_DAX`, `FS_XFLAG_DAX` notwithstanding**, and
  `statx(STATX_ATTR_DAX)` is always 0.
- btrfs (`/home/dev/disp`, `/home/dev/.local/share/containers`): zero `dax`
  occurrences in `fs/btrfs/super.c` (v7.2) - btrfs has no DAX support at all, and
  `FS_XFLAG_DAX` is a pure hint even where DAX exists
  (`dax.rst:166-168`: setting it happens even if the media does not support DAX).

**Plain answer to bob's Q2:** the host cannot report DAX per file, and cannot
report it per filesystem either - it has no DAX-capable device, so the
`virtiofsd`/upstream `dax=attr` per-inode policy would mark **zero** inodes DAX
in this repo. That is the check that settles the upstream-shaped version of the
question. It does not settle a size/path policy, which needs nothing from the host
beyond `mmap(MAP_SHARED)`.

Making the host DAX-capable is not a cang change: it would need a pmem/CXL device
(or virtio-pmem from the hypervisor) plus a filesystem created on it, and it would
*not* be needed by the mechanism anyway - it would only make `dax=attr` non-empty.

## 6. If the host capability is missing, is there a useful per-inode approximation?

**No in-kernel one.** The kernel has exactly two ways to point a guest VMA at host
file pages: the DAX window (`S_DAX` + `fuse_dax_file_aops` + `SETUPMAPPING`) and
`FUSE_PASSTHROUGH`, which is unreachable when the FUSE server is the VMM itself
(note 01). There is no "map this file into the window but keep the page-cache
path" mode: `SETUPMAPPING` is issued only from `fs/fuse/dax.c`, and the server
only honours it when `shm_region` exists (`server.rs:150-174`). A server-chosen
window mapping without `S_DAX` is therefore not expressible in this kernel - the
`S_DAX` machinery is not overhead around the feature, it *is* the feature.

The approximations that do exist are not DAX and are already this map's other
levers:

| approximation | what it buys | status |
|---|---|---|
| worker pool / queue depth | removes request serialization: the thing the 88%-idle worker indicts | note 01, ticket 04 |
| `WRITEBACK_CACHE` | write path only; and ticket 03 rules out caching guest writes | decided against |
| tmpfs for build scratch | measured: 232 s vs 299 s | out of scope (not a virtiofs change) |
| `/workspace` on virtio-blk | changes the class of the problem | outside this map |
| a second share with `dax=always` | scoping DAX | excluded by the map; per-inode is strictly cheaper for the same effect |

Cost of the "do it anyway" option, for the record: ~8 files in the fork (§2.2), a
256 MiB window at 20 GiB guest-physical with a 128 MiB-multiple/1 GiB-aligned
geometry constraint (note 05 §2.4), `rootflags=dax=inode` in the launcher, a
policy config surface in cang, one new boot-brick failure mode (window geometry,
unchanged from note 05), and a new uninterruptible stall mode on window
exhaustion that only exists for DAX'd inodes.

## 7. Does the workload want it? No - the page cache already does this job

The measured shape of the penalty (MAP, 2026-10-25/26): host 104 s, guest-local
`/dev/shm` 232 s, guest-virtiofs 299-311 s, with the single `fs_worker` thread at
**12% of one core** while guest vCPUs ran at ~65%. So the FUSE penalty over the
RAM control is 67-82 s, and the worker is idle for ~88% of it: the cost is
per-request round-trip latency, not server CPU or copy bandwidth.

DAX against that:

1. **Cold read-once data gets no cheaper; it gets one new serialization point.**
   `read(2)` under `S_DAX` is `fuse_dax_read_iter()` -> `dax_iomap_rw()`
   (`dax.c:676-693`, `fs/dax.c:1734-1779`), which copies through
   `dax_iomap_iter()` from the window into the caller's iovec - a full memcpy by
   the *guest* CPU, which is already the busy side. Today the same read is a
   `FUSE_READ` of up to 1 MiB (`server.rs:35`, `:933`) issued asynchronously under
   `ASYNC_READ` and readahead, i.e. **pipelined**. DAX replaces that with a
   synchronous `SETUPMAPPING` per 2 MiB region on the faulting thread's critical
   path, and it does not prefetch the next region. For a build that touches 16801
   sources cold, that is a round trip per 2 MiB with *less* overlap than today.
2. **Re-reads are already free.** The guest page cache retains file data across
   processes; `rustc`'s second reader of a shared `.rmeta` pays zero FUSE
   requests. DAX's advantage over a page-cache hit is nothing. It only wins when
   the guest cannot retain the data - and the `/dev/shm` arm demonstrates an 18 GiB
   guest retains this whole tree.
3. **Writes don't benefit at all**: extending writes bypass DAX by design
   (`fuse_dax_write_iter()` -> `fuse_dax_direct_write()` for
   `file_extending_write()`; `dax.c:695-714`, `:737-741`), so `target/` creation
   behaves exactly as today. Non-extending writes go through `dax_iomap_rw` and
   are memcpy'd by the guest, again no round-trip saving versus a pipelined
   `FUSE_WRITE`.
4. **The CPU ceiling is small**: 12% of a core over 299 s is ~36 core-seconds of
   server work in total, so even a lever that eliminated *all* server-side data
   work could not recover the 67 s FUSE delta - the rest is latency, which is what
   the worker pool addresses and DAX does not.
5. **It voids the map's remaining levers for those inodes**: on `S_DAX` inodes the
   aops are `fuse_dax_file_aops` (`noop_direct_IO`/`noop_dirty_folio`,
   `dax.c:1291-1294`), so page-cache policy work stops applying to their data
   (note 05 §3.5).

The one workload shape that would flip this: file data too large for guest RAM,
large files, mapped repeatedly. Then DAX's window is a genuine second cache, and
the 2 MiB slot count is the constraint to size for. Nothing in this map's
workloads (2401-crate synthetic tree, cang's own tree) looks like that.

## 8. Verdict

**No.** Per-inode DAX is *achievable* (≈8 files in the fork, one window, one
`rootflags=` fragment) and it is the *right* shape if DAX is ever wanted, because
it is the cheapest way to scope a window to a subtree. But on this host the
upstream policy is empty by capability (`dax=attr` -> zero inodes: no DAX device
on the machine, ext4 gates DAX on the block device, btrfs has none), and any
replacement policy is worthless for this workload because the guest page cache
already serves every re-read for free, the cold path today is pipelined while
DAX's is not, and the entire server-side budget to win is ~12% of one core.

- **The deciding capability check:** `statx(STATX_ATTR_DAX)` on a `/workspace`
  file on the host. It is 0, permanently, on this machine (§5.2) - so
  "let virtiofs decide per inode" as designed upstream decides "no" for every
  file, and any other policy is cang inventing one.
- **The one measurement that would reopen it:** per-opcode FUSE accounting in one
  cold build (`FUSE_READ` count and bytes, page-cache hit ratio, read latency
  distribution). If the guest page cache were failing to retain file data, DAX
  would have a case; on an 18 GiB guest with a tree that fits in `/dev/shm` it is
  not.
- **What to do instead:** nothing new. The map's ordering stands (worker pool for
  the measured latency, cache-coherence contract already decided), and if the
  ~30% has to disappear rather than be explained, the levers are outside this
  map: `/workspace` on a block device, or memory-backed scratch.
