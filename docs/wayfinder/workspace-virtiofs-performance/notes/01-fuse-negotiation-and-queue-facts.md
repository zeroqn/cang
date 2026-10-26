---
label: wayfinder:research
title: What can this guest kernel and the fork's FUSE server actually negotiate, and where does upstream virtiofsd differ?
status: open
blocked_by: []
claimed_by: pi research child (2026-10-25)
---

# 01 - FUSE negotiation, queue facts, and the lever ordering

Ticket: `tickets/01-fuse-negotiation-and-queue-facts.md`.

Read-only: this note changed nothing in the tree and ran no build and no VM.
Two things were *not* done on purpose - no `cargo` and no guest boot - so every
magnitude below is a mechanism argument, not a measurement. Ticket 02's harness
is what turns these into numbers.

Sources (identifiers are what I actually read):

| what | where |
|---|---|
| the fork's virtio-fs device | `deps/libkrun` @ `79228a72` (branch `cang-rebase-20261020`, tag `v2.0.0-cang.10`), `src/devices/src/virtio/fs/{mod,device,worker,server,fuse,augment_fs}.rs`, `.../fs/linux/passthrough.rs` |
| fork device plumbing | `deps/libkrun/src/devices/src/virtio/{device.rs,pci.rs}`, `src/libkrun/src/api/device_builders.rs` |
| cang's call site | `crates/cang-libkrun/src/linked.rs`, `crates/cang/src/runtime/session/mod.rs` (vCPU count) |
| guest kernel, upstream | `v7.2` tree on `git.kernel.org` (`fs/fuse/virtio_fs.c`, `inode.c`, `dir.c`, `file.c`, `dev.c`, `dev_uring.c`, `dax.c`, `fuse_i.h`, `include/uapi/linux/fuse.h`, `drivers/virtio/*`) |
| guest kernel, as built | `deps/libkrunfw/Makefile` (linux-7.2.10 + linux-hardened v7.2.10-hardened1), `deps/libkrunfw/config-libkrunfw_x86_64-kvm` and `-kvm-lto` |
| upstream virtiofsd | `gitlab.com/virtio-fs/virtiofsd` @ `main`: `README.md`, `src/vhost_user.rs`, `src/server.rs`, `src/main.rs`, `src/passthrough/mod.rs`, `src/passthrough/inode_store.rs` |
| upstream libkrun (for "is this a fork regression?") | `github.com/containers/libkrun` @ `main`, `src/devices/src/virtio/fs/{mod,device}.rs` |

