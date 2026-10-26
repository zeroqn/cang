---
label: wayfinder:research
title: Can cang drive the share through /dev/fuse, or build its own passthrough? /dev/fuse, DAX and the designs left
status: open
blocked_by: []
claimed_by: pi research child (2026-10-26)
---

# 24 - Home-grown passthrough: what /dev/fuse can and cannot reach, and what cang could build

Read-only: no build, no VM boot (a sibling session is running benchmark arms).
Kernel sources are `linux` tag **v7.2.10** - the pinned guest kernel,
`deps/libkrunfw/Makefile:1` (`KERNEL_VERSION = linux-7.2.10`) - fetched from
`git.kernel.org` `plain/?h=v7.2.10` and quoted by `grep -n` line number; the VMM
is the fork's `deps/libkrun/src/devices/src/virtio/fs/`; the guest config is
`deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto`.

## 0. Correction first: `/dev/fuse` **does** exist in the guest

An earlier statement in this session - and the *phrasing* of note 22 §3.1's
title - says a virtio-fs guest "does not have a `/dev/fuse` file". **That is
wrong, and it was wrong when written.** The pinned guest config has

- `CONFIG_FUSE_FS=y` (`config-libkrunfw_x86_64-kvm-lto:2889`),
- `CONFIG_DEVTMPFS=y` (`:1411`) and `CONFIG_DEVTMPFS_MOUNT=y` (`:1412`), so
  devtmpfs is mounted on `/dev` before init runs.

`CONFIG_FUSE_FS=y` links `fuse_dev_init()`, which registers a misc device named
`"fuse"` (`fs/fuse/dev.c:2400-2419`, `.name = "fuse"` at `:2402`,
`misc_register` at `:2415`), and devtmpfs creates a node for every registered
misc device. So `/dev/fuse` is present, openable, and `open()` on it succeeds in
a cang guest.

Note 22 §3.1's **conclusion** stands (FUSE_PASSTHROUGH is unreachable), but its
**premise** is the wrong obstacle, and the real obstacle is stronger: there is no
node problem at all, there is no *file that maps to the virtio-fs connection*.
The difference matters, because "no node" is a one-line config fix while the real
obstacle is a property of the design (`fuse_dev` is a kernel object, not an
inode).

## 1. Why a virtio-fs connection cannot be driven through `/dev/fuse`

### 1.1 What the virtio-fs driver does with its `fuse_dev`s

1. **The driver allocates its own `fuse_dev` objects, one per virtqueue, in the
   super-block fill.** `virtio_fs_fill_super()` loops `fs->nvqs` times calling
   `fuse_dev_alloc()` (`fs/fuse/virtio_fs.c:1588-1594`), then - only after
   `fuse_fill_super_common()` (`:1605`) - installs each one onto the new
   connection's channel: `fuse_dev_install(fsvq->fud, fc->chan)`
   (`:1609-1613`).
2. **`fuse_dev_alloc()` is a plain `kzalloc`.** `fuse_dev_alloc_no_pq()`
   (`fs/fuse/dev.c:426-438`) allocates a `struct fuse_dev`, sets a refcount and
   inits the processing queue; `fuse_dev_alloc()` (`:440-451`) adds a prealloc'd
   pqueue. Nothing in it registers a device, creates an inode, or records a
   path. `struct fuse_dev` itself has exactly five members - `ref`, `sync_init`,
   `chan`, `pq`, `entry` (`fs/fuse/fuse_dev_i.h:298-313`) - and no `inode`,
   no `file`, no `cdev`.
3. **`fuse_dev_install()` only links the object into the channel.**
   `fuse_dev_install_with_pq()` cmpxchg's `fud->chan = fch`
   (`fs/fuse/dev.c:468`), `list_add_tail(&fud->entry, &fch->devices)` (`:482`)
   and takes a connection reference (`:483`). The `fud` remains a kernel object
   with no userspace handle.
