---
label: wayfinder:research
title: What does "cache" mean for this virtiofs server, and can it be scoped to /workspace only?
status: open
blocked_by: []
claimed_by: pi research child (2026-10-26)
---

# 13 - Cache semantics and scoping

Reopened question on the virtio-fs map (`tickets/` has no ticket for this yet;
this note is the ticket's answer if bob opens one). Read-only: no source edits,
no builds, no VMs.

Read first: [01](01-fuse-negotiation-and-queue-facts.md) (negotiation, worker,
levers) and [05](05-dax-window-viability.md) (why a per-connection window cannot
be aimed at `/workspace`).

**Alignment (2026-10-26).** bob reopened the decision ticket 03 had closed:
ticket 03 now carries a "Superseded in part (2026-10-26, bob)" section -
caching is allowed, **`/workspace` only, opt-in, and only with a launch warning**
and [ticket 16](../tickets/16-workspace-only-opt-in-cache.md) is the
implementing ticket. This note is ticket 16's input:

| ticket 16's question | answered in |
|---|---|
| 1 which mode, and exactly which FUSE_INIT flags each sets | §0.1-0.2, §1.1, §1.4, **§1.5** (four reachable combinations, not two) |
| 2 the opt-in surface (flag / `cang.toml`, default off) | §2.2, §5.1 |
| 3 the warning: text, place, and what the guest sees | §4.1-4.2 |
| 4 what must be proved before it ships | §5.2 (the four gates, and the one shape under which all of them still hold) |
| 5 cang vs fork split of work | §3.3 (fork, incl. tag/pin/`flake.lock`/live smokes) and §3.4 (cang + guest) |

Sources actually read (line numbers are stable for these refs):

| what | where |
|---|---|
| the fork's virtio-fs server | `/home/dev/cang/cang/deps/libkrun` @ `7160f48f` (`v2.0.0-cang.10-2-g7160f48f`): `src/devices/src/virtio/fs/{device,worker,server,fuse}.rs`, `.../fs/linux/passthrough.rs`, `src/libkrun/src/api/device_builders.rs`, `src/libkrun/src/vmm/{builder.rs,device_manager/kvm/mmio.rs}`, `include/libkrun.h`, `docs/bindings-generation.md`. **The worktree's `deps/libkrun` (`79228a72`) has the same line numbers for every fs file cited here** - the two commits between them touch docs and the huge-page `madvise` only. |
| cang | worktree `/home/dev/cang/cang-vfio`: `crates/cang-libkrun/src/{api,linked,lib}.rs`, `crates/cang/src/runtime/vm/{libkrun/launcher.rs,prepared_root/mod.rs}`, `crates/cang/src/runtime/launch/{config/model.rs,config/components/mounts*,components/mounts/workspace.rs}`, `crates/cang-guest-init/src/guest_init/{runtime/cang.rs,components/fdwatch.rs}` |
| guest kernel, upstream | `v7.2` on `git.kernel.org`: `fs/fuse/{file.c,inode.c}`, `include/uapi/linux/fuse.h` |
| upstream virtiofsd | `gitlab.com/virtio-fs/virtiofsd` @ `main`: `README.md`, `src/passthrough/mod.rs`, `src/server.rs` |
| virtiofsd docs (the canonical trade-off sentence) | `virtio-fs.gitlab.io/qemu/tools/virtiofsd.html` |

## Verdict

**"cache=auto" is already what cang runs today, so there is no knob to turn on
for it; "cache=always" is one line in the fork (per-open reply flags) and
writeback is another one line (`WRITEBACK_CACHE`, a *separate* option in
virtiofsd too, not part of `--cache`). Only the writeback half is
connection-wide and therefore unscopable: the cache=*/`KEEP_CACHE`/`DIRECT_IO`
half lives in per-open reply flags and *could* be decided per path, but the flag
that removes the write round trip cannot. Scoping to `/workspace` therefore
means one thing only - a second virtio-fs device with its own `fuse_conn` - and
that is ~150 lines across fork FFI, `cang-libkrun`, the launcher,
`prepared_root` and `cang-guest-init`; the fork's transport, MMIO manager,
cmdline and device-index plumbing already support a second device with **no
changes**. For bob's decision (a `/workspace`-wide opt-in cache) that device is
the whole answer, and with `auto` + writeback it is the best buy/risk pair of the
four combinations that exist (§1.5). The strict subset that keeps *every* one of
ticket 03's regression gates is the same device mounted at **`/workspace/target`**
instead: sources stay exact, and the cached directory is the one a rustc build
spends its writes on (§5.2).

## 0. Corrections to the question's own framing

Three premises in the brief do not survive the source:

1. **`cache=auto` is not a mode you enable; it is the absence of flags.** In
   both implementations `CachePolicy` never appears in `init()` - it appears only
   in the *open reply* (`passthrough/mod.rs:1024-1045` upstream,
   `linux/passthrough.rs:702-717` in the fork): `never` sets `FOPEN_DIRECT_IO`,
   `always` sets `FOPEN_KEEP_CACHE`/`FOPEN_CACHE_DIR`, `auto` sets **nothing**.
   cang's `Config` default is `Auto` (`passthrough.rs:370` + `:260-261`), so
   cang already has virtiofsd's `--cache=auto` semantics; the only differences
   from upstream's default are `xattr: true` vs `false` (`:373` vs
   `passthrough/mod.rs:464`), no `Metadata` policy (fork `passthrough.rs:251-268`
   has `Never|Auto|Always` only), and the same 5 s/5 s timeouts
   (`passthrough.rs:368-369` vs `passthrough/mod.rs:458-459`).