Line numbers are stable for the refs above; the kernel refs are `v7.2`, and the
pinned tree is `linux-7.2.10` (+ `linux-hardened` v7.2.10-hardened1,
`Makefile:1-6`). The ticket says "linux 7.2.7"; the pin moved to 7.2.10, which
does not change anything cited here (every line was read in `v7.2` and the
2.10 stable delta touches none of these files' FUSE logic).

## Verdict

**Three of the six candidate levers are already at their ceiling, one is
structurally unreachable, one is a large speculative project, and two are real.**
The fork already negotiates essentially every "free" FUSE_INIT flag - the map's
"advertised FUSE_INIT set is minimal: `<DO_READDIRPLUS | READDIRPLUS_AUTO>`"
starting fact is **wrong** - so the flag lever is nearly spent; `FUSE_PASSTHROUGH`
and `FUSE_IO_URING` cannot work when the FUSE server is the VMM; what is actually
left is **writeback cache** (a one-line fork default with a real coherence
price) and **parallel FUSE dispatch** (the only structural win, and cheaper than
the map feared: a worker pool over the existing single request queue, exactly
what upstream virtiofsd does with `--thread-pool-size`, needs no virtio header,
feature or queue-count change).

## 0. Corrections to the map's verified starting facts

Two of the map's bullets do not survive reading the code. Both matter for
ticket 04, because ticket 04's "flag set" lever was scoped against them.

1. **The FUSE_INIT set is not minimal.** The map read
   `linux/passthrough.rs::init` (`:899`, not `:897`) and stopped there. That
   function only decides the *filesystem-specific* flags; the reply is the
   intersection of the kernel's offer with `want | supported`, where `supported`
   is a server-level mask in `server.rs:897-908`:

   ```rust
   897         // These fuse features are supported by this server by default.
   898         let mut supported = FsOptions::ASYNC_READ
   899             | FsOptions::PARALLEL_DIROPS
   900             | FsOptions::BIG_WRITES
   901             | FsOptions::AUTO_INVAL_DATA
   902             | FsOptions::ASYNC_DIO
   903             | FsOptions::HAS_IOCTL_DIR
   904             | FsOptions::ATOMIC_O_TRUNC
   905             | FsOptions::MAX_PAGES
   906             | FsOptions::SUBMOUNTS
   907             | FsOptions::HANDLE_KILLPRIV_V2
   908             | FsOptions::INIT_EXT;
   ...
   918         let flags_64 = ((flags2 as u64) << 32) | (flags as u64);
   919         let capable = FsOptions::from_bits_truncate(flags_64);
   ...
   937         let enabled = (capable & (want | supported)).bits();
   ```

   (verbatim from `server.rs:897-908`, `:918-919`, `:937`.) `want` is what
   `passthrough.rs::init` asked for, `supported` this mask, `capable` the
   kernel's offer, so the reply is the intersection. The guest offers all of
   these
   (`inode.c:1451-1465`, plus SUBMOUNTS at `:1472-1473`), so cang's guest
   negotiates ASYNC_READ,
   ATOMIC_O_TRUNC, PARALLEL_DIROPS, AUTO_INVAL_DATA, ASYNC_DIO, HAS_IOCTL_DIR,
   BIG_WRITES, MAX_PAGES, SUBMOUNTS, HANDLE_KILLPRIV_V2 and INIT_EXT **today**,
   plus DO_READDIRPLUS/READDIRPLUS_AUTO from `passthrough.rs:899`, plus
   ALLOW_IDMAP (`server.rs:910-912`, and `FsDevice` always builds with
   `PermissionSemantics::LinuxComplete`, `device_builders.rs:671-681`). ALLOW_IDMAP
   only survives the kernel's own consistency check because a virtio-fs mount
   forces `default_permissions = 1` (and `allow_other = 1`,
   `max_read = UINT_MAX`) before the connection is created
   (`virtio_fs.c:1552-1562`, `:1573`); that also means the guest never sends
   FUSE_ACCESS - the VFS checks the server-returned modes itself, which is one
   *fewer* round trip per `access(2)`/`stat`-then-open for free. What is
   *not* negotiated is a short list: WRITEBACK_CACHE (`cfg.writeback` is false),
   the zero-message-open pair (NO_OPEN_SUPPORT / NO_OPENDIR_SUPPORT),
   CACHE_SYMLINKS, EXPLICIT_INVAL_DATA (mutually exclusive with the
   AUTO_INVAL_DATA the fork already sets), MAP_ALIGNMENT (kernel only offers it
   when DAX is on, `inode.c:1466-1471`), DONT_MASK, POSIX_ACL, the locking
   flags, SPLICE_*, SECURITY_CTX (macOS only) and PASSTHROUGH/OVER_IO_URING.

2. **"writeback default false" is right, but it is the only lever in the FUSE_INIT
   family that is off and worth turning on** - see §2 and §5. Everything else in
   that family is either already on or is real server work, not a flag.

Everything else the map verified (one queue, `num_request_queues = 1`,
`VIRTIO_F_MULTIQUEUE` never offered, one `FsWorker` thread, DAX window plumbed
but never enabled) checks out, with these sharpened numbers:

- `defs::NUM_QUEUES = 2`, `QUEUE_SIZE = 1024` (`fs/mod.rs:45-47`);
  `avail_features = VIRTIO_F_VERSION_1 | VIRTIO_RING_F_EVENT_IDX`
  (`fs/device.rs:73`); `config.num_request_queues = 1` (`:78`);
  `queue_config()` returns the static array (`:173-175`).
- One `FsWorker` thread named `"fs worker"` (`worker.rs:148-153`) epolls the
  hiprio and request eventfds (`:156-176`) and calls
  `server.handle_message` inline in `process_queue` (`:234-268`).
- `attr_timeout = 5 s`, `entry_timeout = 5 s`
  (`fs/device.rs:80-86` + `linux/passthrough.rs:365-380`), applied on every
  LOOKUP reply (`passthrough.rs:1011-1018`).
- `FsDevice::set_dax_window_size` exists (`device_builders.rs:659-666`) and
  `crates/cang-libkrun/src/linked.rs:317-326` never calls it, so `shm_size` is
  `None` for cang's share (`device_builders.rs:686`).
- Upstream `containers/libkrun` `main` is identical here: `NUM_QUEUES = 2`,
  `num_request_queues = 1`, same `avail_features`. **Nothing about the queue
  model is a cang-fork regression**; it is inherited crosvm-era code. (The fork's
  newest commit, `79228a72 "virtio/blk: serve guest reads in parallel by
  default"`, parallelised the *block* path - the virtio-fs path was not touched.)

## 1. The guest side: how the pinned kernel sizes and uses this device

### 1.1 Request-queue sizing: device config, then `nr_cpu_ids`; there is no module param

`virtio_fs_setup_vqs` is the whole story (`fs/fuse/virtio_fs.c:950-1010`):

```c
 963  virtio_cread_le(vdev, struct virtio_fs_config, num_request_queues,
 964          &fs->num_request_queues);
 965  if (fs->num_request_queues == 0)
 966      return -EINVAL;
 968  /* Truncate nr of request queues to nr_cpu_id */
 969  fs->num_request_queues = min_t(unsigned int, fs->num_request_queues,
 970                  nr_cpu_ids);
 971  fs->nvqs = VQ_REQUEST + fs->num_request_queues;
 ...
1000  ret = virtio_find_vqs(vdev, fs->nvqs, vqs, vqs_info, &desc);
```

So: **the device's config value is the only input**, clamped to `nr_cpu_ids`;
zero is a hard `-EINVAL` (a device that advertises virtio-fs and reports 0
request queues fails the mount). `v7.2` has **no `num_request_queues` module
parameter** - grepping `module_param` in `virtio_fs.c` returns nothing; the
boot-time knob the ticket asks about is gone from the modern driver.

Queue 0 is `VQ_HIPRIO` (enum at `virtio_fs.c:41-44`), the forget/interrupt
queue; queues 1..nvqs are the request queues. cang's device declares exactly the
two queues that `nvqs = 1 + 1` asks for, which is why the mount works.

### 1.2 What happens when `VIRTIO_F_MULTIQUEUE` is not negotiated: nothing

`virtio_fs.c` never calls `virtio_has_feature()` at all (grep: no hits), so the
driver does not consult the feature bit before creating `1 + n` virtqueues. The
transport does not gate it either: `virtio_pci_common.c:517-544` sets up
whatever `nvqs` the driver passes, and the number the *device* exposes comes
from the PCI common config's `num_queues`, which libkrun writes from
`queue_config().len()` (`deps/libkrun/src/devices/src/virtio/pci.rs:688`).

Consequence for the fork: raising the queue count is mechanically
`queue_config().len()` + a `Vec<QueueConfig>` field (the pattern already exists
for console: `virtio/console/device.rs:63,82-100,307-309`), and the guest will
use the queues without VIRTIO_F_MULTIQUEUE. That "works" but is
spec-noncompliant - a multi-queue device is supposed to offer the feature - so
an honest change offers `VIRTIO_F_MULTIQUEUE` as well. **No device in this fork
offers it today** (net: `net/device.rs:108-111`; blk:
`block/device.rs:330-341`), so this is greenfield in the tree.

The load-bearing fact for ticket 04 is different: **`num_request_queues` is a
device-level (per-share) value, and cang has exactly one share.** More queues on
the root device therefore reach `/workspace` - no second device is needed.

### 1.3 Dispatch and how much can be in flight

With one request queue, the kernel maps every possible CPU to it
(`virtio_fs.c:886-917`: MSI-X affinity if the transport has it, else
`group_cpus_evenly`, else all CPUs to `VQ_REQUEST`), and requests are pushed
onto the virtqueue by the per-connection iqueue ops, which virtio-fs replaces
wholesale (`virtio_fs.c:1545-1550` `virtio_fs_fiq_ops`, installed at `:1721`)
instead of the `/dev/fuse` ones (`dev.c:297-303`). There is no global lock on
submission: many guest threads can have requests in the ring at once.

The in-flight ceilings are therefore:
- the ring: 1024 descriptors (`fs/mod.rs:46`) on the fork side, matching
  `fc->max_pages_limit = min(fuse_max_pages_limit, vq_size - FUSE_HEADER_OVERHEAD)`
  (`virtio_fs.c:1731-1732`, `FUSE_HEADER_OVERHEAD = 4` at `:30`);
- background requests: the fork replies `max_background = u16::MAX`
  (`server.rs:945`), and the guest mount holds `CAP_SYS_ADMIN`, so
  `process_init_limits` does not clamp it (`inode.c:1237-1252`);
- **one**: the number of `FsWorker` threads. That is the binding constraint, and
  it is why §5 puts the worker pool high.

### 1.4 Which FUSE_INIT flags the kernel actually uses

`process_init_reply` (`inode.c:1269-1423`) reads the server's reply; the flag
tests are unconditional (there is no per-flag minor-version gate except
`arg->minor < 13` for the limits, `:1239`):

| flag | kernel effect | line |
|---|---|---|
| ASYNC_READ | `fc->async_read = 1`; without it the kernel allows one pending read per fh and orders reads by offset | `:1292-1293` |
| ATOMIC_O_TRUNC | `fc->atomic_o_trunc = 1`; `O_TRUNC` is then carried in OPEN/CREATE instead of a preceding SETATTR | `:1303-1304` |
| BIG_WRITES | `fc->big_writes = 1` (writes > 4 KiB) | `:1310-1311` |
| DONT_MASK | `fc->dont_mask = 1` | `:1312-1313` |
| AUTO_INVAL_DATA | `fc->auto_inval_data = 1` (else EXPLICIT_INVAL_DATA is tried) | `:1314-1317` |
| DO_READDIRPLUS / READDIRPLUS_AUTO | `do_readdirplus`, `readdirplus_auto` | `:1318-1322` |
| ASYNC_DIO | `fc->async_dio = 1` | `:1323-1324` |
| WRITEBACK_CACHE | `fc->writeback_cache = 1` | `:1325-1326` |
| PARALLEL_DIROPS | `fc->parallel_dirops = 1` | `:1327-1328` |
| HANDLE_KILLPRIV / _V2 | `handle_killpriv`, `handle_killpriv_v2`; V2 also sets `SB_NOSEC` | `:1329-1330`, `:1361-1364` |
| MAX_PAGES | `fc->max_pages = min(max_pages_limit, max(arg->max_pages,1))`, and `name_max = FUSE_NAME_MAX` when >1 | `:1341-1352` |
| MAP_ALIGNMENT | only under `CONFIG_FUSE_DAX`; fails the init if `fuse_dax_check_alignment` rejects the value | `:1353-1357` |
| SUBMOUNTS | (offered by the kernel only when `fc->auto_submounts`, which virtio-fs sets) | `:1472-1473`, `virtio_fs.c:1726` |
| PASSTHROUGH | `fc->passthrough = 1` **only if** `max_stack_depth` is sane **and** `!(flags & WRITEBACK_CACHE)` | `:1387-1395` |
| OVER_IO_URING | `fuse_chan_io_uring_enable(fc->chan)` if `fuse_uring_enabled()` | `:1404-1405` |

Two numbers worth carrying into ticket 04:

- `max_write` = `MAX_BUFFER_SIZE` = 1 MiB (`server.rs:35`, `:947`) and
  `max_pages` = 256 (`:933`, `:949`). The kernel's ceiling is
  `fuse_max_pages_limit = 256` (`inode.c:38`) - so the fork's 1 MiB is already
  the *maximum* the guest will accept. There is no "bigger requests" lever left.
- `time_gran = 1` ns (`server.rs:948`), so timestamp granularity is not a
  revalidation trigger.

### 1.5 Does the pinned config actually build this?

Yes, in both x86_64 configs cang can boot:

| config line | `-kvm-lto` | `-kvm` |
|---|---|---|
| `CONFIG_FUSE_FS=y` | 2889 | 2886 |
| `CONFIG_VIRTIO_FS=y` | 2891 | 2888 |
| `CONFIG_FUSE_DAX=y` | 2892 | 2889 |
| `CONFIG_FUSE_PASSTHROUGH=y` | 2893 | 2890 |
| `CONFIG_FUSE_IO_URING=y` | 2894 | 2891 |
| `CONFIG_FS_DAX=y` / `CONFIG_DAX=y` | 2874 / 2812 | 2871 / 2809 |
| `CONFIG_ZONE_DEVICE=y` (needed by `devm_memremap_pages` for the DAX window) | 1028 | 1025 |
| `CONFIG_SMP=y`, `CONFIG_NR_CPUS=48` | 333, 383 | 336, 386 |
| `CONFIG_NUMA` | not set (410) | not set (413) |

So the guest can do DAX and advertises both passthrough and io_uring capability
at INIT (`inode.c:1474-1482`) - the guest is not the missing piece for any lever
below, which is what makes §4 a host/architecture question, not a config one.

`CONFIG_NR_CPUS=48` against this host's 28 cores and
`config::resolve_cpu_count()` (`crates/cang/src/runtime/session/mod.rs:308`)
means a cang guest gets ~28 vCPUs competing for one FUSE worker thread.

## 2. What each flag buys for a rustc-shaped load

Rustc/cargo over `/workspace` is: many `statx`/`lookup`, one `open` per file,
`read` or `mmap` of small files (`.rs`, `.rmeta`, `.rlib`, `Cargo.toml`), then
writes into `target/` (large `.o`/`.rlib`/binary outputs plus a swarm of tiny
`.json`/`.d`/fingerprint files), and directory churn (`mkdir`, `rename`,
`unlink`) during incremental builds.

### 2.1 Already negotiated (so: no work, no win)

ASYNC_READ, ATOMIC_O_TRUNC, PARALLEL_DIROPS, AUTO_INVAL_DATA, ASYNC_DIO,
HAS_IOCTL_DIR, BIG_WRITES, MAX_PAGES, SUBMOUNTS, HANDLE_KILLPRIV_V2,
DO_READDIRPLUS/READDIRPLUS_AUTO, ALLOW_IDMAP. This is worth saying plainly to
ticket 04: **the "flag set" lever as scoped on the map is already pulled.**

### 2.2 Round trips removed vs semantics only

| flag | round trips removed | semantics only |
|---|---|---|
| ATOMIC_O_TRUNC | one SETATTR(size=0) per `O_TRUNC` open (rustc/cargo open build outputs with `create+truncate` constantly) | - |
| PARALLEL_DIROPS | none | yes: removes the kernel's per-inode mutex around lookups/readdir (`inode.c:588-598` `fuse_lock_inode`); without it parallel lookups on one directory serialise in the guest |
| MAX_PAGES / BIG_WRITES | read/write request count for the same bytes: 1 MiB per request instead of 4 KiB (`BIG_WRITES` absent) or 32 pages/128 KiB (`MAX_PAGES` absent) - i.e. ~8x fewer requests for large reads | - |
| ASYNC_READ | lets several readahead/read requests per file be in flight instead of one | - |
| HANDLE_KILLPRIV_V2 | removes the SETATTR that drops suid/sgid after write/trunc/chown when the writer lacks `CAP_FSETID` (`dir.c:2321-2348`, `file.c:37-40`, `dir.c:853-856`) | - |
| AUTO_INVAL_DATA | *adds* a `STATX_SIZE` refresh on buffered reads, but only when the attribute is stale (`file.c:1113-1129`) - it is the round-trip price of the cache invalidation insurance (`inode.c:363-385`) | yes: invalidate cached pages when mtime/size moved |
| EXPLICIT_INVAL_DATA | removes that statx | **yes, big one**: the server becomes solely responsible for invalidation; mutually exclusive with AUTO_INVAL_DATA (`inode.c:1314-1317`) |
| NO_OPEN_SUPPORT / NO_OPENDIR_SUPPORT | one OPEN + one RELEASE per file open (`file.c:133-187`: `open = !fc->no_open`) | yes: no per-open handle on the server, so every read/write must work from the inode alone |
| CACHE_SYMLINKS | one READLINK per symlink, cached in the inode | yes: symlink target changes become invisible |
| DONT_MASK, POSIX_ACL, POSIX_LOCKS, FLOCK_LOCKS, EXPORT_SUPPORT | none or negligible for this load | yes |
| WRITEBACK_CACHE | one WRITE round trip per `write(2)`; writes become page-cache copies (`file.c:1495-1547`) flushed by `writepages` as up to `max_pages` requests carrying `FUSE_WRITE_CACHE` (`file.c:2127`) | yes: close-to-open is gone; the host sees writes late; a dirty guest file can be clobbered by a host edit |

### 2.3 The three the ticket singles out

**ATOMIC_O_TRUNC** - already on, and it is the right behaviour for rustc: with
it, `fuse_send_open` keeps `O_TRUNC` in the OPEN flags (`file.c:33-35`) instead
of clearing it and sending a separate `SETATTR` at size 0; the client then
truncates its own page cache (`file.c:287-295`). Nothing to do.

**PARALLEL_DIROPS** - already on. Note what it is *not*: it does not change
server concurrency. It only stops the guest from taking `fi->mutex` around
lookups/readdir (`inode.c:588-598`). The fork's single worker still serialises
the FUSE side of those lookups.

**attr/entry timeout sizing** - the current 5 s/5 s is the virtiofs default, and
it is *per reply*: `lookup` stamps each `Entry` with `cfg.attr_timeout` /
`cfg.entry_timeout` (`passthrough.rs:1011-1018`), the kernel stores them in the
dentry (`dir.c:297-301`) and only revalidates after expiry (`dir.c:399-400`, then
a fresh FUSE_LOOKUP `:423-424`) or an explicit `LOOKUP_REVAL`. Attributes are
re-checked through `fuse_update_attributes` on stat-time, and - because
AUTO_INVAL_DATA is on - on every buffered read that finds the attribute stale
(`file.c:1123-1126`).

So the reason a 5 s timeout is worth touching is subtler than "statx storms":
a build that runs for minutes re-stats everything it touches every 5 s, and each
re-stat is a round trip through the single worker. It is also the cheapest
coherence knob cang has (host edits invisible for the timeout window only), and
it needs *no* fork logic - `Config` already has both fields
(`passthrough.rs:307,314`) and `Fs::new` already wires them
(`device.rs:80-93`); only a way to set them (an `FsDevice` knob, or a different
fork default) is missing.

### 2.4 What can be advertised without changing the coherence contract

- **No change to coherence:** parallel dispatch and more queues (concurrency is
  not semantics), and - of the flags - precisely the set that is already on.
  ATOMIC_O_TRUNC, PARALLEL_DIROPS, MAX_PAGES, BIG_WRITES, ASYNC_READ and
  HANDLE_KILLPRIV_V2 are server-side optimisations the kernel already assumes are
  safe; none of them changes what the client is allowed to remember.
- **Weakens coherence:** WRITEBACK_CACHE (host-visible writes lag; the fork's own
  doc comment spells out the exclusivity requirement, `passthrough.rs:320-333`),
  longer attr/entry timeouts, CACHE_SYMLINKS, EXPLICIT_INVAL_DATA,
  NO_OPEN_SUPPORT (loses per-open server state). These are exactly ticket 03's
  subject, and `WRITEBACK_CACHE` also has a hard interaction worth recording now:
  the kernel refuses `FUSE_PASSTHROUGH` when writeback is on
  (`inode.c:1387-1395`), so those two levers are mutually exclusive by design.

## 3. The server side, measured against upstream virtiofsd

### 3.1 INIT masks: the fork is at parity, and slightly more aggressive

virtiofsd's `Server::init` advertises (`src/server.rs:927-938`) the same set:
ASYNC_READ, PARALLEL_DIROPS, BIG_WRITES, AUTO_INVAL_DATA, ASYNC_DIO,
HAS_IOCTL_DIR, ATOMIC_O_TRUNC, MAX_PAGES, SUBMOUNTS, INIT_EXT, plus
CREATE_SUPP_GROUP and ALLOW_IDMAP. Differences against the fork:

| | fork | virtiofsd |
|---|---|---|
| HANDLE_KILLPRIV_V2 | always advertised (`server.rs:907`) | only when `--killpriv-v2` (`README`, default **off**) |
| CREATE_SUPP_GROUP | not advertised | advertised |
| NO_OPEN_SUPPORT / NO_OPENDIR_SUPPORT | not advertised | not advertised |
| `max_write` / `max_pages` | 1 MiB / 256 (`server.rs:35,947,949`) | 1 MiB / 256 (`server.rs:25,958,960`) |
| `max_background` / `congestion` | `u16::MAX` / `(u16::MAX/4)*3` (`server.rs:945-946`) | `u16::MAX` / same |
| `time_gran` | 1 ns | 1 ns |
| request queue depth | `QUEUE_SIZE = 1024` (`fs/mod.rs:46`) | `QUEUE_SIZE = 32768` (`vhost_user.rs:41`) |
| ring features | VERSION_1, RING_F_EVENT_IDX | VERSION_1, RING_F_INDIRECT_DESC, RING_F_EVENT_IDX, vhost-user protocol |

So on FUSE_INIT the fork is **not behind** virtiofsd except for the writeback
flag (both default off) and the zero-message opens (neither implements them).
The real difference is the transport and the dispatch, below.

### 3.2 virtiofsd's knob semantics

From `README.md` (§Flags/§Options) and `src/passthrough/mod.rs`:

- `--cache auto|always|metadata|never`, default **auto** (the fork's
  `CachePolicy` has only `Never|Auto|Always`, `passthrough.rs:251-268` - there is
  no `metadata` mode on this side); `CachePolicy`
  `Auto` = close-to-open consistency, `Always` = keep the page cache across
  opens (`FOPEN_KEEP_CACHE` / `FOPEN_CACHE_DIR`), `Never` = `FOPEN_DIRECT_IO`.
  The fork has the same enum and the same defaults (`passthrough.rs:251-281`,
  `:703-717`), and cang's device cannot set it (`FsDevice` exposes no cache
  knob, `device_builders.rs:671-690`). **This is an unnoticed lever**: with
  `Auto`, every `open` invalidates the inode's page cache in the guest
  (`file.c:294-297`, `invalidate_inode_pages2` when `FOPEN_KEEP_CACHE` is not
  set), i.e. re-opening the same file twice in one build re-reads it over
  virtiofs.