4. **The driver then plays the server itself, through virtio.** A request is
   pushed into the virtqueue - `virtqueue_add_sgs(vq, ...)`
   (`fs/fuse/virtio_fs.c:1461`), `fpq = &fsvq->fud->pq` (`:1468`),
   `virtqueue_kick_prepare` (`:1479`) - and the reply arrives by the device's
   `used` ring callback (`virtio_fs_vq_done`, registered at
   `:986`/`:996` inside `virtio_fs_setup_vqs`, `:950-1007`, `virtio_find_vqs` at
   `:1000`). The "userspace side" of the FUSE protocol is not a file at all: it
   is a virtqueue pair, i.e. our Rust server in the VMM.

So a virtio-fs connection's `fud`s are reachable only from the kernel code that
created them. There is no fs, no char device, no ioctl on the mount, and no
sysfs attribute that hands one out.

### 1.2 What a userspace FUSE server needs, and why only the misc device provides it

The control plane for a FUSE server is the *fuse device file*, and every entry
point recovers its `fud` from the **file**:

- `fuse_dev_open()` (`fs/fuse/dev.c:1651-1660`) allocates a *fresh* `fud` per
  `open()` and stores it in `file->private_data` (`:1658`). The new `fud` has
  `chan == NULL`.
- `fuse_get_dev(file)` (`:1662-1678`) reads that `fud`
  (`fuse_file_to_fud`, `fs/fuse/fuse_dev_i.h:350-353` - literally
  `file->private_data`) and returns `-EPERM` unless the `fud` already has a
  channel (`:1667-1670`).
- The data plane is the read/write pair on the same file: `fuse_dev_read()`
  (`:1680-1695`) requires a user-backed iov (`:1689-1690`) and calls
  `fuse_dev_do_read()` (`:1516-`), which takes the request from the channel's
  input queue and moves it to `fpq->io` (`:1577-1600`). That is the machinery a
  server thread blocks in.

The two ioctls a passthrough server needs are gated on exactly that `fud`:

- **`FUSE_DEV_IOC_BACKING_OPEN`** → `fuse_dev_ioctl_backing_open()`
  (`fs/fuse/dev.c:2299-2315`): `fud = fuse_get_dev(file)` (`:2302`), then
  `fuse_backing_open(fud->chan->conn, &map)` (`:2314`). The connection is taken
  *from the fud's channel*, i.e. from the file the server opened.
- `fuse_backing_open()` (`fs/fuse/backing.c:83-139`) then requires
  `fc->passthrough` and `CAP_SYS_ADMIN` (`:92-95`), resolves the **caller's**
  fd with `fget_raw(map->fd)` (`:101`), requires a regular file (`:107-109`), and
  rejects a backing super-block at least as deep as the FUSE mount
  (`:111-114`).
- **`FUSE_DEV_IOC_CLONE`** (`fs/fuse/dev.c:2264-2297`) is the only other way to
  give a `fud` a channel, and it requires the source fd to be a *fuse device
  file* whose `fud` already has a channel - same `f_op` (`:2281`) then
  `fuse_dev_install_with_pq(new_fud, fud->chan, pq)` (`:2293`). For virtio-fs
  there is no such source fd to clone *from*.

### 1.3 The precise answer

