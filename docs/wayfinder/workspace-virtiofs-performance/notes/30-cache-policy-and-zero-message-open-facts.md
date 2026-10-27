---
label: wayfinder:research
title: Does CachePolicy (auto/always/never) still decide anything now that the fork answers OPEN with -ENOSYS, and what would switching the default to `always` buy?
status: open
blocked_by: []
claimed_by: pi child: cache-policy vs zero-message open (2026-10-27)
---

# 30 - Cache policy and zero-message open: the facts

Read-only. No builds, no VMs, no source edits. Written to answer the parent's
question about [libkrun#939](https://github.com/libkrun/libkrun/issues/939)
("`CachePolicy::Auto` makes every guest `open()` drop that file's guest page
cache"), given that this fork already ships zero-message open.

Read first: [13](13-cache-semantics-and-scoping.md) (the three modes, the scope
of each lever) and [23](23-zero-message-open-results.md) (the lever, its A/B, its
opcode histogram).

## Verdict

**Yes, it is the same mechanism, and `always` adds nothing measurable: the fork's
own zero-message-open path already delivers exactly what `CachePolicy::Always`
would set.** Concretely:

1. `Always` does one thing: put `FOPEN_KEEP_CACHE` in the per-open reply for a
   file (`FOPEN_CACHE_DIR` for a directory) - `passthrough.rs:899-905`. It sets
   nothing at INIT, and it does not disable `AUTO_INVAL_DATA`.
2. The guest kernel, when it answers `open()` itself because the server replied
   `-ENOSYS`, initialises `ff->open_flags` to **`FOPEN_KEEP_CACHE |
   FOPEN_CACHE_DIR`** before it ever considers sending a request
   (`fs/fuse/file.c:154-156`, "Default for no-open"). With no reply to overwrite
   that default, `fuse_open()`'s invalidation is skipped
   (`file.c:293-298`), and `fuse_dir_open()`'s is too (`fs/fuse/dir.c:1915-1916`).
   That is `Always`'s flag set, delivered by the kernel.
3. cang turns the lever on by default (`crates/cang-libkrun/src/lib.rs:68`,
   `CANG_FS_ZERO_MESSAGE_OPEN = true`), so **every open of every regular file and
   every opendir on the one virtio-fs connection already keeps its page cache and
   its readdir cache**. Issue 939's premise no longer describes this stack.
4. Under that path the server's `cache_policy` is reached by **one** site only:
   the `FUSE_CREATE` reply (`passthrough.rs:1471-1479`). `open` and `opendir`
   return `-ENOSYS` before `do_open`, so the mode is otherwise inert. `Always`
   would therefore change one reply on inodes that were just created and
   therefore have an empty page cache - a no-op.
5. What the issue proposes as the *server-side* half ("check mtime/size, keep the
   cache if unchanged") is already the kernel's job here: the connection
   negotiates `FUSE_AUTO_INVAL_DATA` (`server.rs:1018`, `inode.c:1318-1321`) and
   `fuse_change_attributes_i` compares mtime/size and invalidates only on a
   change (`inode.c:363-386`), on the 5 s attr timeout (`inode.c:1555-1558`).
   Note 13 §1.4's warning still holds: `always` is not "never invalidate", it is
   "don't invalidate **on open**".

Where `always` is still exactly right is the *other* arm: the one-line
`cache_policy: Always` is the server-side equivalent of the kernel's no-open
default, so it is what cang should set **if** zero-message open is ever turned
off (`CANG_FS_ZERO_MESSAGE_OPEN=0`, or a fork/guest-kernel pair that does not
negotiate `FUSE_NO_OPEN_SUPPORT`). It is not additive with the lever.

## Sources read

| what | where |
|---|---|
| the fork's virtio-fs server | `/home/dev/cang/cang/deps/libkrun` @ `d1c58a99` (`v2.0.0-cang.12`): `src/devices/src/virtio/fs/{linux/passthrough.rs,server.rs,device.rs,worker.rs,fuse.rs}`, `src/libkrun/src/api/device_builders.rs`, `include/libkrun.h`. **The map worktree's submodule (`/home/dev/cang/cang-vfio/deps/libkrun` @ `9e423e76`, `v2.0.0-cang.11`) has identical line numbers for every fs file cited here** - spot-checked `passthrough.rs:335/401/429/461/859/901/1280/1382/1415/1476`, `server.rs:144/635/1015/1092/1254`, `device.rs:94/188`, `device_builders.rs:703`. |
| cang | `crates/cang-libkrun/src/{api.rs,linked.rs,lib.rs}`, `crates/cang/src/runtime/vm/libkrun/launcher.rs` |
| guest kernel | **linux-7.2.10** - `deps/libkrunfw/Makefile:1` (`KERNEL_VERSION = linux-7.2.10`), which is what the pinned firmware carries (`nix/pins.nix:205-224`, `libkrunfwRelease.tag = v5.6.2-cang.4`; `git show v5.6.2-cang.4:Makefile` agrees). Fetched as plain text from `git.kernel.org/.../stable/linux.git/plain/<path>?h=v7.2.10`: `fs/fuse/file.c`, `fs/fuse/inode.c`, `fs/fuse/dir.c`, `fs/fuse/readdir.c`, `include/uapi/linux/fuse.h` |

Line numbers below are for those exact revisions. Note 13's fs citations are
**stale in one place and partly mis-attributed** - see §4.3.

## 1. What each mode actually does

### 1.1 The enum, verbatim (`passthrough.rs:331-351`)

```rust
331 /// The caching policy that the file system should report to the FUSE client. By default the FUSE
332 /// protocol uses close-to-open consistency. This means that any cached contents of the file are
333 /// invalidated the next time that file is opened.
334 #[derive(Default, Debug, Clone)]
335 pub enum CachePolicy {
336     /// The client should never cache file data and all I/O should be directly forwarded to the
337     /// server. This policy must be selected when file contents may change without the knowledge of
338     /// the FUSE client (i.e., the file system does not have exclusive access to the directory).
339     Never,
340
341     /// The client is free to choose when and how to cache file data. This is the default policy and
342     /// uses close-to-open consistency as described in the enum documentation.
343     #[default]
344     Auto,
345
346     /// The client should always cache file data. This means that the FUSE client will not
347     /// invalidate any cached data that was returned by the file system the last time the file was
348     /// opened. This policy should only be selected when the file system has exclusive access to the
349     /// directory.
350     Always,
351 }
```

`Config.cache_policy` is `passthrough.rs:399-401`; the `Default` impl is
`passthrough.rs:461-471` (`entry_timeout`/`attr_timeout` 5 s, `cache_policy:
Default::default()` i.e. `Auto`, `writeback: false`, `zero_message_open: false`,
`xattr: true`).

### 1.2 INIT: the policy is *not* consulted, and `AUTO_INVAL_DATA` is not policy-gated

`PassthroughFs::init` (`passthrough.rs:1097-1108`) builds its reply from
`DO_READDIRPLUS | READDIRPLUS_AUTO`, plus `WRITEBACK_CACHE` if the config asked
for it and the client offered it, plus `SUBMOUNTS`. **No branch reads
`cache_policy`.** `AUTO_INVAL_DATA` reaches the client from the server's
`supported` mask instead (`server.rs:1014-1026`):

```rust
1015        let mut supported = FsOptions::ASYNC_READ
1016            | FsOptions::PARALLEL_DIROPS
1017            | FsOptions::BIG_WRITES
1018            | FsOptions::AUTO_INVAL_DATA
...
1022            | FsOptions::ZERO_MESSAGE_OPEN
1023            | FsOptions::ZERO_MESSAGE_OPENDIR
```

`enabled = capable & (want | supported)` (`server.rs:1068`), so `AUTO_INVAL_DATA`
is negotiated under **all three** policies, and `always` does not weaken it.
`FUSE_EXPLICIT_INVAL_DATA` (the "only invalidate on explicit request" mode,
`fuse.h:424`, `inode.c:1320-1321`) is *not* in `supported`, so it is never
negotiated.

### 1.3 The per-object flags: `open`/`opendir` reply, and `create` reply

`do_open` (`passthrough.rs:859-910`) is the only place the mode becomes a reply
flag:

```rust
892        let mut opts = OpenOptions::empty();
893        match self.cfg.cache_policy {
894            // We only set the direct I/O option on files.
895            CachePolicy::Never => opts.set(
896                OpenOptions::DIRECT_IO,
897                flags & (libc::O_DIRECTORY as u32) == 0,
898            ),
899            CachePolicy::Always => {
900                if flags & (libc::O_DIRECTORY as u32) == 0 {
901                    opts |= OpenOptions::KEEP_CACHE;
902                } else {
903                    opts |= OpenOptions::CACHE_DIR;
904                }
905            }
906            _ => {}
907        };
```

`create` repeats the same match twice: on the zero-message path
(`passthrough.rs:1471-1479`) and on the normal path (`:1491-1498`).

| | `Never` | `Auto` (default) | `Always` |
|---|---|---|---|
| INIT flags from the policy | none | none | none |
| `open` file reply `open_flags` | `FOPEN_DIRECT_IO` | `0` | `FOPEN_KEEP_CACHE` |
| `opendir` reply `open_flags` | `0` | `0` | `FOPEN_CACHE_DIR` |
| `create` reply `open_flags` | `FOPEN_DIRECT_IO` | `0` | `FOPEN_KEEP_CACHE` |
| effect on the guest | direct I/O, `MAP_SHARED` impossible on this fork (no `FUSE_DIRECT_IO_ALLOW_MMAP`; note 13 §1.1) | kernel drops the inode's pages on every open (issue 939) | kernel keeps the pages across opens |

`FOPEN_KEEP_CACHE` is *exactly* "don't invalidate the data cache on open"
(`fuse.h:379`), and `FOPEN_CACHE_DIR` is "allow caching this directory"
(`fuse.h:381`). No other `FOPEN_*` is ever emitted by the server: `fuse.rs:52-70`
defines `DIRECT_IO`, `KEEP_CACHE`, `NONSEEKABLE`, `CACHE_DIR`, and a grep for
their use across `src/` returns only the four lines in `do_open` and the four in
`create` (the windows/macos personalities aside). `NONSEEKABLE`, `STREAM`,
`NOFLUSH`, `PARALLEL_DIRECT_WRITES`, `PASSTHROUGH` are never set.

## 2. How the device gets its config, and what cang can reach

`Fs::new` builds the `passthrough::Config` for every virtio-fs device and passes
only three fields - the issue's `..Default::default()` claim is accurate:

```rust
 86        let attr_timeout = if matches!(semantics, PermissionSemantics::LinuxSimplified) { ... } else { Duration::from_secs(5) };
 94        let fs_cfg = shared_dir.map(|root_dir| passthrough::Config {
 95            root_dir,
 96            semantics,
 97            attr_timeout,
 98            ..Default::default()
 99        });
```
(`device.rs:86-99`; the config is cloned into the worker inside `activate`
(`device.rs:280-287`) and turned into the `PassthroughFs` at
`worker.rs:380-416`, so any setter must run before activation.)

`Fs`'s setters are `set_exit_code` (`:150`), `set_worker_threads` (`:168`),
`set_zero_message_open` (`:188`), `set_opcode_stats` (`:200`) and the macOS
`set_map_sender`. **There is no cache-policy or writeback setter anywhere in the
fork**: `grep -rn cache_policy` over `src/` returns the field, its default, and
the three match sites, and `grep cache_policy include/libkrun.h` returns nothing.

`FsDevice` (`device_builders.rs:626-716`) exposes `new`, `new_read_only`,
`new_null`, `set_overlay`, `set_dax_window_size`, `set_worker_threads`,
`set_zero_message_open`, `set_opcode_stats`; the C API mirrors exactly those
(`include/libkrun.h:241-300`). cang's trait (`crates/cang-libkrun/src/api.rs:50-54`)
and its implementation (`linked.rs:317-326` for `fs_device_new`, `:328-365` for
the four setters) match that list, so **cang cannot set the cache policy at all
today**, not even to `always`; doing so needs the fork setter + regenerated header
+ one trait method (note 13 §3.3 priced it at ~30 fork lines + ~15 in
`cang-libkrun`). cang registers exactly one device and sets only the knobs it
has (`launcher.rs:317-355`):

```
317  let rootfs = setup("krun_fs_device_new",
319      self.api.fs_device_new(CANG_FS_ROOT_TAG, &config.task_rootfs, false))?;
323  ... fs_device_set_overlay(rootfs, overlay)
331  ... fs_device_set_worker_threads(rootfs, CANG_FS_WORKER_THREADS)
341  ... fs_device_set_zero_message_open(rootfs, zero_message_open)
351  ... fs_device_set_opcode_stats(rootfs, opcode_stats)
355  add_device(&mut self.api, devices, rootfs)?;
```

tag `"/dev/root"` (`crates/cang-libkrun/src/lib.rs:25`), lever default `true`
(`lib.rs:68`), override `CANG_FS_ZERO_MESSAGE_OPEN=0|1` (`lib.rs:72`), CLI
`--fs=zero-message-open:on|off` winning over the env (`launcher.rs:88-101`).

## 3. Zero-message open in this fork, as built

- **Advertised**: `ZERO_MESSAGE_OPEN`/`ZERO_MESSAGE_OPENDIR` are in `supported`
  unconditionally (`server.rs:1022-1023`), so the INIT reply echoes them whenever
  the guest offers them - which 7.2.10 always does (`inode.c:1462`,
  `include/uapi/linux/fuse.h:416`, `:423`). **The advertisement is inert on its
  own**: the kernel sets `fc->no_open`/`fc->no_opendir` only from a `-ENOSYS`
  reply (`file.c:169-178`); `inode.c` never tests `FUSE_NO_OPEN_SUPPORT` at all.
  So the win comes from the `-ENOSYS` answer, not from the flag.
- **The answer**: `open` returns `-ENOSYS` before touching `do_open`
  (`passthrough.rs:1389-1398`), `opendir` likewise
  (`passthrough.rs:1286-1292`). `CREATE` answers with *no* handle and the
  policy's flags (`:1465-1479`) - a handle returned there would never be released,
  because the kernel completes every `FUSE_RELEASE` locally once `fc->no_open` is
  set (`file.c:110-113`).
- **`FUSE_ATOMIC_O_TRUNC` leaves the supported set with the lever**
  (`server.rs:1032-1043`), so `O_TRUNC` becomes a `SETATTR` size 0 after the open.
- **What the kernel sends instead**: `ff->fh` stays 0 (`file.c:154`), so `READ`,
  `WRITE`, `READDIRPLUS`, `FLUSH`, `FSYNC`, `LSEEK` name only the inode; the
  server resolves `fh == 0` through the inode's lazily opened descriptor
  (`passthrough.rs:652-703`, `:714-732`; two slots, read and write, because a
  read must not force a writable descriptor). `GETATTR_FH`/`FATTR_FH` treat
  `fh == 0` as "no handle" (`server.rs:338`, `:370`), and `do_release` accepts a
  zero handle (`passthrough.rs:912-919`).
- **Does the server still return `FOPEN_KEEP_CACHE` under that path? No - it
  returns no open reply at all.** `cache_policy` is reached only by `CREATE`
  (`:1471-1479`), and with cang's `Auto` that reply carries `open_flags = 0`
  (`server.rs:1302`, `passthrough.rs:1476`).

## 4. The guest kernel: a kernel-answered open does **not** invalidate

### 4.1 The default that buys it (`fs/fuse/file.c:133-187`)

```c
133 struct fuse_file *fuse_file_open(struct fuse_mount *fm, u64 nodeid,
134 				 unsigned int open_flags, bool isdir)
...
139 	bool open = isdir ? !fc->no_opendir : !fc->no_open;
...
154 	ff->fh = 0;
155 	/* Default for no-open */
156 	ff->open_flags = FOPEN_KEEP_CACHE | (isdir ? FOPEN_CACHE_DIR : 0);
157 	if (open) {
...
162 		err = fuse_send_open(fm, nodeid, open_flags, opcode, outargp);
163 		if (!err) {
164 			ff->fh = outargp->fh;
165 			ff->open_flags = outargp->open_flags;
166 		} else if (err != -ENOSYS) {
167 			fuse_file_free(ff);
168 			return ERR_PTR(err);
169 		} else {
170 			if (isdir) {
171 				/* No release needed */
172 				kfree(ff->args);
173 				ff->args = NULL;
174 				fc->no_opendir = 1;
175 			} else {
176 				fc->no_open = 1;
177 			}
178 		}
179 	}
```

Three things follow, and they are the whole answer:

1. The `-ENOSYS` arm leaves `ff->open_flags` at the **no-open default**, which is
   `FOPEN_KEEP_CACHE | FOPEN_CACHE_DIR`. The server never gets to name flags.
2. That default is identical to what `CachePolicy::Always` would have put in a
   reply for the same object (`passthrough.rs:899-905`).
3. `fc->no_open` is set on the first `-ENOSYS` and is a property of the
   superblock's `fuse_conn`, so it covers every inode of the share from then on -
   including every later open, for any access mode (there is no `FMODE`/access-mode
   condition anywhere in the path).

### 4.2 Where the invalidation would have happened - and is skipped

`fuse_open` (`file.c:250-306`) calls `fuse_do_open` then `fuse_finish_open`, and
only then:

```c
293 	if (!err) {
294 		if (is_truncate)
295 			truncate_pagecache(inode, 0);
296 		else if (!(ff->open_flags & FOPEN_KEEP_CACHE))
297 			invalidate_inode_pages2(inode->i_mapping);
298 	}
```

With the no-open default, `FOPEN_KEEP_CACHE` is set, so line 296-297 never runs.
`fuse_finish_open` itself (`file.c:216-235`) contains **no** invalidation in
7.2.10 - it only does `fuse_file_io_open`, `stream_open`/`nonseekable_open` and
`fuse_link_write_file`. (Note 13 §1.2 attributes the `invalidate_inode_pages2`
to `fuse_finish_open` at `file.c:296-297`; the line numbers still land on it but
the enclosing function is `fuse_open`. The conclusion is unchanged.)

Directories get the same treatment, on the directory's own mapping:
`fuse_dir_open` (`dir.c:1892-1920`) ends with

```c
1915 		if (!(ff->open_flags & FOPEN_KEEP_CACHE))
1916 			invalidate_inode_pages2(inode->i_mapping);
```

so with the lever on, a repeated `opendir` neither drops the directory's readdir
cache nor loses `FOPEN_CACHE_DIR`. `FOPEN_CACHE_DIR` is what makes
`fuse_readdir()` consult the cache at all (`readdir.c:640-656`), populate it
(`:122-127`) and close it out (`:405-411`). That is note 23's
129 -> 4446 ops/s readdirplus result, mechanically.

### 4.3 What `FUSE_AUTO_INVAL_DATA` does, and why the issue's server-side proposal already exists

`fc->auto_inval_data` is set when the *reply* carries `FUSE_AUTO_INVAL_DATA`
(`inode.c:1318-1321`); the fork always offers it (`server.rs:1018`). The
machinery it enables, for a regular file with no writeback (`cache_mask == 0`,
`inode.c:304-312`):

```c
363 	if (!cache_mask && S_ISREG(inode->i_mode)) {
364 		bool inval = false;
365
366 		if (oldsize != attr->size) {
367 			truncate_pagecache(inode, attr->size);
368 			if (!fc->explicit_inval_data)
369 				inval = true;
370 		} else if (fc->auto_inval_data) {
...
380 			if (!timespec64_equal(&old_mtime, &new_mtime))
381 				inval = true;
382 		}
383
384 		if (inval)
385 			invalidate_inode_pages2(inode->i_mapping);
386 	}
```

Compare mtime/size, keep the cache if unchanged, drop it if not - exactly the
server-side half issue 939 proposes, implemented in the kernel and driven by the
*attr timeout*, not by open. It runs only when attributes are actually refetched:
`fuse_update_get_attr` (`dir.c:1531-1585`) goes to the server only when the
inode's `i_time` has expired or a mask is dirty (`:1549-1558`), i.e. every
`attr_timeout` (5 s here, `passthrough.rs:461-471`). Note 23 measured the
consequence of that timeout directly (host rewrite visible at 6 s, not at 1 s).

The same flag is what keeps the **directory** cache honest: at the start of a
scan, `readdir.c:518-523` forces an attribute refresh when `auto_inval_data` is
on, and `:543-551` resets the cached scan if the directory's mtime/iversion
moved. Without `AUTO_INVAL_DATA` the readdir cache would be unboundedly stale -
which is why `always` cannot be treated as "free readdir caching plus free page
retention": the two are the same flag pair and they depend on this kernel path.

### 4.4 `FUSE_CREATE`, the one place flags still come from the server

`fuse_create_open` sets `ff->open_flags = outopenp->open_flags`
(`dir.c:898`) - it does **not** use the no-open default - and then:

```c
921 	} else {
922 		if (fm->fc->atomic_o_trunc && trunc)
923 			truncate_pagecache(inode, 0);
924 		else if (!(ff->open_flags & FOPEN_KEEP_CACHE))
925 			invalidate_inode_pages2(inode->i_mapping);
926 	}
```

With `Auto` + zero-message open that invalidate *does* run, on the inode that was
just instantiated by this reply (`d_instantiate` at `:909`) and therefore has an
empty page cache. `Always` would set `KEEP_CACHE` here instead. Structurally
`CREATE` is only reached for a dentry that is not positive
(`fuse_atomic_open`, `dir.c:939-971`; a positive dentry takes
`finish_no_open` and the normal `fuse_open` path, `:950-953`), so no cached
pages can exist for that inode in the first place.

## 5. Conclusion: what is left paying, and what `always` would buy

| class | under cang today (zero-message open, `Auto`) | does `always` add anything? |
|---|---|---|
| regular file, any access mode (`O_RDONLY`/`O_WRONLY`/`O_RDWR`) | `KEEP_CACHE` by kernel default -> pages retained across opens | no - same flag, same effect |
| directory (`opendir`) | `KEEP_CACHE`+`CACHE_DIR` by default -> readdir cache retained, validated by the mtime check of §4.3 | no |
| `mmap`/`MAP_SHARED` mapped file | pages stay resident across open/close cycles | no |
| newly created file (`FUSE_CREATE`) | invalidate runs on a fresh, empty inode | one reply byte; no observable effect |
| first `OPEN`/`OPENDIR` of the connection | one round trip each, then `fc->no_open`/`no_opendir` are set; the `-ENOSYS` reply invalidates nothing | no |
| `O_TRUNC` open | truncation via `SETATTR` (`ATOMIC_O_TRUNC` withdrawn) -> `truncate_pagecache` in `fuse_setattr` (`dir.c:2170-2311`) | no |
| attribute refresh (any open or `stat` after 5 s) | `AUTO_INVAL_DATA` mtime/size comparison may drop pages (`inode.c:363-386`) | no - `always` does not touch it |
| guest `O_DIRECT` opens | kernel drops pages when swapping cache modes (`file.c:1684-1707`) | no - guest-requested |
| **lever off** (`CANG_FS_ZERO_MESSAGE_OPEN=0`) | every open/opendir invalidates; page cache and readdir cache lost per open; 2 extra round trips | **yes - that is the arm `always` was written for** |

So the precise answer to "would switching the default policy to `always` add
anything": **no, not while zero-message open is on.** The two are the same
mechanism reached from opposite ends - one puts `FOPEN_KEEP_CACHE` in a reply the
kernel reads, the other never sends the request so the kernel applies its own
`FOPEN_KEEP_CACHE` default. Turning both on is redundant, and `always` on this
fork is *strictly* the weaker of the two: it still pays the `OPEN`/`RELEASE`
round trips (16.4% of a build's requests, note 23 §2) that the lever deletes,
and it does not give `FOPEN_CACHE_DIR` on a directory reply without a matching
enable, whereas the no-open default gives both for free.

Two corollaries worth carrying into the map:

- **Issue 939 is not a cang bug and not a cang lever.** Its premise (every
  `open()` drops the page cache) is `CachePolicy::Auto` *without*
  `FUSE_NO_OPEN_SUPPORT`, i.e. the default of every other libkrun consumer and
  of upstream virtiofsd. For cang the same file is already retained; note 23 §2
  measured the consequence as `READ` falling 35.76% -> 3.26% of requests, which
  was the larger half of its 2.4x.
- **The remaining close-to-open cost is the attr timeout, not the open.** What
  bounds staleness today is `entry_timeout`/`attr_timeout` = 5 s
  (`passthrough.rs:461-471`, `device.rs:86-99`), and that is exactly what a
  "check mtime/size on open" server proposal would re-implement badly. If the map
  ever wants *less* staleness without losing the cache, the knob is a shorter
  `attr_timeout` (per-connection, fork-only, one line), not a cache policy - and
  if it wants *more* retention it already has it.

## 6. What this note does not settle

- No new measurement. The retention claim is note 23's arm matrix (README/§2:
  `READ` collapse, `readdirplus` 34x, 2.42x wall clock on re-measurement); this
  note only establishes *why*, from the kernel source, and that the two levers
  are the same mechanism.
- Whether cang should ever *ship* the lever off. It would be a regression on
  every number note 23 recorded, so the only reason would be a guest kernel or a
  fork that stops answering `-ENOSYS` - and in that world the right one-liner is
  `cache_policy: Always` in `Fs::new` (`device.rs:94-99`), not a new CLI knob.
- `CachePolicy::Never` is now doubly unusable: with the lever on it is never
  consulted for `open` at all, and with the lever off it makes `MAP_SHARED`
  impossible on this fork (no `FUSE_DIRECT_IO_ALLOW_MMAP`, note 13 §1.1).