- `--thread-pool-size`, default **0 = pool disabled**: the serial path is
  `process_queue_serial` (`vhost_user.rs:254-290`); with a pool, every available
  descriptor chain is spawned onto a shared `ThreadPool` and the descriptor is
  returned from the worker (`:213-252`, `return_descriptor` under
  `VringMutex`). **Upstream virtiofsd has exactly one request queue too**
  (`vhost_user.rs:41-47`: `REQUEST_QUEUES = 1`, `NUM_QUEUES = 2`, with the
  comment "The spec allows for multiple request queues. We currently only
  support one.") - so the reference implementation's answer to this exact
  performance problem is *a pool over one queue*, not multiqueue.
- `--inode-file-handles never|prefer|mandatory`, default `prefer`: reference
  inodes by `name_to_handle_at`/`open_by_handle_at` instead of holding an
  `O_PATH` fd per inode. It is an fd/`rlimit-nofile` and NFS-silly-rename
  measure, not a round-trip one; the guest sees no difference. The fork holds
  one `O_PATH` fd per inode (`passthrough.rs:392` + `lookup`'s `openat(...O_PATH)`
  at `:949-961`), which is a separate (fd-count) concern for cang and not a
  performance lever for the compile.
- `--announce-submounts`, default **on**: report `FUSE_ATTR_SUBMOUNT` per
  submount so the guest gets per-submount device ids and `SYNCFS` per submount;
  the README warns that without it a single `SYNCFS` may lose data. The fork
  advertises SUBMOUNTS whenever the guest offers it (`passthrough.rs:905-908`)
  and stamps `ATTR_SUBMOUNT` on device/mnt_id changes (`:967-972`) - same
  behaviour, and cang's root share only ever has one mount, so it is inert here.

### 3.3 The mechanism virtiofsd already solved, and the state it shares

The interesting part for ticket 04 is what the pool threads share, because that
is what the map's "Not yet specified - the fork's server state under parallel
dispatch" is asking about:

- `VhostUserFsThread::server` is an `Arc<Server>` (`vhost_user.rs:130-163`,
  `:231`), and `Server::handle_message` is `&self` (`server.rs:98`) - the same
  shape as the fork (`server.rs:71-93`, `Server { fs, options: AtomicU64 }`).
- the inode map is `InodeStore { inner: Arc<RwLock<InodeStoreInner>> }` over a
  `BTreeMap<Inode, Arc<InodeData>>` (`passthrough/inode_store.rs:81-89`), and
  handles live in another lock - i.e. **the same two-lock model the fork
  already has** (`passthrough.rs:392` `inodes: RwLock<MultikeyBTreeMap<...>>`,
  `:397` `handles: RwLock<BTreeMap<...>>`).
- per-request work is done on the worker thread: the `DescriptorChain` is moved,
  and `Reader`/`Writer` are constructed inside the pool task
  (`vhost_user.rs:205-211`), then the used-ring insertion happens back under the
  `VringMutex` (`:247`).

### 3.4 What that means for the fork (the map's open question, answered)

**No inode-map refactor is needed.** `PassthroughFs` methods all take `&self`
and every mutable field is already behind `RwLock`/`Atomic*`
(`passthrough.rs:392-408`), `AugmentFs` is `RwLock`-based too
(`augment_fs.rs:51-86`), and `Server::handle_message` is `&self`. So
`Arc<FsServer>` handed to N worker threads is memory-safe as-is. The work is
entirely in `worker.rs`:

1. own the queue's used-ring and notification behind a `Mutex` (today
   `process_queue` at `:234-268` does `queue.pop`, then inline
   `handle_message`, then `queue.add_used` + `interrupt.signal_used_queue`);
