---
label: wayfinder:research
title: Is dropping FUSE_FLUSH safe? - what the close-time request actually carries, what FOPEN_NOFLUSH skips, and the interposer that costs no kernel patch
status: done
blocked_by: []
claimed_by: pi flush-noflush child (2026-10-27)
---

# 33 - Is dropping FUSE_FLUSH safe?

Ticket: under zero-message open the guest still sends `FUSE_FLUSH` on every
`close(2)` - 212,052 of the 1,000,000th request of a cold 2401-crate build,
**21.21%** (note 23 section 2) - and the proposal on the table is two lines in the
libkrunfw fork: add `FOPEN_NOFLUSH` to the kernel's "Default for no-open"
`open_flags` (`fs/fuse/file.c:154-156`). This note traces what that flag would
skip, what the server keeps on FLUSH, who in practice holds the locks FLUSH is
supposed to release, and what the kernel's own authors say the flag is for.

Kernel source read: **upstream stable `v7.2.10`** from git.kernel.org
(`fs/fuse/file.c`, `dir.c`, `inode.c`, `include/uapi/linux/fuse.h`, `fs/open.c`),
i.e. exactly the tarball `deps/libkrunfw` builds (`Makefile:1 KERNEL_VERSION =
linux-7.2.10`). The guest kernel is `v7.2.10-hardened1`: the hardened patch
(103 KB, fetched from its release URL) contains **zero** `fs/fuse` hunks, and none
of the fork's 23 `patches/*.patch` touch `fs/fuse` either, so these line numbers
are the guest's line numbers. Server citations are the fork at
`/home/dev/cang/cang/deps/libkrun` = `d1c58a99` (v2.0.0-cang.12; `.12` is `.11` plus
a zstd advisory bump that does not touch virtio-fs). The `cang-vfio` checkout
(`9e423e76` + an uncommitted lookup-histogram patch) has a **byte-identical**
`linux/passthrough.rs` and a `server.rs` that differs only by those lookup
counters, so its `server.rs` line numbers sit ~+14 from the ones quoted here.

## Verdict

1. **Nothing about locks is lost. Not one byte of state.** FLUSH's `lock_owner` is
   only meaningful to a server that implements remote locking
   (`libfuse: "If the filesystem supports file locking operations (setlk, getlk)
   it should remove all locks belonging to 'fi->owner'"`). This server never
   advertises `FUSE_POSIX_LOCKS`/`FUSE_FLOCK_LOCKS`, has no `setlk`/`getlk`/flock
   handler at all, and ignores the `lock_owner` argument of its `flush`
   (`passthrough.rs:1901`). The guest's POSIX locks are therefore **local**: they
   live in the guest kernel's own lock table (`fs/fuse/file.c:2576-2577` under
   `fc->no_lock`) and are dropped by the VFS at `close(2)` whether or not any FUSE
   request happens (`fs/open.c:1475 locks_remove_posix`, which runs after
   `->flush` and is independent of it). RELEASE's `FUSE_RELEASE_FLOCK_UNLOCK` is
   unreachable under zero-message open anyway (`file.c:112-113`), and flock is
   local too (`file.c:2590-2591` under `fc->no_flock`) - so that path is already
   dead today, before any NOFLUSH change.
2. **The one semantic that is lost is real but narrow: `close(2)` stops being able
   to report a host-side write error.** Everything FLUSH does is
   `write_inode_now` + `filemap_check_errors` + one synchronous request whose
   server side is `dup()+close()` on the host descriptor (`passthrough.rs:1913,
   1918`) - the classic virtiofsd idiom for surfacing errors the host filesystem
   defers to `close`. libfuse says of exactly this: *"the filesystem is not forced
   to flush pending writes. One reason to flush data is if the filesystem wants to
   return write errors during close. However, such use is non-portable because
   POSIX does not require close to wait for delayed I/O to complete."*