2. **`FUSE_WRITEBACK_CACHE` is not "which of auto/always".** Neither mode sets
   it. It is a separate server option in both implementations - virtiofsd's
   `--writeback` (README, "Enable writeback cache") feeding
   `passthrough/mod.rs:1588-1595`, the fork's `Config.writeback`
   (`passthrough.rs:333`, `:900-903`). Both default to off. The QEMU-era doc
   sentence "`always` sets a long cache lifetime at the expense of coherency"
   is about page retention, not about write buffering.
3. **"how `auto` decides per-open (direct IO vs cached)" - it never decides.**
   `auto` emits neither flag, so every open is cached and every open invalidates
   the inode's pages (see §1.2). The mode that mixes per-object is `metadata`
   (`passthrough/mod.rs:1031-1037`: DIRECT_IO for files, KEEP_CACHE|CACHE_DIR for
   directories) and the fork does not implement it.

Everything else the brief assumes - one share mounted as `/`, FUSE options per
connection, `/workspace` a bind graft into that share - is correct and is
re-confirmed in §3.

## 1. The three modes, mechanically

### 1.1 What each mode touches, and where

| | `never` | `auto` (cang today) | `always` | `--writeback` (orthogonal) |
|---|---|---|---|---|
| FUSE_INIT flags | none | none | none | `FUSE_WRITEBACK_CACHE` (bit 16, `fuse.h`) if the guest offered it |
| fork: init source | - | - | - | `passthrough.rs:900-903` → `server.rs:937` intersection |
| per-open reply (`fuse_open_out.open_flags`) | `FOPEN_DIRECT_IO` for non-dirs (`passthrough.rs:705-708`) | nothing (`:716`) | `FOPEN_KEEP_CACHE`, or `FOPEN_CACHE_DIR` for dirs (`:709-715`); `create` sets `KEEP_CACHE` unconditionally (`:1213-1217`) | unchanged |
| guest page cache, file data | bypassed | used, invalidated at every open | used, **kept across opens** | used; dirty pages |
| guest page cache, directories | n/a | dentries/readdir cached for `entry_timeout` | plus readdir results (`FOPEN_CACHE_DIR`) | same as its cache mode |
| `write(2)` | one FUSE_WRITE round trip, synchronously | one FUSE_WRITE round trip, synchronously (**write-through**) | same as `auto` | buffered into dirty folios; flushed later as ≤1 MiB `FUSE_WRITE`+`FUSE_WRITE_CACHE` |
| `close(2)` | `FUSE_FLUSH` (nothing dirty) | `write_inode_now(inode,1)` then `FUSE_FLUSH` | same as `auto` | same as `auto`, plus an extra `write_inode_now` in `fuse_release` |
| `fsync` | `FUSE_FSYNC` | flush dirty pages, then `FUSE_FSYNC` | same | same |
| `mmap` MAP_SHARED | **fails `-ENODEV`** unless the server advertises `FUSE_DIRECT_IO_ALLOW_MMAP` (the fork never does) | allowed; dirty pages written back | allowed; pages kept | allowed; dirty pages |
| `mmap` MAP_PRIVATE | allowed (faults via FUSE_READ) | allowed | allowed | allowed |
| host sees a guest write | when `write(2)` returns | when `write(2)` returns | same | when the inode is next flushed - at the latest `close(2)`, which is a barrier |
| guest sees a host write | immediately | at the next open, or after the attr timeout's mtime check | only after an mtime check or attr expiry | **never** for a regular file (see §1.4) |

Citations, in the order of the rows: `passthrough/mod.rs:1024-1045` and
`linux/passthrough.rs:669-720`; `fs/fuse/file.c:1495-1554` (`fuse_cache_write_iter`,
`:1541` `iomap_file_buffered_write`, `:1546` `fuse_perform_write`), `:1356-1409`
(`fuse_perform_write` - synchronous `FUSE_WRITE` per chunk, pages left clean),
`:2127` (`FUSE_WRITE_CACHE` on writepage args), `:467-518` (`fuse_flush` →
`write_inode_now(inode, 1)`), `:397-412` (`fuse_release`),
`:540-579` (`fuse_fsync` → `file_write_and_wait_range` + `fuse_sync_writes`),
`:2397-2450` (`fuse_file_mmap`, `:2422-2428` the DIRECT_IO/`VM_MAYSHARE`
`-ENODEV`, `:2432-2435` `MAP_PRIVATE`), `fs/fuse/inode.c:1269-1326`
(`process_init_reply`), `include/uapi/linux/fuse.h` (`FOPEN_*` history at 7.2 /
7.28, `FUSE_WRITEBACK_CACHE` at 7.23, `FUSE_DIRECT_IO_ALLOW_MMAP` at 7.39 =
`1ULL << 36`).