2. keep the `disable_notification` / `enable_notification` dance
   (`handle_event`, `:212-232`) on the thread that owns the eventfd, so
   notifications are not lost when replies come from pool threads;
3. bound the pool (a fixed small pool, or one thread per request queue) - the
   ring is 1024 so unbounded spawning is not needed and would only add latency.

The semantic risks are FUSE-level, not lock-level: FORGET/INTERRUPT arrive on
the hiprio queue and share the same server, and replies must be returned to the
ring with the length the handler produced. Both are visible in the existing
code, and virtiofsd's split (`process_message` → `return_descriptor`) is the
line-for-line template.

## 4. Host requirements: passthrough, io_uring, DAX

### 4.1 `FUSE_PASSTHROUGH` - structurally unreachable for cang

The mechanism is not "the server hands the guest an fd". It is a **kernel-side
id registry on the connection**:

- the server registers a backing file with
  `ioctl(FUSE_DEV_IOC_BACKING_OPEN)` **on its own `/dev/fuse` fd**
  (`dev.c:2298-2314`), which resolves to `fud->chan->conn` - the *server's*
  `fuse_conn` - and stores the `struct file` in `fc->backing_files_map`
  (`fuse_i.h:762-763`, `:1257-1276`);
- FUSE replies then carry only a `backing_id`, and the client looks it up in
  *its* connection's map (`fuse_backing_lookup`, `fuse_i.h:1261`).