3. **Do not ship the kernel-default version as filed. Ship the same request-count
   win as a server-side `-ENOSYS` on `FLUSH`, behind an opt-in launch flag.** The
   kernel already has a sanctioned "no flush" interposer with no kernel patch:
   answer FLUSH with `-ENOSYS` once, the kernel sets `fc->no_flush` and stops
   sending it (`file.c:490-492, 504-508`), which is also virtiofsd's own trait
   default (`filesystem.rs:795`, documented at `:785-787`). That removes the same
   21.21% of requests **and keeps `write_inode_now` + `filemap_check_errors`**,
   which the `FOPEN_NOFLUSH` early return (`file.c:479-480`) throws away. The
   kernel-side change costs a libkrunfw re-pin (firmware rebuild + tag + FLUSH
   gate); the server-side one costs one line in the fork's `flush`, is switchable
   per launch, and is trivially revertible.

## 1. What `fuse_flush()` does, in order, and what NOFLUSH skips

`fs/fuse/file.c:467-518` (v7.2.10), verbatim skeleton:

```
467  static int fuse_flush(struct file *file, fl_owner_t id)
476      if (fuse_is_bad(inode))
477          return -EIO;
479      if (ff->open_flags & FOPEN_NOFLUSH && !fm->fc->writeback_cache)
480          return 0;
482      err = write_inode_now(inode, 1);
486      err = filemap_check_errors(file->f_mapping);
490      err = 0;
491      if (fm->fc->no_flush)
492          goto inval_attr_out;
494      memset(&inarg, 0, sizeof(inarg));
495      inarg.fh = ff->fh;
496      inarg.lock_owner = fuse_lock_owner_id(fm->fc, id);
497      args.opcode = FUSE_FLUSH;
502      args.force = true;
504      err = fuse_simple_request(fm, &args);
505      if (err == -ENOSYS) { fm->fc->no_flush = 1; err = 0; }
515      if (!err && fm->fc->writeback_cache)
516          fuse_invalidate_attr_mask(inode, STATX_BLOCKS);
```

Facts:

1. **The NOFLUSH test is an early `return 0`, not a skip-the-request test.** It
   runs after `fuse_is_bad` (:476) and before everything else, so it skips *all
   four* of: the dirty-page writeback `write_inode_now(inode, 1)` (:482), the
   accumulated-writeback-error check `filemap_check_errors` (:486), the
   `FUSE_FLUSH` request itself (:494-508), and the writeback-cache `i_blocks`
   invalidation (:515-516).
2. **The test is inert unless `writeback_cache` is off**: `&& !fm->fc->writeback_cache`
   (:479). cang never negotiates it - see fact 12 - so the test is live here.
3. **The early return is not "no request"; it is also "no writeback".** That
   distinction is the whole difference between the two candidate levers (section 6).
4. **What the request carries:** `fh` (0 under zero-message open), `lock_owner`
   (`fuse_lock_owner_id` = the closing fd's `files_struct` pointer, XTEA-scrambled,
   `file.c:427-443`), no flags, no data - and `force = true`, so it is a *synchronous*
   round trip that `close(2)` waits on.
5. **What the kernel does on -ENOSYS:** caches `fc->no_flush = 1` and treats it as
   success (:505-507); every later `fuse_flush` jumps straight to `inval_attr_out`
   (:491-492), i.e. **it keeps :482 and :486**. This is the interposer in fact 3 of
   the verdict.
6. **Under `writeback_cache` = 0 (cang), `write_inode_now` is normally a no-op with
   a caveat.** It reaches `fuse_write_inode` (`file.c:2056-2068`) ->
   `fuse_flush_times` (`dir.c:2102-2127`), which sends a **`SETATTR`** for
   mtime/ctime - but `fuse_write_inode` returns immediately unless the inode has a
   `write_file` on `fi->write_files`, and that list is only populated by
   `fuse_link_write_file` under `writeback_cache` (`file.c:231-232`) **or by a
   `MAP_SHARED` `VM_MAYWRITE` mmap** (`file.c:2450-2451`). So with writeback off
   the flush-time writeback is a no-op for ordinary `write(2)` files and does real
   work only for a file the guest has memory-mapped shared and dirtied.
7. **FLUSH is sent on every `close(2)`, not per open and not only for writers.**
   `filp_flush` (`fs/open.c:1460-1478`) calls `f_op->flush` for each fd close
   (:1470-1471), then `locks_remove_posix` (:1475). The kernel commit that added
   the flag says it plainly: *"FUSE flushes local write cache of ANY file, even non
   FMODE_WRITE and sends a 'flush' command to server (if server implements it)."*
   That is why a build whose opens are overwhelmingly `O_RDONLY` `open`/`read`/
   `close` cycles spends a fifth of its requests here.