Two facts worth stating explicitly because they are usually got wrong:

- **`DIRECT_IO_ALLOW_MMAP` is not in the fork at all.** `grep -r
  DIRECT_IO_ALLOW_MMAP deps/libkrun` returns nothing - not in `FsOptions`
  (`fuse.rs:314` neighborhood), not advertised. virtiofsd exposes it as
  `--allow-mmap` (`README`; `passthrough/mod.rs:1638-1640`). So under
  `cache=never` on this fork `MAP_SHARED` is impossible, which also means
  `cache=never` is not a safe "just make it coherent" alternative for anything
  that mmaps a file for writing (rustc/cargo mmap **reads** as `MAP_PRIVATE`, so
  a build would survive; a linker writing through a mapping would not).
- **`close(2)` is a flush barrier in every mode, including writeback.** What
  writeback removes is not the ordering guarantee, it is the round trip *per
  `write(2)`*: bytes are summed in dirty folios and leave as ≤1 MiB background
  requests, and `fuse_flush` (`file.c:482`) then waits for the inode's writeback
  before replying. A guest fd that stays open is the case where the host sees
  nothing for an arbitrarily long time - not a file that is written and closed.

### 1.2 `auto` = close-to-open, and where its invalidation actually comes from

`auto` keeps the page cache but invalidates it on **open**:
`fuse_finish_open` → `else if (!(ff->open_flags & FOPEN_KEEP_CACHE))
invalidate_inode_pages2(inode->i_mapping)` (`file.c:296-297`; the same shape for
`create`/`O_TRUNC` at `:291-298`). A file re-opened N times in one build is
re-read N times, which is exactly the pattern a 2401-crate compile produces
(`.rs` read by rustc, same file re-read for dep-info/fingerprint).

The second invalidation channel is `AUTO_INVAL_DATA` (negotiated; note 01 §1.4):
on any attribute refresh the kernel compares mtime/size and drops the pages if
they moved (`inode.c:355-374`). That is why `auto` is "like NFS" - and why the
`qemu` doc's "1 second metadata cache timeout" is stale: both current
implementations default to **5 s** (`passthrough.rs:368-369`,
`passthrough/mod.rs:458-459`), and virtio-fs forces `default_permissions` so the
guest never even sends FUSE_ACCESS.

The third channel - the one everybody assumes exists - **does not exist in
either implementation**: neither the fork nor virtiofsd sends
`FUSE_NOTIFY_INVAL_INODE`/`INVAL_ENTRY` (`grep -c 'INVAL_INODE|inotify'` = 0 in
both `server.rs` and `passthrough/mod.rs`; the fork only answers
`NOTIFY_REPLY`, `server.rs:143`,`:1252`). There is no host→guest invalidation
path at all, so "the server could invalidate when the host file changes" is not
a lever this codebase has - it would be new work in both implementations, and
it is the only design that would make caching on a live host tree *safe* rather
than merely fast.

### 1.3 `always` = keep the pages

`FOPEN_KEEP_CACHE` suppresses the open-time invalidate, so a page stays in the
guest until something else removes it: the `AUTO_INVAL_DATA` mtime/size check
(`inode.c:355-374`), memory pressure, or a server notification (none exists).
For directories `FOPEN_CACHE_DIR` (`fuse.h`, added 7.28) caches readdir results
too, so a directory listing can outlive the entries themselves.

Both implementations document the precondition in the enum itself - "This policy
should only be selected when the file system has exclusive access to the
directory" (`passthrough.rs:263-267`, verbatim in
`passthrough/mod.rs`'s `Always`) - which cang's `/workspace` never satisfies.

### 1.4 Writeback: what it changes, and the safety net it removes

`WRITEBACK_CACHE` makes `write(2)` a page-cache copy (`file.c:1506-1516`,
`:1541`) and the flush path a background `writepages`
(`FUSE_WRITE` with `FUSE_WRITE_CACHE`, `:2127`). Two consequences the server has
to honour (the fork does, copied from crosvm/virtiofsd):

- the guest may `read(2)` a file it opened `O_WRONLY`, so `open_inode` turns
  `O_WRONLY` into `O_RDWR` (`passthrough.rs:499-506`);
- the kernel owns `O_APPEND`, so the server must clear `O_APPEND`
  (`passthrough.rs:508-516`) - i.e. **the host no longer gets the guest's append
  atomicity**, which is only sound if nobody else appends to that file.