Server-side registration and client-side lookup must therefore land in **one
kernel and one `fuse_conn`**. In cang the client is the *guest* kernel and the
server is the VMM in a userspace process on the host, which has no `/dev/fuse`
fd for the guest's connection at all - so there is no ioctl to issue and no map
in which the id would appear. (`virtiofsd` reflects this: its sources contain no
`PASSTHROUGH` or backing-fd code whatsoever, grep over `src/main.rs`,
`src/server.rs`, `src/vhost_user.rs`.) The guest side is ready -
`CONFIG_FUSE_PASSTHROUGH=y` (config:2893), `virtio_fs.c` offers the bit
(`inode.c:1474-1475`) - but passthrough can only ever fire when a userspace FUSE
daemon runs *in the same kernel as the client*, i.e. inside the guest over
`/dev/fuse`. That is a different architecture (an in-guest `fuse-overlayfs`-ish
daemon), not a fork change.

Also record the constraint even if it were reachable: the kernel refuses
passthrough together with writeback cache (`inode.c:1387-1395`), and requires
`max_stack_depth` in the init reply (the fork replies 0, `server.rs:952`).

### 4.2 `FUSE_IO_URING` - a `/dev/fuse` transport, not reachable from a VMM server

`FUSE_OVER_IO_URING` is an information bit: if the guest sees it, the server
"is either sending `IORING_OP_URING_CMD` or not" (`inode.c:1477-1482`). The
machinery lives entirely on the `/dev/fuse` file: `enable_uring` module param and
ring setup in `dev_uring.c:15-26`, `:1028-1090`, `:1241`, a `uring_cmd` file
operation (`dev.c:2388-2391`), and `fuse_chan_io_uring_enable(fch)`
(`dev.c:414-417`) which only has meaning for a chan whose server is a
`/dev/fuse` reader.