8. **FLUSH is not the release.** `FUSE_RELEASE` is a separate request with
   `release_flags` (`fuse.h:509-510`), and under `fc->no_open` the kernel completes
   it locally without talking to the server (`file.c:101-131`, the
   `args->opcode == FUSE_RELEASE && ff->fm->fc->no_open` branch at :112-113).
   FLUSH is also *not* durability: `fsync` is `fuse_fsync` (:540-...) with its own
   writeback-and-`FUSE_FSYNC` sequence (:550-580).

## 2. Is `FOPEN_NOFLUSH` honoured when the kernel completed the open itself?

Yes, and it is honoured *only* there in a cang-like configuration.

9. `ff->open_flags` is initialised at `file.c:154-156` **before** the
   `if (open)` block: `ff->fh = 0; /* Default for no-open */
   ff->open_flags = FOPEN_KEEP_CACHE | (isdir ? FOPEN_CACHE_DIR : 0);`. When an
   OPEN/OPENDIR reply arrives it is *overwritten* by the server's flags (:164-165);
   on `-ENOSYS` the default stands and `fc->no_open`/`no_opendir` is set (:166-178,
   and for directories `ff->args` is freed at :170-173). So editing :156 affects
   **only** the no-open path - a mount that answers OPEN normally is completely
   unaffected (its flags come from :165). That is a genuinely reassuring property
   of this 2-line change: it cannot regress the non-zero-message arm.
10. The flag is consulted in exactly **one** place in the whole kernel:
    `ff->open_flags` appears at `file.c:156, 165, 182, 226/228, 296, 479, 1429,
    1684, 1851, 1872, 1887, 1899, 2423`, and `FOPEN_NOFLUSH` appears once
    (`:479`). `fuse_release` (:397-412) does not look at `open_flags` at all
    (it calls `write_inode_now` under `writeback_cache`, :405-406, then
    `fuse_release_common`), and `fuse_file_put` (:101-131) keys on
    `fc->no_open`, not on `open_flags`. `fuse_finish_open` (:216-235) reads only
    `FOPEN_STREAM`/`FOPEN_NONSEEKABLE`; `fuse_open`'s truncate branch reads
    `FOPEN_KEEP_CACHE` (:296). Nothing else in `fs/fuse` knows the flag.
11. Nothing clears it: the only masking is `ff->open_flags &= ~FOPEN_DIRECT_IO`
    for directories (:182), and directories have no `->flush` at all
    (`fuse_dir_operations`, `dir.c:2427-2436`, has `.open/.release/.fsync` and no
    `.flush`), so the flag is meaningless for the `FOPEN_CACHE_DIR` half either
    way: no `FUSE_FLUSH` is ever sent for a `closedir`.