The obstacle is **(b): there is no file descriptor that maps to the virtio-fs
connection** - not (a) a missing `/dev/fuse` node, and not (c) anything exotic.
A guest can open `/dev/fuse` as many times as it likes; each `open()`
(`dev.c:1651-1660`) produces a `fud` with `chan == NULL`, no virtio-fs code path
will ever install the virtio-vs `fuse_chan` into it (`virtio_fs.c:1612` installs
into the driver's *own* `fud`s), and every server entry point
(`fuse_get_dev`, `dev.c:1662-1678`) refuses a channel-less `fud` with `-EPERM`.
`FUSE_DEV_IOC_SYNC_INIT` (`dev.c:2334-2343`) only makes such an fd *wait* for an
install that will never come.

### 1.4 And a second, ordinary `/dev/fuse` mount could not serve the host's files

Suppose a guest mount did `mount -t fuse` against its own userspace server. Every
FUSE operation that server answers is expressed in **its own namespace**: to
answer `FUSE_LOOKUP`/`FUSE_READ` it must itself `open()`/`stat()` the path. In a
cang guest the only place the host's bytes exist is the root virtio-fs mount, so
the server's own `open()`/`read()` goes straight back over virtio - one round trip
replaced by one round trip plus a userspace hop. There is no
"here is the host's file identity, resolve it remotely" primitive in the FUSE
protocol: a server always resolves paths locally, which is why `virtiofsd` is a
*host* process.

`FUSE_PASSTHROUGH` is the one shortcut that would avoid that, and it is
self-defeating here for a second, independent reason: the only guest-visible
regular file with the host's bytes is a file **on the virtio-fs mount itself**,
and `fuse_backing_open()` rejects a backing super-block whose `s_stack_depth` is
`>= fc->max_stack_depth` (`backing.c:111-114`) - the virtio-fs mount's depth is
exactly `max_stack_depth` (`fs/fuse/inode.c:1396-1398`). Passthrough to the same
mount is refused by design. (`FUSE_PASSTHROUGH` *is* negotiable on a virtio-fs
connection in principle - the kernel offers it in `FUSE_INIT`
(`inode.c:1479-1480`) and accepts it if the server replies with the flag plus a
`max_stack_depth` (`:1391-1399`); the fork's initialiser replies without it,
`deps/libkrun/src/devices/src/virtio/fs/server.rs:1067-1085`,
`map_alignment: 0` at `:1081`. The blocker is not the negotiation, it is that no
`backing_id` can ever be created.)

## 2. The design space, with the mechanism each one needs

| design | mechanism (what exists today) | guest kernel work | host / VMM work | what it removes | ceiling |
|---|---|---|---|---|---|
| **(a) Shared data window** (virtio-fs DAX) | **Fully plumbed in the fork, never enabled by cang**: `FsDevice::set_dax_window_size` (`deps/libkrun/src/libkrun/src/api/device_builders.rs:660-666`) → `DeviceRequirements.shm_size` → `create_fs_region` (`.../vmm/builder.rs:2079-2088`) → guest probe remaps it as the DAX device (`virtio_fs.c:1076-1140`, `virtio_get_shm_region(..., VIRTIO_FS_SHMCAP_ID_CACHE)` at `:1093`) → `FUSE_SETUPMAPPING` (`dax.c:181-230`) → host `mmap(MAP_SHARED\|MAP_FIXED)` of the **host file** over that window (`linux/passthrough.rs:2272-2316`, mmap at `:2306-2316`) | none: `CONFIG_FUSE_DAX=y` (`config:2892`) + `rootflags=dax=always` on the cmdline (`deps/libkrun/src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:5-7` mounts `rootfstype=virtiofs`; note 05 §1.2) | one `set_dax_window_size()` call in `crates/cang/src/runtime/vm/libkrun/launcher.rs` (next to `:305-340`, where there is none today) + the cmdline fragment | every **data** round trip on an `S_DAX` inode: `read()` → `dax_iomap_rw` (`dax.c:676-693`), non-extending `write()` → `dax_iomap_rw` (`:716-750`), `mmap` faults → `dax_iomap_fault` (`:753-795`) | read-once data only; costs one `SETUPMAPPING` round trip per 2 MiB region per inode (`dax.c:20-22`, `:200`), no fallback when the 2 MiB slots run out (note 05 §3.4), applies to the **whole root** because DAX is per-connection and the only mount option is `dax` (`virtio_fs.c:116-120`) |
| **(b) A new "file handle" virtio device** | the shape cang already carries for GPU zero-copy: an out-of-spec feature bit + a control command + driver support (`deps/libkrunfw/patches/0018-…-CREATE_GUEST_HANDLE….patch:33,131`, `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`) | **a new driver** (a virtio device id, a feature bit, a request/reply ring, and a VFS object to attach the pages to) - i.e. our own kernel patch, as with the 23 carried patches | a new `virtio/` device in the fork, a host `mmap` of each file's page-cache pages into a memfd/udmabuf per handle, plus lifetime/revocation | data round trips **plus possibly `open`**, if handles were also handed out for namespace objects | still needs the metadata protocol: `lookup`/`getattr`/`readdirplus`/`create`/`unlink` are 100% of the non-data requests, and DAX already removes the data ones; so it only wins where DAX's *fixed window* is the binding constraint |
| **(c) `FUSE_PASSTHROUGH` over virtio-fs** | kernel-side bypass, merged upstream (`CONFIG_FUSE_PASSTHROUGH=y`, `config:2893`; negotiation `inode.c:1391-1399`, `:1479-1480`; `fuse_backing_open`, `backing.c:83-139`; `fuse_passthrough_read_iter` etc., `fuse_i.h:1304-1324`) | **structurally unavailable, not "unimplemented"** (see §1.3-§1.4): needs a `/dev/fuse`-of-that-connection fd (`dev.c:2299-2315`) *and* a guest-visible backing file on a shallower stack (`backing.c:107-114`) - the guest has neither | none worth writing: the plumbing would be a char device per connection, and the fd would still be unusable | would remove `open`+`read`+`write` round trips *if* the guest already had the file locally - which is the case `passthrough` was designed for (server and client sharing one namespace), and the case cang does not have | n/a in cang |
| **(d) Something the virtio-fs spec defines but nobody implements** | nothing: the device's entire non-FUSE surface is `struct virtio_fs_config { tag[36]; num_request_queues }` plus **one** shared-memory-region id, `VIRTIO_FS_SHMCAP_ID_CACHE 0` (`include/uapi/linux/virtio_fs.h`, whole file, 22 lines). `FUSE_SETUPMAPPING`/`FUSE_REMOVEMAPPING` are the only non-upstream-FUSE opcodes, and they exist solely to back that window (`dax.c:181-230`, `:232-285`). There is no file-handle, passthrough, or inode-identity extension to implement. | none | none | - | - |

Not on the list, and why: **virtio-9p** (a different, older protocol - same
round-trip class, worse caching); **a block-device share** (cang already passes
virtio-blk disks with `set_parallel_reads`, but a host *directory* is not a
filesystem image, and cang's contract is "the host's files, live" - it would
break the contract rather than the cost); **NFS/9p over virtio-vsock** (the one
real alternative namespace bridge: a host daemon, RPC framing and a socket round
trip on top of the same metadata work, plus a second moving part in the host
process model); **FUSE-over-io_uring** (note 22 §3.2: same fuse-device
requirement as (c)).

### 2.1 What (a) actually gives the guest, precisely

- **`read()` and `write()` both go through the window** - not just `mmap`.
  `fuse_dax_read_iter` (`dax.c:676-693`) and `fuse_dax_write_iter` (`:716-750`)
  call `dax_iomap_rw`, which resolves the file offset to `iomap->addr =
  dmap->window_offset + offset` (`fuse_fill_iomap`, `:395-427`) and memcpy's into
  or out of the window. Only a **file-extending** write falls back to the FUSE
  path (`file_extending_write`, `:695-702` → `fuse_dax_direct_write`, `:704-714`).
- **Under `S_DAX` the guest has no page cache for that inode**: `i_data.a_ops`
  becomes `fuse_dax_file_aops` with `noop_dirty_folio` (`dax.c:1291-1294`,
  applied at `:1318-1325`).
- **The window's pages *are* the host's page cache**, so host visibility is
  immediate and unchanged for cang's rules: the server maps the host file
  itself, `mmap(..., MAP_SHARED|MAP_FIXED, fd, foffset)` over the shared region
  (`linux/passthrough.rs:2306-2316`), and the guest's writes land in those pages.
  A host reader sees them at once; a host `O_DIRECT` reader, which bypasses the
  page cache, would not - that is the only visibility caveat, and it is the same
  caveat the current `FUSE_WRITE` path has.
- **Cost per first touch**: one synchronous `FUSE_SETUPMAPPING` on the faulting
  thread per 2 MiB (`dax.c:181-230`; `len = FUSE_DAX_SZ` at `:200`, `fh = -1` at
  `:198`), plus `FUSE_REMOVEMAPPING` traffic when slots are reclaimed. Note 18
  §2.2 priced it as "one round trip per 2 MiB region"; note 05 §3.4 showed
  exhaustion is a **wait**, not a fallback.
- **Scope is the whole connection**, and the only mount parameter is `dax`
  (`virtio_fs.c:116-120`) - there is no `subpath`, so a second mount of the same
  tag cannot be aimed at `/workspace` alone; and cang exports **one** share
  covering the entire guest root (`launcher.rs:298-340`,
  `CANG_FS_ROOT_TAG` over `config.task_rootfs`), with `/workspace-src` as an
  overlay entry inside it.

## 3. Cost/benefit, honestly

Baseline from the map: on a 300 s cargo build the whole share costs **~70 s**
against a **232 s** no-FUSE floor (guest-local `/dev/shm`). The per-op table is
note 20's (1 thread): `open` **934 us**, `read` **1189 us**, `stat` **357 us**,
`readdirplus` **14.9 ms/scan**; at 64 threads `read` is 23.1 ms/op with 2773
ops/s, i.e. the wall time is a round trip and not the server (`fs worker` 26% of
one core at 64 threads, 12% during a build).

| design | effort | removes (measured class) | expected share of the 70 s |
|---|---|---|---|
| (a) DAX window | **a weekend** - one call + one cmdline fragment in cang, the fork is complete (`builder.rs`, `server.rs`, `passthrough.rs` all present); the boot-bricking geometry and the no-fallback stall are the inherited risks (note 05 §2.4, §3.4) | `FUSE_READ` on `S_DAX` inodes, non-extending `FUSE_WRITE`, mmap faults | the **read-class share only**. Note 18 §5.2 priced it at "low single-digit seconds on a 299 s build, plausibly negative", because DAX trades two pipelined reads per 2 MiB for one *synchronous* mapping and the guest page cache already absorbs every repeat (the 18 GiB guest holds the tree - that is what the 232 s arm proves) |
| (b) file-handle device | **a kernel project** - new device + driver + VFS glue, 3-6 weeks, and a permanent fork patch to maintain | the same data round trips as (a), possibly `open` | bounded above by (a) on the data half and by "metadata requests" on the rest; `lookup`/`getattr`/`readdirplus`/`create`/`unlink` are untouched by both. It only pays if a *fixed* 2 MiB-slot window is the binding constraint, which nothing measured |
| (c) passthrough over virtio-fs | **not buildable** - §1.3/§1.4: no fd for the connection, and no guest-visible backing file that is not refused by `backing.c:111-114` | n/a | 0 |
| (d) spec extension | **nothing there** | - | 0 |

**Was "DAX is not worth it" right for the reason given?** Note 05's verdict was
right, and its *reasoning* was mostly right, with one part corrected later:
note 05 did not require host DAX, but it also framed the objection as scope
("one share as `/` covers `/nix` too"). Note 18 §5.1-§5.2 corrected the part that
mattered: the mechanism needs **no host DAX at all** (the window maps the host
*page cache*, `passthrough.rs:2306-2316`; `dax=attr` is the only policy that would
consult `STATX_ATTR_DAX`), so the decision rests on **benefit**, not capability.
That distinction survives this note: nothing in the guest-side or passthrough
space changes the benefit, and every design in §2 has the *same numerator* - the
read-class share of the 70 s - because none of them can remove metadata requests
without becoming a new filesystem protocol.

The hard ceiling to keep in view: even if **every** data round trip vanished, the
`open` row alone (two requests per file open, note 22 §1.1) plus `lookup`/
`getattr`/`readdirplus` would remain, and note 21 already measured what a full
build does with a *perfectly parallel* server: **-3.4%, i.e. noise**. That is the
empirical answer to "is the FUSE round trip the critical path of a cang build":
it is worth ~70 s, but it is spread across a request mix that no data-path design
touches end to end.

## 4. The one experiment that would settle it

**One VM arm, no code: run the existing cang-tree build with the fork's opcode
histogram on.** `CANG_FS_STATS=1` (`crates/cang-libkrun/src/lib.rs:66`) turns on
counting in `crates/cang/src/runtime/vm/libkrun/launcher.rs:93-98, 331-339`; the
server counts every opcode (`server.rs:104-157`, 64 slots, milestones at 5 k,
20 k, 100 k, 300 k, 1 M, 3 M, ... at `:112-118`) and logs the histogram, most
frequent first, at each milestone and at teardown
(`server.rs:161-176`, `worker.rs:550-561`) - at `warn`, because cang floors
libkrun's level there, so it survives a default launch. The arm is the existing
`rust-compile-bench` cang-tree workload; the deliverable is one table of
`FUSE_READ`, `FUSE_WRITE`, `OPEN`, `LOOKUP`, `GETATTR`, `READDIRPLUS` counts out
of one cold build.

Decision rule, fixed **before** the run:

1. Compute the addressable share = (read-class requests) x (note 20's per-op
   cost at the build's achievable concurrency), against the measured 70 s.
2. If read-class requests are **not the plurality** of the mix, then every design
   in §2 is capped below the noise floor of the map's own spread (~5%,
   note 21 §3.1) - **close the whole design space** (a)-(d), write it in the
   map's Decisions, and spend the map's remaining effort on request-count levers
   (note 22 §2) instead.
3. If reads do dominate, run note 18 §5.3's A/B (`rootflags=dax=always` + a
   256 MiB window vs today, same tree, cold), which is the *cheapest* member of
   the family and bounds (b) from above. Only if that wins does (b) get a
   design document - and if it wins, the win is available for a weekend's work,
   not a kernel project.