virtio-fs never goes near it: the connection's iqueue ops are replaced by
`virtio_fs_fiq_ops` (`virtio_fs.c:1545-1550`, installed `:1721`), so requests
never reach `dev.c`'s queue/`pqueue` paths. In cang the "server" is the VMM's
Rust FUSE implementation, which has no `/dev/fuse` and no io_uring ring to
register. **Not a lever.** (Same conclusion as passthrough, for the same
structural reason: both are kernel-to-kernel-fd optimisations for a
`/dev/fuse`-based daemon.)

### 4.3 The DAX window - reachable, but a mount-time, whole-share decision

Guest side, both halves are in the pinned kernel and enabled:
`virtio_fs_setup_dax` allocates the dax device and maps the device's
`VIRTIO_FS_SHMCAP_ID_CACHE` shm region (`virtio_fs.c:1076-1140`) - it returns
early with "No cache capability" if the device exposes no window (`:1093-1098`),
which is exactly cang today (`shm_size = None`,
`device_builders.rs:686`, `linked.rs:317-326`). DAX is then selected **by a
mount option**, not by the INIT flags: `dax=always|never|inode`
(`virtio_fs.c:104-140`), and `dax=always` without a dax device fails the mount
(`:1596-1603`). Mapping granularity is `FUSE_DAX_SZ = 2 MiB`
(`dax.c:20-22`), allocated/freed per 2 MiB range with reclaim thresholds
(`dax.c:113`, `:1163`), and the reply's `map_alignment` must be `<= 21`
(`dax.c:1338-1340`; the fork replies 0, `server.rs:950`, which is fine).