12. **The `!writeback_cache` conjunct is satisfied in cang.** The fork's `supported`
    set is `ASYNC_READ | PARALLEL_DIROPS | BIG_WRITES | AUTO_INVAL_DATA | ASYNC_DIO
    | HAS_IOCTL_DIR | MAX_PAGES | ZERO_MESSAGE_OPEN | ZERO_MESSAGE_OPENDIR |
    SUBMOUNTS | HANDLE_KILLPRIV_V2 | INIT_EXT` (+`ALLOW_IDMAP`, +(ATOMIC_O_TRUNC)
    only when zero-message open is off) at `server.rs:1015-1042`; it contains
    neither `WRITEBACK_CACHE` nor `POSIX_LOCKS` nor `FLOCK_LOCKS`. `WRITEBACK_CACHE`
    can also arrive through `fs.init()`'s `want` (`enabled = capable & (want |
    supported)`, `server.rs:1068`), and `passthrough::init` does return it - but
    only `if self.cfg.writeback && capable.contains(WRITEBACK_CACHE)`
    (`passthrough.rs:1098-1100`), and `Config::writeback` defaults to `false`
    (`:467`) with **no setter anywhere**: `grep -rn writeback crates/` in cang is
    empty, and `Fs`/`FsDevice` expose no such setter (note 30 section 2). So
    `fc->writeback_cache == 0` in the guest, and `FOPEN_NOFLUSH` would take effect.
13. **The server cannot choose it per open - with one exploitable exception.**
    Under `no_open` the kernel sends no `FUSE_OPEN`, so there is no
    `fuse_open_out.open_flags` for the server to fill and :156 is the only source.
    But `FUSE_CREATE` still arrives (17,615 of the 1M-request histogram, 1.76%),
    and a create reply's `open_flags` goes straight into `ff->open_flags`
    (`dir.c:878-880, 896-898`). The fork's zero-message `create` already builds an
    `OpenOptions` and returns it (`passthrough.rs:1472-1479`), so it *could* set
    `FOPEN_NOFLUSH` for creates only. That is per-open control over the smallest
    slice of the traffic and it cannot reach the `O_RDONLY` opens that dominate the
    FLUSH count - i.e. it is not the narrower variant worth having.
14. `FOPEN_NOFLUSH` is a v7.35 protocol addition (`fuse.h:189`, value `1 << 5` at
    `:392`), so it needs a protocol minor >= 35 handshake; the fork declares
    `KERNEL_MINOR_VERSION` and rejects older minors (`server.rs:1005-1012`), so the
    guest is already >= 7.35 and the bit is meaningful.

## 3. What server-side state is keyed on FLUSH?

15. **The fork's `flush` is `dup()` + `close()` and nothing else**
    (`passthrough.rs:1896-1924`): `let data = self.any_handle_of(inode, handle)?;`
    then `dup()` the inode's lazily opened descriptor and `close()` the copy,
    returning `last_os_error()`. Its own comment (:1909-1911) says the method
    "is called whenever an fd is closed in the client, we can emulate that
    behavior by doing the same thing (dup-ing the fd and then immediately
    closing it)". **The `_lock_owner` parameter is unused** (:1901).
16. **There is no lock state in the server to release.** `grep -n 'fn setlk\|fn
    getlk\|fn flock\|fn setlkw' linux/passthrough.rs` = **0 hits**; there is no
    `libc::F_SETLK`, no `libc::flock`, no owner->locks map. Upstream virtiofsd's
    `flush` is the identical dup/close and also ignores `_lock_owner`
    (`virtio-fs/virtiofsd main, src/passthrough/mod.rs:2300-2324`).
17. `release` ignores `flush`, `flock_release` and `lock_owner` too
    (`passthrough.rs:1402-1419`, parameters `_flush`, `_flock_release`,
    `_lock_owner`), so even on a mount *without* zero-message open the
    `FUSE_RELEASE_FLOCK_UNLOCK` that `file.c:362-365` sets for an `ff->flock`
    file is discarded by this server.
18. **Under zero-message open the RELEASE never arrives at all**, so the last
    close-time message the server sees is the FLUSH. `fuse_file_put`'s
    `fc->no_open` branch completes it locally (`file.c:112-113`), which note 23
    section 1 relies on. The parent brief's phrasing needs one correction here:
    **`FUSE_FLUSH` carries `lock_owner`; `FUSE_RELEASE_FLOCK_UNLOCK` is a RELEASE
    flag** (`fuse.h:509-510`, set at `file.c:362-365`) - two different requests,
    two different mechanisms, and both are no-ops against this server.
19. **Guest locks are local and are released by the guest, not by FLUSH.**
    `fc->no_lock = 1` because `FUSE_POSIX_LOCKS` is not in `supported`
    (`inode.c:1298-1299`), so `fuse_file_lock` takes the `posix_test_lock` /
    `posix_lock_file` path (`file.c:2569-2580`) - the VFS's own lock table on the
    guest inode. The same for flock: `fc->no_flock = 1` (`inode.c:1301-1302`) ->
    `locks_lock_file_wait` (`file.c:2590-2591`). Whatever a guest process locks
    is removed at `close(2)` by `locks_remove_posix(filp, id)` (`fs/open.c:1475`),
    which runs **after** `f_op->flush` and **whether or not flush did anything** -
    an early `return 0` in `fuse_flush` cannot skip it, and neither can
    `fc->no_flush`.
20. **So the answer to "what leaks when FLUSH never arrives" is: nothing, in this
    stack.** The server holds no locks to leak (facts 15-17); the guest's locks are
    local and dropped by the VFS (fact 19). The two things that *do* change are
    (a) the host descriptor stops being dup/close-probed at every close, so a
    close-time host error is never reported, and (b) with the kernel-flag variant
    only, the guest skips `write_inode_now` (matters only for `MAP_SHARED`-dirtied
    mappings, fact 6) and `filemap_check_errors`.