The mechanism that matters for bob's corruption question is smaller and sharper
than "writes are late":

```c
 304	u32 fuse_get_cache_mask(struct inode *inode)
 305	{
 306		struct fuse_conn *fc = get_fuse_conn(inode);
 307
 308		if (!fc->writeback_cache || !S_ISREG(inode->i_mode))
 309			return 0;
 310
 311		return STATX_MTIME | STATX_CTIME | STATX_SIZE;
 312	}
```

`fs/fuse/inode.c:304-312`. With writeback on, every regular file of the
connection gets a non-zero cache mask, and `fuse_change_attributes_i` then
**skips the mtime/size invalidation entirely**:

```c
 355		if (!cache_mask && S_ISREG(inode->i_mode)) {
 356			bool inval = false;
 ...
 369			} else if (fc->auto_inval_data) {
 ...
 375			if (inval)
 376				invalidate_inode_pages2(inode->i_mapping);
```

(`inode.c:355-376`; the kernel also *trusts* its own size/mtime over the
server's, `:329-345`.) So `WRITEBACK_CACHE` does not merely add latency - it
disables, for the whole connection, the one channel that would let the guest
notice that the host changed a file. `AUTO_INVAL_DATA` stays negotiated and
becomes inert for regular files. That is the precise reason the fork's own
`Config` doc says "the server does **NOT** have exclusive access ... can increase
the risk of data corruption" (`passthrough.rs:320-333`), and it is also why the
kernel refuses `FUSE_PASSTHROUGH` together with writeback (`inode.c:1387-1395`).

### 1.5 The four combinations that exist, not the two the ticket names

Ticket 16 offers a choice between "`cache=auto`-equivalent" and "`cache=always`-
equivalent (writeback cache)". Those are two independent axes in this server
(§0.2), so there are four reachable combinations and the implementing change
should pick one **by name**:

| combination | per-open reply flags | `WRITEBACK_CACHE` | `write(2)` | host edit seen by the guest | what it buys |
|---|---|---|---|---|---|
| `Auto` (cang today) | none | no | synchronous | at the next `open(2)` | nothing new |
| **`Auto` + writeback** | none | yes | buffered, flushed by close/writeback | at the next `open(2)` (best effort, see below) | the write half, with the smallest read-side cost |
| `Always` | `KEEP_CACHE`/`CACHE_DIR` | no | synchronous | only when an attr refresh notices mtime/size (5 s) | re-reads avoided, no write win |
| **`Always` + writeback** | `KEEP_CACHE`/`CACHE_DIR` | yes | buffered | **not at all** for a regular file (`cache_mask`, §1.4) | both halves, at full fidelity cost |

The second row is the one worth arguing for: with `Auto` + writeback the
open-time invalidate still runs - `file.c:296-297` is independent of
`cache_mask` - so a host edit to a source file *is* picked up at the guest's next
`open(2)`, and only the dirty-page case is exposed. Two caveats, both real: the
call's return value is ignored (`file.c:296-297`), and `invalidate_inode_pages2`
waits for writeback and returns `-EBUSY` for pages it cannot discard
(`mm/truncate.c`: "Return: -EBUSY if any pages could not be invalidated"), so it
is best-effort rather than a barrier. Within a single open, a host edit is
invisible in every row - that part cannot be bought back at any mode.

## 2. The fork today, and the exact knobs

### 2.1 The fields exist; only the defaults decide

`passthrough::Config` (`linux/passthrough.rs:299-363`) already carries
everything: `entry_timeout` `:307`, `attr_timeout` `:314`, `cache_policy` `:318`,
`writeback` `:333`. The `Default` impl (`:365-380`) is 5 s / 5 s / `Auto` /
`false` / `xattr: true`.

`Fs::new` builds that config for the device and passes **only** `root_dir`,
`semantics` and `attr_timeout`:

```rust
  88	        let fs_cfg = shared_dir.map(|root_dir| passthrough::Config {
  89	            root_dir,
  90	            semantics,
  91	            attr_timeout,
  92	            ..Default::default()
  93	        });
```

`src/devices/src/virtio/fs/device.rs:88-93` (`attr_timeout` itself is chosen at
`:80-86`: 0 s for `LinuxSimplified`, else 5 s). The config is cloned into the
worker at `activate` (`device.rs:229`) and turned into a `PassthroughFs` there
(`worker.rs:110-120`), so any setter must run before activation - which is what
`FsDevice::attach` is (`device_builders.rs:711-734`).

`FsDevice` exposes exactly five knobs and none of them is caching
(`device_builders.rs:626-667`; C API `include/libkrun.h:221-253`):
`new`, `new_read_only`, `new_null`, `set_overlay`, `set_dax_window_size`.

### 2.2 Which single knob is which

| target semantics | exact edit | size |
|---|---|---|
| "like virtiofsd `cache=auto`" | **nothing** - it is the deployed behaviour (`passthrough.rs:370` + `:260-261`). The only auto-like delta left is the 5 s timeouts, already at parity. | 0 |
| "like virtiofsd `cache=always`" | `cache_policy: CachePolicy::Always`, one statement: either move `#[default]` from `Auto` (`passthrough.rs:260`) to `Always` (`:267`), or set it in `Fs::new` (`device.rs:88-93`). Effect is `passthrough.rs:709-715` + `:1213-1217`. | 1 line |
| "like virtiofsd `--writeback`" | `writeback: true` at `passthrough.rs:371` (or in `Fs::new`), picked up by `init` at `:900-903` and intersected into the reply at `server.rs:937`. | 1 line |

Both one-liners are **device-global**: there is no `set_cache_policy`/
`set_writeback` on `Fs` or `FsDevice`, so a flip changes every share the VMM
creates. cang creates one (§3.1), so today "global" and "the share" coincide -
which is exactly why scoping needs a second device rather than a second knob.

## 3. Scoping: what is per connection, per open, per reply

### 3.1 The connection is the grant, and cang has exactly one

`fuse_send_init` runs once per **superblock**: `fuse_fill_super` →
`fuse_fill_super_common` → `fuse_send_init(fm)` (`fs/fuse/inode.c:1816-1833`),
and the negotiated state lives in that superblock's `struct fuse_conn`
(`fc->writeback_cache`, `inode.c:1325-1326`). A virtio-fs superblock is
identified by *the device* (`virtio_fs_test_super`, note 05 §1.2), so:

- one device ⇒ one connection ⇒ one set of INIT flags for every inode under it;
- **re-mounting the same tag elsewhere reuses the existing superblock** and
  silently keeps the old flags (note 05, `virtio_fs.c:1673-1680`, `:1735`) - the
  "mount `/workspace` twice with different options" trick does not exist;
- cang registers exactly one device: `fs_device_new(CANG_FS_ROOT_TAG,
  &config.task_rootfs, false)` (`crates/cang/src/runtime/vm/libkrun/launcher.rs:255-270`,
  tag `"/dev/root"` at `crates/cang-libkrun/src/lib.rs:25`), and the guest mounts
  it as `/` from the kernel cmdline.

So yes - **`/nix`, `~/.cargo`, `~/.cache/sccache` and `/workspace` all sit on one
`fuse_conn`**, and a `WRITEBACK_CACHE` bit set at INIT covers every one of them.
Per-path caching *for writeback* is impossible in the protocol, not merely
unimplemented: there is no per-inode or per-path writeback flag, and
`fuse_get_cache_mask` keys off `fc->writeback_cache` alone (`inode.c:304-312`).

### 3.2 What *is* scoped per path, and why it is not enough

Two of the three levers are per-object in the protocol:

- `FOPEN_DIRECT_IO` / `FOPEN_KEEP_CACHE` / `FOPEN_CACHE_DIR` are fields of the
  per-open reply (`struct fuse_open_out.open_flags`,
  `include/uapi/linux/fuse.h`), so a server may answer differently per open.
- `entry_valid` / `attr_valid` are per-reply (`struct fuse_entry_out`,
  `struct fuse_attr_out`), stamped from `cfg.*_timeout` at
  `passthrough.rs:1011-1018` and `server.rs:1163-1166`.

So a **path-scoped `always` (and path-scoped timeouts) is implementable inside
one share**, ~40 lines in the fork: resolve the opened inode's host path (the
server already goes through `/proc/self/fd/<fd>`, `passthrough.rs:496-528`, so
one `readlink` per open is enough), compare against a configured prefix, and set
`KEEP_CACHE` only there. It cannot ever carry writeback, and its benefit is
smaller than the write half of a build - noted here so nobody thinks the
"one-line `always`" is scoped. **Recommendation: do not do this.** It buys the
cheap half with the whole fidelity cost (see §4: `always` alone already breaks
the "host edit then guest rebuild" gate), and it costs a per-open syscall.