The fork's server half exists: `setupmapping` `mmap`s the host file into the
window with `MAP_SHARED|MAP_FIXED` (`passthrough.rs:2045-2095`, bounds check
`:2069-2071`), and `removemapping` frees it (`:2097+`).

Two consequences for the map's "whether a knob can even reach `/workspace`":
DAX *is* per-device + mount-time, but since cang has one share mounted as `/`,
`/workspace` is inside it - so a window on the root device plus `dax=` in the
guest's mount options does cover `/workspace` **without a second device**. What
DAX cannot do is be scoped to `/workspace`: the window is one sized region
shared by every file of the share, divided into 2 MiB slots, so `/nix`,
`~/.cargo` and `target/` compete for it.

### 4.4 Which knobs are per-share (device/mount-time) vs per-connection

| knob | scope | does it reach `/workspace` today? |
|---|---|---|
| FUSE_INIT flags (incl. writeback) | per connection, negotiated at mount | yes - same connection |
| attr/entry timeout | per reply, server config | yes |
| cache policy (KEEP_CACHE / DIRECT_IO) | per open reply, server config | yes |
| request queue count | per device | yes - one share |
| worker pool | per device (server side) | yes |
| DAX window | per device **and** a guest mount option | yes, but only whole-share, and it needs `rootflags`/mount-option plumbing |
| passthrough / io_uring | per connection | unreachable |

## 5. Table: flag -> round trips removed -> coherence cost -> fork work

"Round trips" are FUSE request/response pairs through the single `FsWorker`;
"*" marks the ones the fork already negotiates today, so their columns are
historical.

| flag | guest round trips removed | coherence cost | fork work required |
|---|---|---|---|
| WRITEBACK_CACHE | one per `write(2)`; writes become page-cache writes flushed as <=1 MiB `FUSE_WRITE_CACHE` requests in parallel with compute (`file.c:1495-1547`, `:2127`) | **high**: host sees guest writes late; a host edit to a dirty file loses; requires exclusive access (`passthrough.rs:320-333`); excludes PASSTHROUGH | **1 line** - `writeback: false` -> `true` at `device.rs:88-93`, or a `Config` knob via `FsDevice` (`device_builders.rs:671-690`) + cang (`linked.rs:317-326`). passthrough already implements the mode (killpriv via `drop_effective_cap(CAP_FSETID)`, `passthrough.rs:685-689`) |
| NO_OPEN_SUPPORT (+NO_OPENDIR_SUPPORT) | one OPEN and one RELEASE per open (`file.c:133-187`) - the most frequent remaining op for a rustc load | medium: no per-open server handle; `fh = 0` for all I/O; `FOPEN_KEEP_CACHE` becomes the default open_flags (`file.c:156`) | **medium-large**: the fork must serve read/write/getattr/flush from the inode when `fh == 0`; today `read`/`write` look the handle up and fail |
| CACHE_SYMLINKS | one READLINK per symlink (cached) | low-medium: retargeting a symlink stays invisible | small: advertise the bit (`server.rs:897-908`); no handler change |
| ATOMIC_O_TRUNC * | one SETATTR(size=0) per `O_TRUNC` open - without it the kernel strips `O_TRUNC` from OPEN (`file.c:33-35`) and calls `fuse_do_truncate` separately (`file.c:2708`) | none | none - already on (`server.rs:904`) |
| PARALLEL_DIROPS * | none | none (removes a guest-side mutex, `inode.c:588-598`) | none - already on (`server.rs:899`) |
| MAX_PAGES + BIG_WRITES * | ~8x fewer read/write requests at 1 MiB vs 128 KiB/4 KiB | none | none - already on (`server.rs:900,905`); 256 pages is the kernel ceiling (`inode.c:38`) |
| ASYNC_READ * | one pending read per file handle -> many | none | none - already on |
| HANDLE_KILLPRIV_V2 * | one SETATTR per suid-affecting write/trunc/chown | none (fork drops `CAP_FSETID` around the op) | none - already on (`server.rs:907`) |
| AUTO_INVAL_DATA * | costs a statx-size refresh per buffered read *when stale* (`file.c:1123-1129`) | bought: safe invalidation | none - already on |
| EXPLICIT_INVAL_DATA | removes that statx | **high**: server owns all invalidation; excludes AUTO_INVAL_DATA | medium (the fork already sets AUTO_INVAL_DATA; switching is a policy decision, not code) |
| attr/entry timeout (5 s -> longer) | repeated GETATTR/LOOKUP for anything a >5 s build touches twice (`dir.c:399-400`, `file.c:1123`) | medium: host edits/`chmod` invisible for the window | **tiny**: fields exist (`passthrough.rs:307,314`), only a setter/knob missing |
| cache policy `Always` (`FOPEN_KEEP_CACHE`) | re-read of a file already opened once (kills the open-time `invalidate_inode_pages2`, `file.c:294-297`) | medium: stale pages until the mtime check fires | tiny: `cache_policy` field exists (`passthrough.rs:318`), no knob |
| DAX window (`dax=`) | data reads/writes bypass FUSE entirely for windowed mappings; one SETUPMAPPING per 2 MiB range | medium: window is shared whole-share; writes need flush/fault bookkeeping | **large**: window sizing + `shm_size` plumbing (`device_builders.rs:664`), guest `dax=` mount option, and the fork's DAX path is untested in cang |
| FUSE_PASSTHROUGH | would remove data round trips per inode | n/a | **impossible** in this architecture (§4.1) |
| FUSE_IO_URING | n/a | n/a | **not reachable** (§4.2) |