21. The one real *latent* landmine to record: if the fork ever adds flock support
    (`FUSE_FLOCK_LOCKS` + `ff->flock`), lock release would ride **RELEASE**, which
    zero-message open already turns into a local no-op - so flock would be broken
    under ZMO *today*, independent of this ticket - and adding `FOPEN_NOFLUSH`
    would then also remove the FLUSH that a future implementation might have used
    as its cleanup hook. Whoever adds flock must add its own owner-keyed state and
    must not assume a close-time message exists.

## 4. Who in practice holds POSIX record locks on `/workspace` or `/`?

22. **This is the fact that makes the lock worry moot, and it is a mechanism
    argument, not a census**: lock traffic cannot be affected by FLUSH *because
    there is no remote lock traffic at all*. The guest kernel sends `FUSE_SETLK`
    only when `POSIX_LOCKS` was negotiated (`file.c:2574-2579`), and the fork's
    opcode histogram of a real build lists no `SETLK`/`SETLKW`/`GETLK` line at all
    (note 23 section 2, 30 opcodes, none of them lock opcodes). So a guest build
    issues zero remote lock requests and, by construction, zero remote unlock
    requests; FLUSH's `lock_owner` field is dead weight on the wire.
23. **The tools in question use flock or lock files, not `fcntl` POSIX locks.**
    cargo's lock is `flock(2)` (`libc::LOCK_SH/LOCK_EX`, rust-cargo 0.66.0
    `src/cargo/util/flock.rs:352-368, 387-397`) on `$CARGO_HOME/.package-cache` -
    flock, and not on the share. git's discipline is lock *files* (`foo.lock`
    created `O_CREAT|O_EXCL`, `lockfile.c:96-103, 121`), not fcntl. `rustc` has
    no lock on its inputs that I could find (it memory-maps sources; asserted, not
    audited). podman's storage locks are flock in the guest's own store, not
    `/workspace`. The honest caveat: I did not audit every tool in an
    arbitrary workload, and the classic POSIX-record-lock user that *could* appear
    under `/workspace` is sqlite (its default VFS uses `F_SETLK`) - but fact 19
    covers it: those locks are local, and close releases them locally.
24. **What FLUSH is *not* allowed to be used for**, per the kernel's own protocol
    documentation: not durability (`fsync` is `FUSE_FSYNC`, `file.c:550-580`), not
    close/release (that is `FUSE_RELEASE`), not "flush pending writes" - the flag's
    author writes that the current behaviour "seems over agressive and arbitrary
    and does not make a lot of sense when writeback caching is disabled", and
    libfuse spells out why: *"unlike fsync, the filesystem is not forced to flush
    pending writes"* and *"POSIX does not require close to wait for delayed I/O to
    complete"*. The only defensible use is returning close-time errors, which
    libfuse itself calls "non-portable".

## 5. Precedent: does anyone set it, and does the kernel recommend it?

25. **The kernel's reason for the flag** - commit `a390ccb316beb8ea594b8695d53926710ca454a3`,
    "fuse: add FOPEN_NOFLUSH", Amir Goldstein, 2021-10-24, v5.16, verbatim:
    *"Add flag returned by FUSE_OPEN and FUSE_CREATE requests to avoid flushing
    data cache on close. ... Most disk filesystems do not implement ->flush() at
    all ... FUSE flushes local write cache of ANY file, even non FMODE_WRITE and
    sends a 'flush' command to server (if server implements it). The FUSE
    implementation of ->flush() seems over agressive and arbitrary and does not
    make a lot of sense when writeback caching is disabled. Instead of deciding on
    another arbitrary implementation that makes sense, leave the choice of
    per-file flush behavior in the hands of the server."* The diff is exactly the
    three lines now at `:479-480` plus the flag definition.
26. **The uapi comment** (`include/uapi/linux/fuse.h:383`): *"FOPEN_NOFLUSH: don't
    flush data cache on close (unless FUSE_WRITEBACK_CACHE)"*.