### 3.3 A second device: what the fork already gives you

Verified end to end in the fork - **the transport needs no change at all**:

- each device registers under its own id, `format!("virtiofs{}",
  ctx.device_index())` (`device_builders.rs:733`);
- the MMIO manager keys devices by `(DeviceType, device_id)` and hands each one
  its own 4 KiB window and IRQ (`vmm/device_manager/kvm/mmio.rs:81-102`,
  `:122-163`);
- the guest is told about every one of them: the cmdline loop walks
  `virtio_mmio_devices()` and appends a `virtio_mmio.device=` token per device
  (`vmm/builder.rs:1414-1432` → `kvm/mmio.rs:167-184`);
- `Fs` is per-device already: its own worker thread, queue pair, `passthrough_cfg`
  and `shm_region` (`device.rs:47-113`, `:203-245`).

The only fork work is the knob, and the C header is generated, not hand-edited
(`docs/bindings-generation.md:88`, `make gen-libkrun-bindings`):

| layer | change |
|---|---|
| fork `passthrough` | none - `Config` has `cache_policy`/`writeback` |
| fork `Fs` (`device.rs`) | `pub fn set_cache_config(&mut self, policy: CachePolicy, writeback: bool)` mutating `self.passthrough_cfg`, same shape as `set_shm_region` (`:119-121`) |
| fork `FsDevice` (`device_builders.rs:626-667`) | one `#[ffier::export]` method `set_cache_mode(&mut self, policy, writeback)` locking `inner` and calling it; regenerate `include/libkrun.h` (`:250-251` is where the new declaration lands, next to `krun_fs_device_set_dax_window_size`) |
| `crates/cang-libkrun` | one trait method beside `fs_device_new` (`api.rs:50-51`) and its `Slot::FsDevice` arm (`linked.rs:317-338`; the slot already holds one `FsDevice` per handle, so no enum change) |