Why this settles it rather than deferring it: all four designs in §2 spend their
effort on the same requests. Counting those requests once converts an open-ended
"could we build passthrough?" into a number that either clears the noise floor
or does not.

## 5. What this note does not settle

- The **opcode mix** (that is §4) and therefore the exact addressable seconds.
- Whether a window's *geometry* can be made boot-safe on a cang guest with a
  dynamic RAM size (note 05 §2.4's memory-block alignment trap) - unchanged by
  this note.
- Whether `dax=always` regresses a build by losing readahead on the cold path
  (note 18 §5.2's argument); only §4's step 3 measures it.

## Sources (read this session, with the line ranges relied on)

Kernel, `stable` `v7.2.10` via `git.kernel.org/.../plain/?h=v7.2.10`:
`fs/fuse/virtio_fs.c` (116-120, 929-1007, 1028-1140, 1430-1543, 1552-1626),
`fs/fuse/dev.c` (420-511, 1516-1600, 1651-1678, 2215-2251, 2264-2315,
2334-2366, 2379-2419), `fs/fuse/fuse_dev_i.h` (295-313, 344-363),
`fs/fuse/inode.c` (1340-1399, 1465-1490, 1745-1795), `fs/fuse/backing.c`
(83-139), `fs/fuse/dax.c` (20-22, 181-285, 395-427, 660-795, 1195-1344),
`include/uapi/linux/virtio_fs.h` (whole file).

Fork (`deps/libkrun`): `src/devices/src/virtio/fs/{linux/passthrough.rs,
server.rs, worker.rs, device.rs, fuse.rs}`,
`src/libkrun/src/api/device_builders.rs:600-666`,
`src/libkrun/src/vmm/builder.rs:2070-2098`,
`src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:1-18`.

cang: `crates/cang/src/runtime/vm/libkrun/launcher.rs:60-105, 280-345`,
`crates/cang-libkrun/src/lib.rs:66`, `crates/cang-libkrun/src/linked.rs:290-375`.

Guest config: `deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto:1411-1413, 2812-2813,
2874-2875, 2889-2894`; `deps/libkrunfw/Makefile:1-2`;
`deps/libkrunfw/patches/0018-…-CREATE_GUEST_HANDLE….patch:33,131`.

Map notes: 05 (§2.4, §3.4, §5), 14 (§1.2, §2.1), 18 (§1.1, §2.2, §2.3, §5.1-5.3),
19, 20 (§2 tables), 21 (§3.1), 22 (§1.1, §2, §3.1-3.2, §4).