27. **libfuse has the same switch as a per-open bit**, `struct fuse_file_info`:
    `noflush` - *"Can be filled in by open, to indicate that flush is not needed on
    close."* Note "**by open**": libfuse offers no way to set it without an open
    reply either, which is why the no-open default is the only lever there.
28. **libfuse's `flush` contract, verbatim** (`include/fuse_lowlevel.h:596-634`):
    *"This is called on each close() of the opened file. Since file descriptors can
    be duplicated (dup, dup2, fork), for one open call there may be many flush
    calls. Filesystems shouldn't assume that flush will always be called after some
    writes, or that if will be called at all. ... NOTE: the name of the method is
    misleading, since (unlike fsync) the filesystem is not forced to flush pending
    writes. One reason to flush data is if the filesystem wants to return write
    errors during close. However, such use is non-portable because POSIX does not
    require [close] to wait for delayed I/O to complete. If the filesystem supports
    file locking operations (setlk, getlk) it should remove all locks belonging to
    'fi->owner'. If this request is answered with an error code of ENOSYS, this is
    treated as success and future calls to flush() will succeed automatically
    without being send to the filesystem process."*
29. **libfuse's own dispatcher implements the ENOSYS interposer**: `do_flush`
    (`lib/fuse_lowlevel.c:1438-1453`) replies `ENOSYS` when the filesystem has no
    `flush` op - so every libfuse server without a flush handler (many) already runs
    with `no_flush` set. This is the strongest precedent available: dropping FLUSH
    is a supported, normal kernel state, not a hack.
30. **virtiofsd (rust-vmm) does NOT set it, and qemu's `contrib/virtiofsd` did not
    either**: `grep -c NOFLUSH` over upstream `src/passthrough/mod.rs` = 0; its
    `do_open` reply only ever emits `DIRECT_IO` (cache=never/metadata) or
    `KEEP_CACHE`/`CACHE_DIR` (cache=always) (`:1861-1867`), and its `flush`
    (`:2300-2324`) is the same dup/close as ours. No virtiofsd CLI option turns
    flush off. So there is **no production virtio-fs server shipping this**, which
    is a precedent against shipping it unmeasured - but also the reason nobody has
    measured the 21%.
31. **Kernel documentation**: this v7.2.10 `Documentation/filesystems/` tree has
    **no `fuse.rst`** at all (74 entries; the only virtio-fs doc is
    `virtiofs.rst`, which does not mention flush or NOFLUSH). There is therefore no
    kernel-doc recommendation either way; the uapi comment (fact 26) and the commit
    message (fact 25) are the whole documented intent.

## 6. Recommendation

**Ship it behind an opt-in launch flag, as the server-side `-ENOSYS` interposer,
not as the kernel default.** Concretely:

- **The lever**: in the fork's `flush` (`passthrough.rs:1896-1924`), when a new
  `Config::no_flush` (or a `Fs::set_no_flush` + `CANG_FS_NOFLUSH` env, mirroring
  `set_zero_message_open`) is on, keep doing the dup/close for the *first* flush
  and answer `Err(ENOSYS)`; every later flush is then handled in the guest
  (`file.c:490-492, 505-508`) with `write_inode_now` and `filemap_check_errors`
  still running. Zero kernel patch, zero libkrunfw re-pin, revertible per launch,
  and it is the shape virtiofsd's own trait default already has
  (`filesystem.rs:795`).
- **Why not the kernel line**: it is the same 21.21% of requests but it skips the
  guest's own writeback/error check as well (fact 1), it is inert on every mount
  that answers OPEN (fact 9, so it buys nothing in the non-ZMO arm), and it costs a
  firmware rebuild + fork tag + pin cascade to deliver a `1 << 5` bit that the
  server can already express as `-ENOSYS`.
- **Why gated rather than on by default**: the lost semantic (fact 2) is real, and
  FLUSH is currently the only close-time signal this server gets; a flag makes the
  default arm a live control in every A/B, and the histogram already reports FLUSH
  per launch.
- **Expected win, honestly labelled**: FLUSH is 21.21% of the request mix at 1 M
  requests (note 23 section 2) and each request is ~3 userspace exits (note 22
  section 1.3), on a workload where wall time tracked request count almost linearly
  (2.1x fewer requests -> 2.66x, note 23). A plausible band is therefore 10-20% of
  a rustc build, **but no A/B exists** (this note is read-only, no VM was booted),
  so it is a hypothesis with a priced mechanism, not a measurement.