That is ~30 lines in the fork + bindings, ~15 in `cang-libkrun`.

### 3.4 A second device: what cang and the guest need

This is the part that is real work, because cang deliberately collapsed a
per-mount design into one grafted tree:

- **Host side today.** `config.mounts` is a list of `BindMount { source, tag,
  target, source_kind, read_only }` (`config/model.rs:179-185`), each already
  carrying a tag (`WORKSPACE_TAG = "cang-workspace"`, `WORKSPACE_TARGET =
  "/workspace"`, `model.rs:14-15`, contributed by
  `components/mounts/workspace.rs`). `prepared_root` does not pass tags to the
  guest: it `mount --bind`s every source **into** the one exported tree
  (`prepared_root/mod.rs:103-139` plan, `:150-190` apply, `:218-239` the
  `mount --bind`), and the single `FsDevice` exports that whole tree.
- **Guest side today.** Nothing mounts anything: `cang-guest-init` only *checks*
  that the grafted targets exist (`runtime/cang.rs:23-31`
  `PREPARED_ROOT_TARGETS`, `:500-521` `validate_prepared_root_path*`, which
  accepts any existing directory), and the only `libc::mount` calls in the crate
  are tmpfs/userns plumbing (`components/rootless/idmap.rs:155-167`). The env
  channel that used to describe mounts is deliberately gone - `config/tests.rs:
  107-122` asserts no `CANG_MOUNT_*`/`CANG_WORKSPACE_TAG` key reaches the guest.
- **Guest side needed.** One mount step early in `enter`, before
  `validate_prepared_root_paths` and before the workload exec: retry
  `mount("cang-workspace", "/workspace", "virtiofs", 0, "")` (or the `target/`
  variant, §5) until the tag appears, then `create_dir_all` any mountpoint the
  workload needs below it. The guest needs the tag string, so exactly one env
  var has to come back through the `guest_config_env` allowlist - which is the
  assertion `config/tests.rs:107-122` must be updated to permit.
- **Host side needed.** In device mode the workspace mount must **not** be
  grafted (an empty mountpoint directory instead; the `BindMount` entry itself
  has to stay, because `validate_mounts`/`workspace_mount`
  (`config/components/mounts.rs:33-71`) reject a mount list without a
  `/workspace` target - only its *application* changes), otherwise the same host
  directory is exported twice - two `fuse_conn`s, two inode maps, two caches over
  one tree, which is §4's worst row before the guest even starts. `cang` must
  also create the host directory that backs the device (the existing
  `validate_source_dir` at `prepared_root/mod.rs:301-310` is the natural gate).

Total, realistically: fork+bindings ~40, `cang-libkrun` ~20, `launcher` ~25,
`prepared_root` ~20, guest-init ~50, plus tests on both sides and two
repository-invariant tests that currently assert the absence of the env key.
**~150-200 lines; no fork transport, MMIO, cmdline, queue or pin change.**

## 4. What corrupts, and what the guest sees

Assumption for this section: caching (either `always` or `writeback`, or both) is
on for a share that contains `/workspace`, i.e. the host's live working tree.

### 4.1 The missing channel is the whole problem

Because no invalidation notification exists (§1.2), every "host changed it" case
degenerates to a timeout question:

| host action | mechanism | guest sees | damage |
|---|---|---|---|
| edit a source file in place (editor, `sed -i` keeps the inode) | guest pages still mapped; `always`: no invalidation at all until an attr refresh; `writeback`: `cache_mask` suppresses even that (`inode.c:355-362`) | old bytes, **no error** | a build compiles stale input and writes a wrong artifact; if the guest also writes the file, its flush lands over the host's edit - **lost host edit** |
| `git checkout`/`git switch` (writes new content, often via replace/rename) | replaced path = new inode; the guest's dentry still binds the old nodeid | old file forever (dentry cached, then revalidated against a name that now maps to a different inode) | guest writes go to the **unlinked** inode and vanish on close, silently; host's new content intact but the guest never sees it |
| `git gc`, `git prune`, `rm -rf` of a tree the guest has open | the server resolves writes through its `O_PATH` fd into `openat(/proc/self/fd/N)` (`passthrough.rs:496-528`), so deleted-but-open files stay writable | reads/writes succeed on a file the host no longer has | silent loss at close; guest ENOENT on paths the host already recreated |
| host build in the same tree (`cargo build` in `target/`) | both sides write the same paths, each with its own page cache and its own flush order | torn `.o`/`.rlib`/binaries, `invalid metadata`, linker "file truncated" | **corruption of `target/`** that looks like a cang bug, not like contention |
| a second cang task / second VM on the same workspace | two `fuse_conn`s, two inode maps, two caches, two servers, both believing they have exclusive access (`passthrough.rs:320-333`) | ENOENT storms on directories one side just created, half-written files, size/mtime disagreement | **already observed in this map**: ticket 10 traced the original ENOENT to concurrent VMs on one workspace, and closed it by requiring `require_workspace_free` |
| host process reading mid-flush | writes leave in ≤1 MiB requests | a host `cat`/`md5sum`/watcher can read a partially written file | misleading checksums, spurious "file changed" events |
| host `truncate`/`ftruncate` while the guest has dirty pages | guest's local `i_size` wins (`inode.c:329-345`) | pre-truncate size, then a hole-plus-data mix after flush | corrupt file on the host |
| host only *reads* | none | nothing | none |

Note what is **not** new: unlinked-but-open files, host-created files invisible
for up to `entry_timeout`, and a host `cargo check` racing a guest build are all
true **today** with `cache=auto` + 5 s timeouts. Caching worsens the window from
"seconds, bounded by metadata timeouts" to "unbounded for anything with a cache
mask", and turns read staleness into lost writes.

### 4.2 What the guest reports - and why a warning is the only channel

Nothing in §4.1 produces an error in the guest: a stale read is a cache hit, a
lost host edit is a successful `write(2)`, and a torn ELF is only caught later by
rustc's or the linker's own integrity checks, if at all. The server cannot detect
the conflict either - it holds no comparison point, and `AUTO_INVAL_DATA` is
precisely the comparison it is being denied (§1.4). So there is **no runtime
diagnostic to add**; the honest mechanism is a launch-time warning plus docs.

Ticket 16 asks for four facts in that warning; here they are as text, each one
traceable to §4.1:

- **guest writes may reach the host late or never** - late by up to a
  `close(2)` (the flush barrier), never if the VM is killed with pages still
  dirty (`target/` after a killed build is partly written, partly absent);
- **a host process touching the directory can corrupt what the guest sees** -
  edits, builds, `git checkout`, `git gc`, `rm`, and a second cang task on the
  same tree are all in that class, and the *silent* outcome is the dangerous one;
- **what the guest sees specifically**: a *stale* read if the host edited a file
  the guest has open (no error, old bytes - it then writes derived output that is
  wrong), and a *torn* read if the host reads a file the guest is writing
  mid-flush; the reverse - the guest's next flush landing over the host's edit -
  is the corruption that loses the host's work;
- **which directory** the flag applies to, so a scoped mode is not misread as
  "everything under `/`".

Drafted text (stderr, once per launch, before the VM starts):

```
cang: warning: --workspace-cache=<mode> is ON: <host path> -> /workspace
  In this mode the guest caches file data for that directory. Guest writes may
  reach the host late (at the latest when the guest closes the file) or NOT AT ALL
  if this VM is killed. While this task runs:
    * do not edit, build, `git checkout`, `git gc`, or delete anything under
      <host path>, and do not start a second cang task on it - the guest cannot
      see your changes and its next flush can overwrite them;
    * do not read files there expecting guest output to be current.
  What you will see if you do: stale reads in the guest (no error), a build whose
  output is wrong, or a host file that silently reverts. Nothing reports it.
  Leave the flag off when a host tool must see guest output live.
```

Note the wording deliberately does **not** promise "only the guest can corrupt
the tree" (the host can corrupt the guest's view just as easily) and does not
name a runtime check, because none exists.

## 5. The smallest change, now that bob has reopened it

bob's decision (2026-10-26) fixes the shape: `/workspace` only, opt-in, warned.
Ranked by (work x coherence cost) *within that decision*:

### 5.1 The change