## 6. Ordering for ticket 04, by expected win / (risk + fork work)

The rustc-shaped load splits into a read/metadata half and a write half. The
single `FsWorker` hurts both; writeback is the only lever that helps the write
half for free; the pool is the only lever that raises the ceiling of both.

1. **WRITEBACK_CACHE.** Best ratio on the board: near-zero fork work (a default
   or a knob), and it changes the write path from "synchronous round trip per
   `write(2)`, serialised behind the single worker" to "page-cache write,
   flushed as <=1 MiB requests in the background". Every rustc-shaped build
   writes: object files, `rlib`/`rmeta`, dep-info, and the thousands of tiny
   files in `target/*/.fingerprint`. Risk is entirely coherence, which is
   ticket 03's decision, and it is reversible with a flag. **Ship first.**
2. **Parallel FUSE dispatch (a worker pool over the single request queue).**
   Highest ceiling, and cheaper than the map assumed: no virtio feature, no
   queue-count change, no inode-map refactor (§3.3-3.4) - it is `worker.rs`
   plus `Arc<FsServer>` and a used-ring mutex, with upstream virtiofsd's
   `process_queue_pool` as the template. It lifts the read/metadata half that
   writeback does not touch, and it *multiplies* the value of every other lever
   because round trips stop being serial. Risk is concurrency semantics
   (reply/FORGET/interrupt ordering), not data structure ownership.
3. **attr/entry timeout sizing (+ optionally cache policy `Always`).** Cheap
   and safe to A/B: the fields exist, the only work is a way to set them. Win is
   bounded but real for long builds; the cost is a coherence window that ticket
   03 must bless. Do it after 1-2 so the measurement of 1-2 is not polluted by
   a timeout change.
4. **The residual FUSE_INIT set (NO_OPEN_SUPPORT, CACHE_SYMLINKS).** Real
   round-trip savings (one OPEN per open is the single most frequent remaining
   request for this load) but that is *not* a flag flip: it changes the server's
   handle model. Worth a ticket of its own only if 1-2 leave the number short.
5. **DAX window.** High potential on paper (data path bypass), but large work,
   a whole-share window sized against `/nix` + cargo + `target/`, an untested
   fork path, and a 2 MiB granularity that suits big mapped files rather than
   rustc's small-file reads. Ticket 05 already owns the viability question;
   this note's contribution is that it is *reachable* (no second device needed)
   but should not be first.
6. **Multiqueue (`VIRTIO_F_MULTIQUEUE` + N request queues).** After the pool, it
   buys nothing for one share: the pool already saturates the host CPUs from a
   single ring of 1024 descriptors, and multiqueue adds the feature-bit,
   queue-config-vector and per-queue-worker work on top. Only worth revisiting if
   a pool over one queue measurably hits a ring/notification ceiling.
7. **passthrough / io_uring.** Not levers in this architecture. Record the
   reason (§4.1-4.2) so the map stops treating them as candidates.

## 7. What this note does not settle

- No numbers. Which of (1) and (2) moves the wall clock more is exactly what
  ticket 02's harness measures; §6's ordering is win/(risk+work), not win.
- Whether writeback is *acceptable* is a product decision about the workspace
  coherence contract (ticket 03): cang's `/workspace` is a host directory that a
  human also edits, so "the host sees guest writes late" is not hypothetical.
- The fork's `O_PATH`-per-inode model (`passthrough.rs:949-961`) and the
  1024-entry queue are the same design as upstream libkrun, so any of these
  changes is fork-local work that upstream does not have; nothing here is a
  regression to report upstream.
- `xattr` is on by default in the fork (`passthrough.rs:373`) and off by default
  in virtiofsd (it needs `--xattr`). In the fork the flag only decides whether
  the four xattr handlers answer or return `ENOSYS`
  (`passthrough.rs:1766,1814,1861,1902`), so it is not a round-trip lever for a
  rust compile - it matters only if something in the guest asks for xattrs
  (`cp --preserve=xattr`, `rsync -X`, a container layer). Recorded so nobody
  re-derives it as a candidate.

## Correction (2026-10-26, ticket 15)

`VIRTIO_F_MULTIQUEUE` **does not exist for virtio-fs**. Section 1.2's "an honest
change offers VIRTIO_F_MULTIQUEUE" is wrong: the virtqueues a virtio-fs device
has are fixed by its config word (`num_request_queues`), and the guest driver
reads that word and creates exactly that many request queues (Linux v7.2
`fs/fuse/virtio_fs.c:963-971`, zero `MULTIQUEUE` hits in the driver or in the
spec's virtio-fs device). The fork's single queue is therefore a hard-coded
design constant (`NUM_QUEUES = 2`), not a withheld feature - see
[15-why-one-queue-and-one-thread](15-why-one-queue-and-one-thread.md).