**The exact semantic that is lost** (state it in the map and in the flag's
doc-comment): *with the flag on, `close(2)` in the guest no longer returns a
host-side error that the host filesystem defers to `close()`* (the dup/close probe
at `passthrough.rs:1913-1922`), and - for the kernel-flag variant only -
`MAP_SHARED`-dirtied pages of a file are written back by the guest's background
writeback rather than at close, and an accumulated `AS_EIO` is no longer reported
by `close`. Everything else FLUSH does is either a no-op here (writeback off, fact
6), already local (locks, facts 19-21), or dead code on the server (facts 15-18).

**How to detect a regression, concretely** (three arms, all in-guest, all with
`CANG_FS_STATS=1` so the histogram proves the lever fired):

| # | probe | pass | what it catches |
|---|---|---|---|
| A | the rustc/fullone arm from note 23 with the flag on: `FLUSH` count == 0 (or 1) in the teardown histogram, build succeeds, `git status` clean, note 23 section 5's correctness transcript unchanged | FLUSH 0, same results | the lever did nothing, or removed more than flush |
| B | guest writes N bytes with `write(2)`, then `close()` must return 0, then the **host** re-reads the file and compares | host sees all N bytes | a lost close-time writeback (the writeback-cache/mmap class) |
| C | guest mmaps a `/workspace` file `MAP_SHARED`, stores 4 KiB, `munmap`+`close`, then creates a marker file; the host polls the marker then reads the mapped file | full 4 KiB present. **This is the arm that separates the two variants**: it must pass with the ENOSYS interposer (`write_inode_now` still runs) and may legitimately fail right after close with the kernel `FOPEN_NOFLUSH` default | silently deferring dirty page cache past close |

A fourth probe is worth having but is not honestly buildable in this repo: an arm
that makes the *host* filesystem return an error at `close` (NFS/EIO). Local
filesystems report ENOSPC/EROFS at `write(2)` in the guest, so the lost
close-error path is nearly unreachable here - which is exactly why losing it is
cheap, and exactly why it cannot be regression-tested without a fault-injecting
host filesystem.

## 7. What I could not verify, and the limits of this note

- **No measurement.** No build, no VM, no A/B (task constraint). The 21.21% is
  note 23's ZMO-on histogram; the wall-clock band is extrapolated from note 23's
  "wall time tracks request count" line and could be wrong in either direction if
  per-FLUSH cost differs from the mean request (a synchronous round trip on
  `close(2)` is plausibly *above* mean, which would make the win larger, not
  smaller - unmeasured either way).
- **No lock census of a real guest.** Fact 22 is a mechanism argument: since no
  lock opcode ever appears in the histogram, FLUSH cannot be carrying lock
  release. A guest workload that *did* depend on remote locking would be a
  different story, and would require the fork to advertise `POSIX_LOCKS` first.
- **The close-time error path is untested and untestable with local host
  filesystems** (section 6, probe 4). If `/workspace` ever lives on NFS or another
  server that defers errors to close, this flag is the wrong default to turn on
  there.
- **The `MAP_SHARED` exposure** (fact 6) was read from the kernel, not measured; I
  did not confirm whether any tool in the bench workload writes through a shared
  mapping (rustc maps *inputs* read-only, which produces no dirty pages, but I did
  not audit every build script).
- **Line numbers are upstream `v7.2.10` + the two no-`fs/fuse` patch sets**, so
  they should be the guest's, but I verified the *patch sets* do not touch
  `fs/fuse` rather than diffing a built guest tree.
- **The `rustc`/`cargo`/`git`/`podman` locking statement (fact 23) is a reading of
  their sources, not of a running guest**: cargo's `flock` and git's lock files
  are cited, `rustc` is asserted from its read path, podman from its storage-lock
  design. Fact 22's histogram argument does not depend on any of them.
- **Precedent search is bounded**: I grepped virtiofsd and qemu's shipped
  virtiofsd for the flag, not the whole FUSE-server ecosystem (fuse-overlayfs,
  gcsfuse, ceph-fuse, ...). "No virtio-fs server ships it" is verified; "no FUSE
  server anywhere ships it" is not claimed.