**A second virtio-fs device on the host workspace directory, mounted at
`/workspace`, opt-in, default off.** §3.3-3.4 is its entire inventory (~150-200
lines); the fork's transport, MMIO manager, cmdline and device-index plumbing
already support it, so the fork work is one `FsDevice` setter plus a tag, a pin
move and `flake.lock` (rule: a fork change means tag `v2.0.0-cang.<n>`, submodule
pointer, flake input, and the live smokes of project rules #223/#229).

Surface, concretely:

| decision | smallest form |
|---|---|
| flag | `cang --workspace-cache=<auto\|always\|auto+writeback\|always+writeback\|off>`; default `off`. `cang.toml [workspace] cache = "<mode>"` for a persistent choice. Per-run by construction: two runs in different modes never share a VM, so the mode never has to be reconciled against a running task. |
| device | `fs_device_new(WORKSPACE_TAG /* "cang-workspace", model.rs:14 */, <host workspace>, false)` + `set_cache_mode(policy, writeback)`; the tag already exists in `BindMount.tag` and is unused by the guest today. |
| mount | guest-init: retry `mount("cang-workspace", "/workspace", "virtiofs", 0, "")` before `validate_prepared_root_paths`; host: do **not** graft that mount into the prepared root (empty mountpoint instead), or the same tree is exported twice. |
| root share | untouched - `Auto`, no writeback, 5 s timeouts. That is what satisfies "never `/nix`, never the cargo registry". |
| warning | §4.2, printed before VM start, plus `docs/usage.md`; gate it on the flag so the default path prints nothing. |

Which of the four modes (§1.5) to ship first: **`auto+writeback`** is the best
buy/risk pair - it captures the write half (the only thing the 12%-busy worker
cannot fix) while a host edit is still picked up at the guest's next open;
`always+writeback` is what the ticket's wording implies and it is the one with
the unbounded staleness window. `always` alone buys re-reads and nothing on the
write side; measure `auto+writeback` against it if the harness has room.

### 5.2 What must be proved, and the one variant that keeps every gate

Ticket 16's gates (from ticket 03) are: host edit -> guest rebuild -> host commit
sees every file; host `git status` agrees with what the guest wrote; a
hard-killed guest leaves the host tree usable; the existing smokes still pass.
`/workspace`-wide caching puts the first two at risk by construction (§4.1), so
the ticket can only run them "where they still make sense" - and the honest
statement is that with the flag on, they no longer hold for sources.

There is a strict subset that keeps all four: **mount the cache device at
`/workspace/target` instead of `/workspace`** (host side: `<workspace>/target`,
created by cang before launch; guest side: `mkdir -p /workspace/target` after the
root share is up, then mount). Then:

- sources, `.git`, `Cargo.toml`, `~/.cargo`, sccache and `/nix` keep today's exact
  visibility, so gates 1-2 stand verbatim and gate 3 is a build-output question;
- the cached directory is exactly the one a rustc build spends its writes on
  (`.o`, `.rlib`, `.rmeta`, `.d`, fingerprints) plus the re-reads of the same
  artifacts on a second pass;
- it is the same device and the same mount step as the ticket's shape - one
  different host path and one different mountpoint - so it is a subset, not an
  alternative architecture, and switching to the full-`/workspace` variant later
  is a flag/plumbing change, not a redesign;
- it is the same idea as [ticket 02](../tickets/02-rust-compile-baseline-harness.md)'s
  "`CARGO_TARGET_DIR` on a guest-local disk" control arm, except the artifacts
  still land on the host (late), so a host build after the task still sees them.
  Nothing in this repo sets `CARGO_TARGET_DIR` today (grep: no hits outside that
  ticket), so the arm has to assert cargo is using the default `target/`.

Measured bar: ~5% of the guest-virtiofs arm (the map's run-to-run spread), i.e.
~15 s of 299 s; only the arms that actually write `target/` can show it. The
three-arm run becomes four: host / guest-local / guest-virtiofs / guest-virtiofs
cached.

### 5.3 Rejected

- **One-line default flips** (`writeback: true` at `passthrough.rs:371`, or
  `#[default] Always` at `:260`): one line each, and the worst option - the
  connection is `/`, so `/nix`, `~/.cargo`, `~/.cache/sccache`, `/etc` and
  `/home` inherit the semantics, and a guest-side bug writing to `/nix/store`
  would be delayed into the host's store. Contradicts the decision bob just made.
- **Path-scoped `always` inside the existing share** (per-open `KEEP_CACHE`,
  §3.2): ~40 fork lines, no mount work, no writeback - so it buys a fraction of
  the write half while giving up read freshness on the files that get edited.
- **`cache=never` anywhere**: on this fork it also makes `MAP_SHARED` impossible
  (§1.1), and it is strictly slower than today.

## 6. What this note does not settle

- No numbers. Whether `target/`-scoped writeback move the 299 s arm by more than
  the map's ~5% spread is a measurement question (ticket 02's harness, one extra
  arm), not a mechanism one. Mechanism says the write half is real work -
  `FUSE_WRITE` per `write(2)` on the single worker today - and that writeback is
  the only lever that removes it.
- Whether an in-guest `FUSE_NOTIFY_INVAL_*` implementation (a server that walks
  the host tree or takes inotify and pushes invalidation) is worth building. It
  is the only design that makes caching on a live host tree sound, it exists in
  neither implementation, and it is a project of its own - out of scope here, but
  it is the answer to "how do large virtio-fs deployments live with caching on a
  shared tree", and cang is not one.
- Whether the 30% penalty is worth any of this at all. The map's own reading
  stands: the VM penalty is 2.2× the host against virtio-fs's 1.3×, so caching
  `/workspace` chases the smaller half while spending the promise that makes
  `/workspace` useful.
