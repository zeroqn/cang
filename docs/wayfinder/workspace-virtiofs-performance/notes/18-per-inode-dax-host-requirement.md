---
label: wayfinder:research
title: Does virtio-fs per-inode DAX need the HOST filesystem to support DAX? (No - and what that changes in note 14 / ticket 17)
status: open
blocked_by: []
claimed_by: pi research child (2026-10-26, dax-host-requirement)
---


> **Floor correction (2026-10-26).** Any "no-FUSE floor" quoted in this note as
> 232 s was a **lever-off** measurement, not a no-FUSE one: `/` is itself the
> virtio-fs share (`/proc/mounts`: `/dev/root / virtiofs`), so the guest-local arm
> launched 2401 `rustc`/`ld` binaries off the same connection and paid
> OPEN+RELEASE for each (OPEN 87,871-89,374). With zero-message open the same
> tmpfs arm runs 111.0-164.9 s, and the true no-FUSE floor is ~131 s. See
> `MAP.md` ("The governor fact that reorders everything") and note 23's
> independent re-measurement for the numbers and the method.
# 18 - Per-inode DAX and the host filesystem: is the host-DAX gate real?

Follow-up to [note 14](14-per-inode-dax-viability.md) and to the closed
[ticket 17](../tickets/17-per-inode-dax-decision.md). bob asks: *"isn't virtiofsd
enabling DAX based on file size?"* - and the two documents above answer a
different question than the one that question implies. Note 14's kernel reading
is accurate; its **decision framing** is not, and the ticket inherited it.

Read-only: no source edits, no build, no VM. Everything below is either kernel /
daemon / spec source read at a named revision, or a host probe re-run this
session.

Sources (what I actually read):

| what | where |
|---|---|
| guest kernel | `v7.2` (torvalds) on `git.kernel.org`, `plain/` per file: `fs/fuse/{dax.c,inode.c,file.c,virtio_fs.c,fuse_i.h}`, `include/uapi/linux/fuse.h`, `Documentation/filesystems/dax.rst`; `v5.16`/`v5.17` `fuse.h`/`dax.c` for the merge point |
| merge history | `git.kernel.org/.../log/fs/fuse/dax.c?h=v7.2&qt=grep&q=inode+DAX` |
| virtio spec | OASIS `virtio-v1.2-csd01.html`, section 5.11.6.4 (+ .1/.2) |
| the fork | `deps/libkrun` @ `7160f48f` (`cang`): `fs/{server.rs,fuse.rs,linux/passthrough.rs}` |
| virtiofsd (Rust) | `gitlab.com/virtio-fs/virtiofsd`: `main` and `v1.10.0`/`v1.13.0` (`src/{server.rs,fuse.rs,passthrough/mod.rs}`), commit `87f67e84`, tags API |
| virtiofsd (C, QEMU) | `gitlab.com/qemu-project/qemu` `v7.2.0`: `tools/virtiofsd/{passthrough_ll.c,fuse_virtio.c,fuse_lowlevel.c}` |
| per-inode policy design | `lore.kernel.org`: Jeffle Xu, `[PATCH v8 0/3] virtiofsd: support per inode DAX` (2022-05-16) and `[PATCH v8 2/3]`; `dax.rst`; virtio-fs list threads `DAX support in virtiofsd` (2025-08/09) |
| this host | `findmnt`, `/sys/bus/dax/devices`, `/proc/modules`, `/dev/dax*` |

## Verdict

1. **The host filesystem is not part of the mechanism.** The guest decides
   `S_DAX` from four things, none of which is a block device or a host mount: the
   guest mount option, `fc->dax != NULL` (a window), `fc->inode_dax` (the server
   echoing `FUSE_HAS_INODE_DAX` in INIT), and `fuse_attr.flags & FUSE_ATTR_DAX`
   in the reply. That is the whole condition - `dax.c:1296-1316` below.
2. **The window's pages are the host's page cache**, which the server gets by
   `mmap(MAP_SHARED|MAP_FIXED)`-ing the host file into a shared memory region the
   VMM exposes as a virtio shm region. No kernel path and no fork path asks a
   block device or a host filesystem whether DAX is supported.
3. **So the host-DAX requirement belongs to exactly one policy, not to the
   feature.** `dax=attr`/xflag (enable DAX when the *host inode* carries
   `FS_XFLAG_DAX`) is one of two policies upstream's own patchset offered; the
   other, `dax=filesize`, reads `st_size >= 32 KiB` and is host-independent. Both
   were posted and neither was merged. bob's memory is right: **"virtiofsd
   enabling DAX based on file size" is a real, written-down policy**, with the
   threshold derived from guest page-descriptor overhead (512 x 64 B = 2^15).
4. **Note 14's conclusion (don't ship it) survives; its stated reason does not.**
   The thing that decides against per-inode DAX on cang is the *benefit*, and the
   benefit for a rustc-shaped build is approximately zero - not a host capability
   that the mechanism never consults. Two of note 14's sentences and three of
   ticket 17's carry the wrong reason, and they would mislead anyone who later
   moves cang to a DAX-capable host, or who re-opens this on a workload where DAX
   does pay: they would conclude the feature had become possible, when nothing
   about the feature was ever gated on it.
5. **Implementability, for the record:** per-inode DAX with a size or path policy
   is implementable in this fork *today*, on this host, with no host-side change:
   advertise `FUSE_HAS_INODE_DAX` when a window exists, add `ATTR_DAX`, set it in
   both the lookup and the getattr reply, allocate the window (note 05's expensive
   part, unchanged), and add `rootflags=dax=inode`. ~8 files, ~50 lines, plus the
   window.
6. **The measurement that decides it:** a same-boot A/B of one cold build on the
   same tree - (A) virtiofs today, (B) `rootflags=dax=always` with a window - with
   per-opcode FUSE counters and window-reclaim counters. Per-inode can only select
   a subset of what `dax=always` marks (plus avoid its slot pressure), so B bounds
   it from above whenever B does not itself regress on reclaim. See
   [section 5.3](#53-the-measurement-that-decides-it).



## 1. Guest kernel: exactly what sets `S_DAX` on an inode

### 1.1 The one function that decides

`fs/fuse/dax.c` @ v7.2, `fuse_should_enable_dax()` - the whole condition, verbatim:

```c
1296 | static bool fuse_should_enable_dax(struct inode *inode, unsigned int flags)
1297 | {
1298 | 	struct fuse_conn *fc = get_fuse_conn(inode);
1299 | 	enum fuse_dax_mode dax_mode = fc->dax_mode;
1300 | 
1301 | 	if (dax_mode == FUSE_DAX_NEVER)
1302 | 		return false;
1303 | 
1304 | 	/*
1305 | 	 * fc->dax may be NULL in 'inode' mode when filesystem device doesn't
1306 | 	 * support DAX, in which case it will silently fallback to 'never' mode.
1307 | 	 */
1308 | 	if (!fc->dax)
1309 | 		return false;
1310 | 
1311 | 	if (dax_mode == FUSE_DAX_ALWAYS)
1312 | 		return true;
1313 | 
1314 | 	/* dax_mode is FUSE_DAX_INODE* */
1315 | 	return fc->inode_dax && (flags & FUSE_ATTR_DAX);
1316 | }
```

and `fuse_dax_inode_init()`, which turns that verdict into `S_DAX`:

```c
1318 | void fuse_dax_inode_init(struct inode *inode, unsigned int flags)
1319 | {
1320 | 	if (!fuse_should_enable_dax(inode, flags))
1321 | 		return;
1322 | 
1323 | 	inode->i_flags |= S_DAX;
1324 | 	inode->i_data.a_ops = &fuse_dax_file_aops;
1325 | }
1326 | 
1327 | void fuse_dax_dontcache(struct inode *inode, unsigned int flags)
1328 | {
1329 | 	struct fuse_conn *fc = get_fuse_conn(inode);
1330 | 
1331 | 	if (fuse_is_inode_dax_mode(fc->dax_mode) &&
1332 | 	    ((bool) IS_DAX(inode) != (bool) (flags & FUSE_ATTR_DAX)))
1333 | 		d_mark_dontcache(inode);
1334 | }
```

Read literally, `fuse_should_enable_dax()` consults four inputs and nothing else:

| input | set where | needs the host? |
|---|---|---|
| `fc->dax_mode` (read into the local `dax_mode`) | mount option: `dax=never\|always\|inode` (`virtio_fs.c:104-108`), legacy `dax` = always | no |
| `fc->dax != NULL` (the window's `fuse_conn_dax`) | `fuse_dax_conn_alloc(fc, ctx->dax_mode, ctx->dax_dev)` (`inode.c:1750-1754`) with the *device's* dax device | no - it is a virtio shared-memory region, see §2 |
| `fc->inode_dax` | server echoed `FUSE_HAS_INODE_DAX` in `FUSE_INIT` (`inode.c:1358-1359`) | no - a server claim |
| `flags & FUSE_ATTR_DAX` | the `fuse_attr.flags` of the entry/attr reply the inode was instantiated from | no - whatever the server chose to say |

The mode enum and its predicate (`fs/fuse/fuse_i.h`):

```c
367 | enum fuse_dax_mode {
368 | 	FUSE_DAX_INODE_DEFAULT,	/* default */
369 | 	FUSE_DAX_ALWAYS,	/* "-o dax=always" */
370 | 	FUSE_DAX_NEVER,		/* "-o dax=never" */
371 | 	FUSE_DAX_INODE_USER,	/* "-o dax=inode" */
372 | };
373 | 
374 | static inline bool fuse_is_inode_dax_mode(enum fuse_dax_mode mode)
375 | {
376 | 	return mode == FUSE_DAX_INODE_DEFAULT || mode == FUSE_DAX_INODE_USER;
377 | }
```

Protocol words (`include/uapi/linux/fuse.h`) - `SETUPMAPPING` is 7.31,
per-inode DAX is 7.36, and `FUSE_ATTR_DAX` is a bit in the *reply's* attr flags:

```c
171 |  *  7.31
172 |  *  - add FUSE_WRITE_KILL_PRIV flag
173 |  *  - add FUSE_SETUPMAPPING and FUSE_REMOVEMAPPING
174 |  *  - add map_alignment to fuse_init_out, add FUSE_MAP_ALIGNMENT flag
...
191 |  *  7.36
192 |  *  - extend fuse_init_in with reserved fields, add FUSE_INIT_EXT init flag
193 |  *  - add flags2 to fuse_init_in and fuse_init_out
194 |  *  - add FUSE_SECURITY_CTX init flag
195 |  *  - add security context to create, mkdir, symlink, and mknod requests
196 |  *  - add FUSE_HAS_INODE_DAX, FUSE_ATTR_DAX
...
486 | #define FUSE_HAS_INODE_DAX	(1ULL << 33)
...
580 |  * FUSE_ATTR_DAX: Enable DAX for this file in per inode DAX mode
581 |  */
582 | #define FUSE_ATTR_SUBMOUNT      (1 << 0)
583 | #define FUSE_ATTR_DAX		(1 << 1)
```

### 1.2 Where the two negotiated half-truths come from

The kernel *offers* the capability from the mount option alone, and honours the
server's echo; it does not require the server to have proved anything:

```c
1466 | #ifdef CONFIG_FUSE_DAX
1467 | 	if (fm->fc->dax)
1468 | 		flags |= FUSE_MAP_ALIGNMENT;
1469 | 	if (fuse_is_inode_dax_mode(fm->fc->dax_mode))
1470 | 		flags |= FUSE_HAS_INODE_DAX;
1471 | #endif
```

```c
1353 | 			if (IS_ENABLED(CONFIG_FUSE_DAX)) {
1354 | 				if (flags & FUSE_MAP_ALIGNMENT &&
1355 | 				    !fuse_dax_check_alignment(fc, arg->map_alignment)) {
1356 | 					ok = false;
1357 | 				}
1358 | 				if (flags & FUSE_HAS_INODE_DAX)
1359 | 					fc->inode_dax = 1;
1360 | 			}
```

(A detail worth keeping from note 14 §1.3: `HAS_INODE_DAX` lives in `flags2`,
which the kernel parses only when the reply set `FUSE_INIT_EXT` - not when the
minor version is high enough. The fork already replies `INIT_EXT` at
`server.rs:898-908` while claiming minor 27, so advertising bit 33 needs no minor
bump.)

### 1.3 It is a creation-time property, applied once per inode

`S_DAX` is set while the inode is being built from the reply that named it:

```c
406 | static void fuse_init_inode(struct inode *inode, struct fuse_attr *attr,
407 | 			    struct fuse_conn *fc)
408 | {
409 | 	inode->i_mode = attr->mode & S_IFMT;
410 | 	inode->i_size = attr->size;
411 | 	inode_set_mtime(inode, attr->mtime, attr->mtimensec);
412 | 	inode_set_ctime(inode, attr->ctime, attr->ctimensec);
413 | 	if (S_ISREG(inode->i_mode)) {
414 | 		fuse_init_common(inode);
415 | 		fuse_init_file_inode(inode, attr->flags);
```

```c
3115 | void fuse_init_file_inode(struct inode *inode, unsigned int flags)
3116 | {
3117 | 	struct fuse_inode *fi = get_fuse_inode(inode);
3118 | 	struct fuse_conn *fc = get_fuse_conn(inode);
3119 | 
3120 | 	inode->i_fop = &fuse_file_operations;
3121 | 	inode->i_data.a_ops = &fuse_file_aops;
3122 | 	if (fc->writeback_cache)
3123 | 		mapping_set_writeback_may_deadlock_on_reclaim(&inode->i_data);
3124 | 
3125 | 	INIT_LIST_HEAD(&fi->write_files);
3126 | 	INIT_LIST_HEAD(&fi->queued_writes);
3127 | 	fi->writectr = 0;
3128 | 	fi->iocachectr = 0;
3129 | 	init_waitqueue_head(&fi->page_waitq);
3130 | 	init_waitqueue_head(&fi->direct_io_waitq);
3131 | 
3132 | 	if (IS_ENABLED(CONFIG_FUSE_DAX))
3133 | 		fuse_dax_inode_init(inode, flags);
3134 | }
```

and a later `GETATTR` can never turn it on or off - it can only notice the server
changed its mind and force the dentry to be dropped:

```c
388 | 	if (IS_ENABLED(CONFIG_FUSE_DAX))
389 | 		fuse_dax_dontcache(inode, attr->flags);
```

### 1.4 The thing that is *not* in the condition

There is no `fs_dax_get_by_bdev()`, no `bdev_dax_supported()`, no superblock or
mount lookup, no `STATX_ATTR_DAX` in any of the above. The only filesystems in the
kernel that gate `S_DAX` on a block device are the *local* ones (ext4:
`EXT4_FLAGS_BDEV_IS_DAX`), because there `S_DAX` means "this inode's pages live on
a DAX-capable device". On FUSE, `S_DAX` means "this inode's pages live in the
window", and the window is memory the VMM exported - so the check that matters is
`fc->dax != NULL`, which is a device property, not a filesystem one. That is the
whole confusion in one sentence.



## 2. `file.c`, `virtio_fs.c` and the fork: who is asked, and who is not

### 2.1 The window comes from the device, at probe time, unconditionally

`fs/fuse/virtio_fs.c` builds the DAX device from the virtio shared-memory region
before any mount option is looked at:

```c
1076 | static int virtio_fs_setup_dax(struct virtio_device *vdev, struct virtio_fs *fs)
1077 | {
1078 | 	struct dax_device *dax_dev __free(cleanup_dax) = NULL;
1079 | 	struct virtio_shm_region cache_reg;
1080 | 	struct dev_pagemap *pgmap;
1081 | 	bool have_cache;
1082 | 
1083 | 	if (!IS_ENABLED(CONFIG_FUSE_DAX))
1084 | 		return 0;
1085 | 
1086 | 	dax_dev = alloc_dax(fs, &virtio_fs_dax_ops);
1087 | 	if (IS_ERR(dax_dev)) {
1088 | 		int rc = PTR_ERR(dax_dev);
1089 | 		return rc == -EOPNOTSUPP ? 0 : rc;
1090 | 	}
1091 | 
1092 | 	/* Get cache region */
1093 | 	have_cache = virtio_get_shm_region(vdev, &cache_reg,
1094 | 					   (u8)VIRTIO_FS_SHMCAP_ID_CACHE);
1095 | 	if (!have_cache) {
1096 | 		dev_notice(&vdev->dev, "%s: No cache capability\n", __func__);
1097 | 		return 0;
1098 | 	}
1099 | 
1100 | 	if (!devm_request_mem_region(&vdev->dev, cache_reg.addr, cache_reg.len,
1101 | 				     dev_name(&vdev->dev))) {
1102 | 		dev_warn(&vdev->dev, "could not reserve region addr=0x%llx len=0x%llx\n",
1103 | 			 cache_reg.addr, cache_reg.len);
1104 | 		return -EBUSY;
1105 | 	}
1106 | 
1107 | 	dev_notice(&vdev->dev, "Cache len: 0x%llx @ 0x%llx\n", cache_reg.len,
1108 | 		   cache_reg.addr);
1109 | 
```

and the mount-time branch only errors for `always`:

```c
1596 | 	if (ctx->dax_mode != FUSE_DAX_NEVER) {
1597 | 		if (ctx->dax_mode == FUSE_DAX_ALWAYS && !fs->dax_dev) {
1598 | 			err = -EINVAL;
1599 | 			pr_err("virtio-fs: dax can't be enabled as filesystem"
1600 | 			       " device does not support it.\n");
1601 | 			goto err_free_fuse_devs;
1602 | 		}
1603 | 		ctx->dax_dev = fs->dax_dev;
1604 | 	}
```

So: `dax=inode` on a device with no window is a legal mount that silently means
`never` (`dax.c:1304-1309`), and `dax=always` without a window is `-EINVAL`. Note
the asymmetry - the *presence of the window* is a device fact, decided by the VMM,
long before any file is involved.

### 2.2 The mapping: one round trip per 2 MiB region, `fh = -1`

`fs/fuse/dax.c` @ v7.2:

```c
20 | #define FUSE_DAX_SHIFT	21
21 | #define FUSE_DAX_SZ	(1 << FUSE_DAX_SHIFT)
22 | #define FUSE_DAX_PAGES	(FUSE_DAX_SZ / PAGE_SIZE)
...
181 | static int fuse_setup_one_mapping(struct inode *inode, unsigned long start_idx,
182 | 				  struct fuse_dax_mapping *dmap, bool writable,
183 | 				  bool upgrade)
184 | {
185 | 	struct fuse_mount *fm = get_fuse_mount(inode);
186 | 	struct fuse_conn_dax *fcd = fm->fc->dax;
187 | 	struct fuse_inode *fi = get_fuse_inode(inode);
188 | 	struct fuse_setupmapping_in inarg;
189 | 	loff_t offset = start_idx << FUSE_DAX_SHIFT;
190 | 	FUSE_ARGS(args);
191 | 	ssize_t err;
192 | 
193 | 	WARN_ON(fcd->nr_free_ranges < 0);
194 | 
195 | 	/* Ask fuse daemon to setup mapping */
196 | 	memset(&inarg, 0, sizeof(inarg));
197 | 	inarg.foffset = offset;
198 | 	inarg.fh = -1;
199 | 	inarg.moffset = dmap->window_offset;
200 | 	inarg.len = FUSE_DAX_SZ;
201 | 	inarg.flags |= FUSE_SETUPMAPPING_FLAG_READ;
202 | 	if (writable)
203 | 		inarg.flags |= FUSE_SETUPMAPPING_FLAG_WRITE;
204 | 	args.opcode = FUSE_SETUPMAPPING;
205 | 	args.nodeid = fi->nodeid;
206 | 	args.in_numargs = 1;
207 | 	args.in_args[0].size = sizeof(inarg);
208 | 	args.in_args[0].value = &inarg;
209 | 	err = fuse_simple_request(fm, &args);
210 | 	if (err < 0)
211 | 		return err;
```

Note `inarg.fh = -1` at line 198: `FUSE_SETUPMAPPING` is addressed by **nodeid**,
not by an open handle. That is what lets the server open the file itself (the fork
does exactly that) and it is why per-inode DAX does not need an `open()` in the
guest's path.

### 2.3 The server side in this fork: open + `mmap`, and nothing else

Protocol gate and address (`deps/libkrun .../fs/server.rs` @ `7160f48f`):

```rust
150 |             x if (x == Opcode::SetupMapping as u32) && shm_region.is_some() => {
151 |                 let shm = shm_region.as_ref().unwrap();
152 |                 #[cfg(any(target_os = "linux", target_os = "windows"))]
153 |                 let shm_base_addr = shm.host_addr;
154 |                 #[cfg(target_os = "macos")]
155 |                 let shm_base_addr = shm.guest_addr;
156 |                 self.setupmapping(
157 |                     in_header,
158 |                     r,
159 |                     w,
160 |                     shm_base_addr,
161 |                     shm.size as u64,
162 |                     #[cfg(target_os = "macos")]
163 |                     map_sender,
164 |                 )
165 |             }
166 |             x if (x == Opcode::RemoveMapping as u32) && shm_region.is_some() => {
167 |                 let shm = shm_region.as_ref().unwrap();
168 |                 #[cfg(any(target_os = "linux", target_os = "windows"))]
169 |                 let shm_base_addr = shm.host_addr;
170 |                 #[cfg(target_os = "macos")]
```

The Linux passthrough implementation (`.../fs/linux/passthrough.rs`):

```rust
2045 |     fn setupmapping(
2046 |         &self,
2047 |         _ctx: Context,
2048 |         inode: Inode,
2049 |         _handle: Handle,
2050 |         foffset: u64,
2051 |         len: u64,
2052 |         flags: u64,
2053 |         moffset: u64,
2054 |         host_shm_base: u64,
2055 |         shm_size: u64,
2056 |     ) -> io::Result<()> {
2057 |         let open_flags = if (flags & fuse::SetupmappingFlags::WRITE.bits()) != 0 {
2058 |             libc::O_RDWR
2059 |         } else {
2060 |             libc::O_RDONLY
2061 |         };
2062 | 
2063 |         let prot_flags = if (flags & fuse::SetupmappingFlags::WRITE.bits()) != 0 {
2064 |             libc::PROT_READ | libc::PROT_WRITE
2065 |         } else {
2066 |             libc::PROT_READ
2067 |         };
2068 | 
2069 |         if (moffset + len) > shm_size {
2070 |             return Err(einval());
2071 |         }
2072 | 
2073 |         let addr = host_shm_base + moffset;
2074 | 
2075 |         debug!("setupmapping: ino {inode:?} addr={addr:x} len={len}");
2076 | 
2077 |         let file = self.open_inode(inode, open_flags)?;
2078 |         let fd = file.as_raw_fd();
2079 | 
2080 |         let ret = unsafe {
2081 |             libc::mmap(
2082 |                 addr as *mut libc::c_void,
2083 |                 len as usize,
2084 |                 prot_flags,
2085 |                 libc::MAP_SHARED | libc::MAP_FIXED,
2086 |                 fd,
2087 |                 foffset as libc::off_t,
2088 |             )
2089 |         };
2090 |         if std::ptr::eq(ret, libc::MAP_FAILED) {
2091 |             return Err(io::Error::last_os_error());
2092 |         }
2093 | 
2094 |         Ok(())
```

That is the entire "host requirement": `open_inode()` (an `openat`), then
`mmap(MAP_SHARED | MAP_FIXED)` of the host file's page cache into
`host_shm_base + moffset`. The fork's `removemapping` re-`mmap`s the range
`PROT_NONE`/anonymous (`:2097-2126`) - which is also how the KVM mmu-notifier
zaps the guest's second-level PTEs. **Nothing in this path inspects the host
filesystem, and it could not beneficially do so**: `MAP_SHARED` of a regular file
is page-cache sharing on every Linux filesystem, DAX-capable or not.

One more detail that matters for the *policy* discussion: the fork's `statx()`
helper does not even ask for the host inode's DAX attribute -

```rust
198 | fn statx(f: &File) -> io::Result<(libc::stat64, u64)> {
199 |     let mut stx = MaybeUninit::<libc::statx>::zeroed();
200 | 
201 |     // Safe because this is a constant value and a valid C string.
202 |     let pathname = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };
203 | 
204 |     // Safe because the kernel will only write data in `st` and we check the return
205 |     // value.
206 |     let res = unsafe {
207 |         libc::statx(
208 |             f.as_raw_fd(),
209 |             pathname.as_ptr(),
210 |             libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
211 |             libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
212 |             stx.as_mut_ptr(),
213 |         )
214 |     };



## 3. What the virtio-fs specification requires (and what it does not)

OASIS `virtio-v1.2-csd01`, section 5.11 (virtio-fs device). The DAX window is
normative prose, not FUSE protocol: the FUSE opcode semantics are the FUSE
protocol's, and the spec defines only the window, the mapping requests' effects,
and the device/driver MUSTs. Quoted in full, because the *absence* of any
statement about files is the answer to the question:

> ##### 5.11.6.4 Device Operation: DAX Window
>
> FUSE_READ and FUSE_WRITE requests transfer file contents between the driver-provided buffer and the device. In cases where data transfer is undesirable, the device can map file contents into the DAX window shared memory region. The driver then accesses file contents directly in device-owned memory without a data transfer.
>
> The DAX Window is an alternative mechanism for accessing file contents. FUSE_READ/FUSE_WRITE requests and DAX Window accesses are possible at the same time. Providing the DAX Window is optional for devices. Using the DAX Window is optional for drivers.
>
> Shared memory region ID 0 is called the DAX window. Drivers map this shared memory region with writeback caching as if it were regular RAM. The contents of the DAX window are undefined unless a mapping exists for that range.
>
> The driver maps a file range into the DAX window using the FUSE_SETUPMAPPING request. Alignment constraints for FUSE_SETUPMAPPING and FUSE_REMOVEMAPPING requests are communicated during FUSE_INIT negotiation.
>
> When a FUSE_SETUPMAPPING request perfectly overlaps a previous mapping, the previous mapping is replaced. When a mapping partially overlaps a previous mapping, the previous mapping is split into one or two smaller mappings. When a mapping is partially unmapped it is also split into one or two smaller mappings.
>
> Establishing new mappings or splitting existing mappings consumes resources. If the device runs out of resources the FUSE_SETUPMAPPING request fails until resources are available again following FUSE_REMOVEMAPPING.
>
> After FUSE_SETUPMAPPING has completed successfully the file range is accessible from the DAX window at the offset provided by the driver in the request. A mapping is removed using the FUSE_REMOVEMAPPING request.
>
> Data is only guaranteed to be persistent when a FUSE_FSYNC request is used by the device after having been made available by the driver following the write.
>
> ##### 5.11.6.4.1 Device Requirements: Device Operation: DAX Window
>
> The device MAY provide the DAX Window to memory-mapped access to file contents. If present, the DAX Window MUST be shared memory region ID 0.
>
> The device MUST support FUSE_READ and FUSE_WRITE requests regardless of whether the DAX Window is being used or not.
>
> The device MUST allow mappings that completely or partially overlap existing mappings within the DAX window.
>
> The device MUST reject mappings that would go beyond the end of the DAX window.
>
> ##### 5.11.6.4.2 Driver Requirements: Device Operation: DAX Window
>
> The driver SHOULD be prepared to find shared memory region ID 0 absent and fall back to FUSE_READ and FUSE_WRITE requests.
>
> The driver MAY use both FUSE_READ/FUSE_WRITE requests and the DAX Window to access file contents.
>
> The driver MUST NOT access DAX window areas that have not been mapped.

Compressing that into requirements:

| question | answer in the spec | where |
|---|---|---|
| must a device provide the window? | no - "MAY"; if present it MUST be shm region ID 0 | 5.11.6.4.1 |
| must the daemon still serve `FUSE_READ`/`FUSE_WRITE`? | yes, "regardless of whether the DAX Window is being used" | 5.11.6.4.1 |
| window bounds | device MUST "reject mappings that would go beyond the end of the DAX window" | 5.11.6.4.1 |
| overlaps | device MUST allow mappings that completely or partially overlap | 5.11.6.4.1 |
| alignment of foffset/moffset/len | "communicated during FUSE_INIT negotiation" (that is `map_alignment` / `FUSE_MAP_ALIGNMENT`) | 5.11.6.4 |
| must a mapping be removed before the file is released? | not stated; mappings are removed with `FUSE_REMOVEMAPPING`, and the driver "MUST NOT access DAX window areas that have not been mapped" | 5.11.6.4, 5.11.6.4.2 |
| anything about the mapping file's backing filesystem, size class, page cache, or per-file policy? | **nothing** | see sweep below |
| per-inode DAX / `FUSE_ATTR_DAX` / `FUSE_HAS_INODE_DAX` | **not mentioned at all** - the spec predates it (the window was added 2019-09-25, per-inode DAX is kernel 5.17) | sweep below |

Absence sweep over the fetched HTML-to-markdown text (536 221 bytes): lowercase
`dax` occurs **23** times, every one of them about the window, the mapping
requests, or the security consideration in 5.11.6.6; `ATTR_DAX` **0**,
`INODE_DAX` **0**, `per-inode`/`per inode` **0**. The spec's own device/driver
requirements leave the *file* out of scope entirely, which is consistent with the
kernel: the only thing that makes a window mapping valid is that the server
promised the range, not that the file lives anywhere special.

Conformance note for the fork, since it is cheap: the fork enforces the one MUST
the spec does state - `if (moffset + len) > shm_size { return Err(einval()) }`
(`passthrough.rs:2069-2071`, quoted in §2.3) - and it allows overlapping mappings
by construction (`MAP_FIXED` replaces).



## 4. The per-inode policy question: bob's file-size memory is a real design

### 4.1 The kernel half is merged (v5.17), and it delegates the decision

Per-file DAX on FUSE is Jeffle Xu's series, committed 2021-12-14, i.e. the v5.17
merge window. Verified by diffing the UAPI header: `v5.16` has no
`FUSE_HAS_INODE_DAX` and no `FUSE_ATTR_DAX`; `v5.17` has both. Commits from
`git.kernel.org`'s log for `fs/fuse/dax.c` (grep `inode DAX`):

| date | commit | subject |
|---|---|---|
| 2021-12-14 | `cecd491641c2` | `fuse: add fuse_should_enable_dax() helper` |
| 2021-12-14 | `780b1b959f9b` | `fuse: make DAX mount option a tri-state` |
| 2021-12-14 | `93a497b9ad69` | `fuse: enable per inode DAX` |
| 2021-12-14 | `2ee019fadcca` | `fuse: negotiate per inode DAX in FUSE_INIT` |
| 2021-12-14 | `c3cb6f935e32` | `fuse: mark inode DONT_CACHE when per inode DAX hint changes` |

and the doc paragraph that came with it says, explicitly, that the *server*
invents the algorithm (`Documentation/filesystems/dax.rst` @ v7.2):

```rst
171 | Enabling DAX on virtiofs
172 | ----------------------------
173 | The semantic of DAX on virtiofs is basically equal to that on ext4 and xfs,
174 | except that when '-o dax=inode' is specified, virtiofs client derives the hint
175 | whether DAX shall be enabled or not from virtiofs server through FUSE protocol,
176 | rather than the persistent `FS_XFLAG_DAX` flag. That is, whether DAX shall be
177 | enabled or not is completely determined by virtiofs server, while virtiofs
178 | server itself may deploy various algorithm making this decision, e.g. depending
179 | on the persistent `FS_XFLAG_DAX` flag on the host.
180 | 
181 | It is still supported to set or clear persistent `FS_XFLAG_DAX` flag inside
182 | guest, but it is not guaranteed that DAX will be enabled or disabled for
183 | corresponding file then. Users inside guest still need to call statx(2) and
184 | check the statx flag `STATX_ATTR_DAX` to see if DAX is enabled for this file.
```

Three things in that paragraph are the answer to the whole ticket:

- the client "derives the hint ... from virtiofs server through FUSE protocol,
  **rather than** the persistent `FS_XFLAG_DAX` flag";
- "whether DAX shall be enabled or not is **completely determined by virtiofs
  server**";
- the server "may deploy **various algorithm**", with host `FS_XFLAG_DAX` given as
  an *example* ("e.g."). A size rule is exactly such an algorithm.

### 4.2 The server half was posted twice, and is the origin of bob's memory

`[PATCH v8 0/3] virtiofsd: support per inode DAX`, Jeffle Xu, 2022-05-16
(`lore.kernel.org/all/20220516033906.58771-1-jefflexu@linux.alibaba.com/`),
against QEMU's C virtiofsd. The cover letter states the four policies:

> changes since v7:
> - merge patches implementing various DAX policies into one patch (patch
>   2); merge patches introducing "dax=" option and negotiation among
>   FUSE\_INIT phase into one patch (patch 3)
> - patch 1: restrict the newly introduced .ioctl() to only querying
>   and setting FS\_DAX\_FL/FS\_XFLAG\_DAX flag (Vivek Goyal [1])
> - patch 2: besides "inode" and "filesize" DAX policy, also add "none"
>   and "always" policy (Vivek Goyal [2])
> - patch3: return error if any operation fails when handling the fuse
>   request, rather than fallback to "dax=none" behavior (Vivek Goyal [3])
> - As disccussed in [2], a default threshold of 32KB in "filesize" policy
>   is fine for now. We can add new option "--dax-filesize-threshold" if
>   it's needed later.

and patch 2/3 (`.../20220516033906.58771-3-jefflexu@linux.alibaba.com/`) gives the
rule *and* its derivation - this is the closest thing to "upstream says virtiofsd
enables DAX by file size":

> As for "filesize" policy, it is inspired by the scenario where DAX
> window is run out and needs to be expanded to avoid the performance
> fluctuation triggered by DAX window recaliming. Considering from the
> perspective of memory overhead, it may not be wise to allocate DAX
> window for files with size smaller than a specific threshold. The
> threshold can be calculated from the hint that guest virtiofs manages
> DAX mapping at 2MB granularity.
>
> To maintain one DAX window chunk (2MB in size), 32KB (512 \* 64 bytes)
> memory footprint will be consumed for page descriptors inside guest.
> Thus it'd better disable DAX for those files smaller than 32KB, to
> reduce the demand for DAX window and thus avoid the unworthy memory
> overhead. Thus only enable DAX for those with the file size greater
> than 32 KB.

The code (patch hunk, from the mail, `tools/virtiofsd/passthrough_ll.c`):

```c
+/*
+ * One page descriptor (64 bytes in size) needs to be maintained for every page
+ * in the DAX window chunk, i.e., there is certain guest memory overhead when
+ * DAX is enabled. Thus disable DAX for those with file size smaller than this
+ * certain memory overhead if virtiofs is mounted in per inode DAX mode. In
+ * this case, the guest page cache will consume less memory than that when DAX
+ * is enabled.
+ */
+#define FUSE_DAX_SHIFT      21
+#define PAGE_DESC_SHIFT     6
+#define FUSE_INODE_DAX_SHIFT \
+    (FUSE_DAX_SHIFT - PAGE_SHIFT + PAGE_DESC_SHIFT)
+#define FUSE_INODE_DAX_THRESH  (1 << FUSE_INODE_DAX_SHIFT)
```

`1 << (21 - 12 + 6)` = `1 << 15` = **32768 B**, and the predicate is a pure
function of the reply the server is already building:

```c
+static int lo_should_enable_dax(struct lo_data *lo, struct lo_inode *inode,
+                                struct fuse_entry_param *e)
+{
+    if (lo->dax == INODE_DAX_NONE || !S_ISREG(e->attr.st_mode)) {
+        return 0;
+    }
+
+    if (lo->dax == INODE_DAX_ALWAYS) {
+        return 1;
+    }
+
+    if (lo->dax == INODE_DAX_INODE) {
+        int res, fd;
+        unsigned int attr;
+
+        fd = lo_inode_open(lo, inode, O_RDONLY);
+        if (fd == -1) {
+            return -1;
+        }
+
+        res = ioctl(fd, FS_IOC_GETFLAGS, &attr);
+        if (res == 0 && (attr & FS_DAX_FL)) {
+            res = 1;
+        }
+
+        close(fd);
+        return res;
+    }
+
+    if (lo->dax == INODE_DAX_FILESIZE) {
+        return e->attr.st_size > FUSE_INODE_DAX_THRESH;
+    }
+
+    return -1;
+}
```

and it is applied in lookup, with `FUSE_ATTR_DAX` set on the entry - i.e. exactly
the fork's missing piece (note 14 §2.1):

```c
+    res = lo_should_enable_dax(lo, inode, e);
+    if (res == -1) {
+        goto out_err;
+    } else if (res == 1) {
+        e->attr_flags |= FUSE_ATTR_DAX;
+    }
```

History: v7 (2021-12-09/10, `[PATCH v7 5/6] virtiofsd: implement xflag based dax
policy` and `[PATCH v7 6/6] virtiofsd: implement file size based dax policy`) and
v8 (2022-05-16) are the last postings; v8's cover even asks whether the series is
still wanted ("I know the C version virtiofsd has been marked as deprecated ...
I'm not sure what the community thinks of this patchset"). It was never merged,
and the daemon it patched was deleted from QEMU by v8.2.

### 4.3 What the daemons actually ship: nothing per-inode, in either one

| daemon | per-inode DAX | mount-wide DAX | evidence |
|---|---|---|---|
| Rust virtiofsd `main` (2026-10) | **no** | **no** - `FUSE_SETUPMAPPING`/`REMOVEMAPPING` reply `ENOSYS` | `server.rs:188-200` below; `ATTR_DAX`/`HAS_INODE_DAX` are unused constants in `fuse.rs:197-198`,`:452-455`,`:563-564` |
| Rust virtiofsd `v1.13.0` / `v1.10.0` | **no** in either release (no `HAS_INODE_DAX` in the INIT `supported` mask, `ATTR_DAX` never set) | yes, delegated to the VMM over vhost-user | `passthrough/mod.rs:1467-1491` below; init `supported` mask at `v1.10.0 server.rs:970-981` below contains no DAX bit |
| QEMU C virtiofsd (`v7.2.0`, last in-tree) | **no** | **no** | 0 occurrences of `dax`, `setupmapping`, `vu_req`, `FS_MAP` in `passthrough_ll.c` (121 610 B), `fuse_virtio.c` (32 667 B), `fuse_lowlevel.c` (74 564 B) |

Rust virtiofsd `main` today:

```rust
188 |     fn setupmapping<T>(
189 |         &self,
190 |         in_header: InHeader,
191 |         _r: Reader,
192 |         w: Writer,
193 |         _vu_req: Option<&mut T>,
194 |     ) -> Result<usize> {
195 |         reply_error(
196 |             io::Error::from_raw_os_error(libc::ENOSYS),
197 |             in_header.unique,
198 |             w,
199 |         )
200 |     }
```

Rust virtiofsd `v1.10.0` (before the removal) - the daemon never mapped anything;
it handed the file descriptor to the VMM through a vhost-user `FS_MAP` request:

```rust
1467 |     fn setupmapping<T: FsCacheReqHandler>(
1468 |         &self,
1469 |         _ctx: Context,
1470 |         inode: Inode,
1471 |         _handle: Handle,
1472 |         foffset: u64,
1473 |         len: u64,
1474 |         flags: u64,
1475 |         moffset: u64,
1476 |         vu_req: &mut T,
1477 |     ) -> io::Result<()> {
1478 |         debug!(
1479 |             "setupmapping: ino {:?} foffset {} len {} flags {} moffset {}",
1480 |             inode, foffset, len, flags, moffset
1481 |         );
1482 | 
1483 |         let open_flags = if (flags & fuse::SetupmappingFlags::WRITE.bits()) != 0 {
1484 |             libc::O_RDWR
1485 |         } else {
1486 |             libc::O_RDONLY
1487 |         };
1488 | 
1489 |         let file = self.open_inode(inode, open_flags)?;
1490 |         (*vu_req).map(foffset, moffset, len, flags, file.as_raw_fd())
1491 |     }
```

and its INIT reply never offered per-inode DAX:

```rust
970 |         // These fuse features are supported by this server by default.
971 |         let supported = FsOptions::ASYNC_READ
972 |             | FsOptions::PARALLEL_DIROPS
973 |             | FsOptions::BIG_WRITES
974 |             | FsOptions::AUTO_INVAL_DATA
975 |             | FsOptions::ASYNC_DIO
976 |             | FsOptions::HAS_IOCTL_DIR
977 |             | FsOptions::ATOMIC_O_TRUNC
978 |             | FsOptions::MAX_PAGES
979 |             | FsOptions::SUBMOUNTS
980 |             | FsOptions::INIT_EXT
981 |             | FsOptions::CREATE_SUPP_GROUP;
```

The removal is explicit in the log (`87f67e84`, 2024-11-08, "Upgrade vhost
dependencies"):

> Support for `FS_*` requests has been removed from the vhost crate in its
> commit 569ef077facafe21f16c0e65ff826b26d3c9a0b3 ("vhost_user: Remove
> support for FS_* requests"), so we have to drop the fs_cache_req_handler
> module that depends on and would handle those messages, and all code
> relating to setupmapping/removemapping.

and the maintainers' own 2025 thread (`DAX support in virtiofsd`, Jeremy Bongio /
Hanna Czenczek, 2025-08-19..09-09 on `virtio-fs@lists.linux.dev`) confirms the
reason it was dropped had nothing to do with host filesystems - it was that
vhost-user has no `FS_*` messages, plus two unresolved edge cases (injecting
`SIGBUS` past EOF, and a backing store that can change size under a live mapping).

So the honest summary of the state of the art: **the kernel mechanism has existed
since 5.17, the policy design was posted twice (v7 2021-12, v8 2022-05) and merged nowhere, and no
daemon (Rust or C) has ever advertised `FUSE_HAS_INODE_DAX`.** A policy in cang's
fork would be the first shipping implementation of a five-year-old kernel
feature - which is what note 14 already said, and it remains true.



## 5. Verdict for cang

### 5.1 It is implementable here, today, with no host change

The fork work is the same four edits note 14 §2.1 already sized (~8 files, ~50
lines), plus the window that note 05 §2 costs. Nothing about the policy needs the
host: a size rule reads `st_size` from the `stat64` the passthrough already has;
a path/subtree rule reads the relative path it already has; the xflag rule is the
only one that would need `STATX_ATTR_DAX` added to the mask at
`passthrough.rs:211` - and it is the one rule that yields nothing on this machine.

Two genuine implementation warnings, both inherited from earlier notes and both
unaffected by anything in this note:

- **`GETATTR` must carry the flag too.** `Entry.attr_flags` is rendered by the
  entry reply, but `FileSystem::getattr` returns `(stat64, Duration)`, so
  `Attr::with_flags(st, 0)` sends flags `0` on every `GETATTR`. A fork that sets
  `ATTR_DAX` only at lookup hands the kernel a permanent "hint changed" signal and
  `d_mark_dontcache()`s the inodes it just marked (note 14 §2.1 item 3,
  `dax.c:1327-1334` above).
- **The window is unpaid, not free.** Every DAX mapping is a 2 MiB slot in a
  region whose geometry can brick the boot (note 05 §2.4) and whose exhaustion is
  an uninterruptible wait with no read fallback (note 05 §3.4). Per-inode DAX
  buys *scope*, not a cheaper mechanism.

### 5.2 The honest expected win for a rustc build: ~0, and the reason is not the host

The measured shape (note 15, MAP): host 104 s, guest-local `/dev/shm` 232 s,
guest-virtiofs 299-311 s, single `fs_worker` thread at 12.3% of one core. Against
that:

- **The addressable time is first-touch read latency only.** A page-cache hit
  under FUSE costs nothing today, and a DAX hit costs nothing tomorrow; DAX only
  changes the *cold* path, where it trades two pipelined ~1 MiB `FUSE_READ`s per
  2 MiB for one synchronous `SETUPMAPPING` plus a guest-side memcpy. That is a
  wash at best on read-once data, and strictly worse in overlap.
- **The reuse case is the only winner, and the page cache already wins it.** DAX
  helps a file touched repeatedly *while its dmap is live* (the linker mapping
  hundreds of MiB of `.o`, rustc re-reading a `.rlib`). The guest page cache
  gives the same reuse for free after the first read - demonstrated by the
  `/dev/shm` arm, where the whole tree is retained in an 18 GiB guest.
- **Writes are untouched.** Extending writes bypass DAX by design
  (`fuse_dax_direct_write`), and `target/` is mostly created fresh, so the write
  half of a build cannot improve (note 05 §3.1).
- **The ceiling is small.** Against a 299 s build the whole FUSE penalty is 67-79 s
  and only the read share of it is DAX-addressable; note 15 already priced the
  worker-pool lever at ~3-5 s on the same 12% measurement. A lever that halves
  read round trips *and* removes readahead overlap on the cold path is not
  obviously positive at all.

So: expected win in the low single-digit seconds on a 299 s build, plausibly
negative; the mechanism's fixed costs (a boot-brick geometry, an uninterruptible
stall mode) are paid in full regardless. The verdict is "don't", and it is the
same verdict note 14 and ticket 17 reached - by a different route, which matters
because their route says "impossible here", and this one says "possible, not
worth it". Those two sentences behave very differently the next time the host, the
guest size, or the workload changes.

### 5.3 The measurement that decides it

Per-inode DAX is a *subset* of mount-wide DAX, so the deciding experiment is the
cheaper one that needs no policy at all: `dax=always` does not require
`FUSE_HAS_INODE_DAX` or `ATTR_DAX` (`dax.c:1311-1312` returns true before the
per-inode test), only a window and `rootflags=dax=always` - i.e. exactly note 05's
minimal change.

**Arm design** (one boot, alternating arms, per rule "never run a VM arm in the
session shell"): cold build of the same tree, same guest, same image;

| arm | root mount | what it isolates |
|---|---|---|
| A | virtiofs, no dax | today |
| B | virtiofs + window, `rootflags=dax=always` | the whole DAX mechanism, mount-wide |
| C (only if B regresses on reclaim) | fork with a size or path policy, `rootflags=dax=inode` | whether *scoping* recovers B |

Record, per arm, in the guest evidence directory: wall time; per-opcode FUSE
counts (`FUSE_READ` bytes, `FUSE_WRITE`, `SETUPMAPPING`/`REMOVEMAPPING` - the
fork's `debug!("setupmapping: ino ...")` at `passthrough.rs:2075` is already the
counter, run the fs worker at debug level); window pressure (mapping high-water
vs the slot count, from the same log); and a page-cache hit proxy (guest
`/proc/<pid>/stat` major/minor faults or `read_bytes`).

**Decision rules, written before the run:**

- *B materially faster* (> ~3% of wall time, i.e. >9 s on 299 s) → DAX pays, and
  the per-inode policy is the way to capture it without marking `execve`'d
  binaries and every 4 KiB metadata file (note 05 §3.2/§3.3). Implement it.
- *B ≈ A* **and** mapping count stays well under the slot count (no reclaim
  churn) → **per-inode DAX is dead**: a policy can only mark fewer inodes than
  `always`, and `always` already showed no win. Close it on benefit, permanently.
- *B slower because of reclaim churn* (mappings far above slots, `REMOVEMAPPING`
  traffic) → run C; the size/path policy is the hypothesis that scoping removes the
  pressure and reveals the reuse win.

**The cheap pre-filter, which can kill it without spending a window at all:**
per-opcode FUSE accounting in one cold build - `FUSE_READ` bytes versus distinct
file bytes read. If those are close (each byte crosses virtiofs ~once), there is no
reuse for DAX to exploit and the lever is dead before any VMM change; if
`FUSE_READ` bytes are a large multiple of the distinct bytes, reuse is real and
the window experiment is worth the boot risk. This is the same instrument note 14
§8 named as "the one measurement that would reopen it", and it is the *first* one
to run, not the last.

### 5.4 What this note does not settle

Nothing was measured. The arms above are specified, not run: this note is
read-only (no source edits, build, or VM), so the win remains an argument from
note 15's 12.3% worker measurement plus the mechanism. If bob wants the question
closed empirically rather than by argument, arm A/B is the run to do, and it is
cheaper than implementing the thing it judges.



## 6. Which claims need correcting

The verdicts in note 14 and ticket 17 stand (don't ship per-inode DAX). The
*reasons* do not, and the difference is load-bearing: a capability verdict says
"re-check when the capability arrives", and the capability they name is one the
mechanism never asked for.

### 6.1 `notes/14-per-inode-dax-viability.md`

| where | the claim (verbatim) | what is wrong |
|---|---|---|
| §Verdict item 1 | "**The upstream per-inode policy is dead here on capability grounds, flatly.** 'Let virtiofs decide per inode' as upstream designed it means: the server answers `FUSE_ATTR_DAX` from the **host file's** `STATX_ATTR_DAX` flag." | "as upstream designed it" is only the *xflag* policy. Upstream also designed - and posted in the same series - `dax=filesize` (`st_size > 32 KiB`), which is a pure function of the reply's own size and needs nothing from the host. The sentence should read: "one of the two posted policies is empty here; the other is implementable". |
| §Verdict item 1, last sentence | "So the `dax=attr` policy enables DAX on **zero** inodes here." | True, and it *only* speaks about `dax=attr` - but it sits under a bolded sentence about "the upstream per-inode policy", which reads as the whole feature. |
| §8 first bullet | "**The deciding capability check:** `statx(STATX_ATTR_DAX)` on a `/workspace` file on the host. It is 0, permanently, on this machine (§5.2) - so 'let virtiofs decide per inode' as designed upstream decides 'no' for every file, and any other policy is cang inventing one." | Demote it: it is the deciding check for a *host-xflag* policy, not for per-inode DAX. The mechanism consults no host capability at all (`dax.c:1296-1316`); the kernel doc says the server "may deploy various algorithm" (`dax.rst:176-179`); the file-size rule is not "cang inventing one" - it is the other half of the posted patchset. |
| §5 heading + §5.2 close | "The host side: what the host filesystem must support, and whether it can" / "That is the check that settles the upstream-shaped version of the question." | The host must support `MAP_SHARED` of a regular file, i.e. any Linux filesystem; there is no DAX-specific host requirement in the mechanism. "Upstream-shaped" should be "xflag-shaped" - §4 of the same note already lists `filesize`, so the note contradicts itself. To its credit §5.1 says exactly this ("the mechanism does not need host DAX - plainly, the *policy* does"); §8 then promotes capability back to "deciding". |
| §3 | "**Rust virtiofsd** ... **no DAX of any kind.**" | Historically wrong, though right about today: through v1.13.x it implemented `SETUPMAPPING`/`REMOVEMAPPING` by forwarding to the vhost-user front end (`passthrough/mod.rs:1467-1491`), and it was dropped on 2024-11-08 (`87f67e84`) because rust-vmm's vhost removed `FS_*` requests. What it *never* did - in any release - is advertise `HAS_INODE_DAX` or set `ATTR_DAX` (§4.3 here). |
| §3 close | "a per-inode policy in cang's fork would be the first shipping implementation of a six-year-old kernel feature" | Still true (five years old now). No change needed; §4.3 confirms it against both daemons. |

Its kernel-side reading needs **no** correction: `fuse_should_enable_dax()` at
`dax.c:1296-1316`, `FUSE_DAX_INODE*` at `fuse_i.h:367-376`, `S_DAX` at inode
instantiation via `file.c:3115-3134`/`inode.c:406-415`, `dontcache` at
`dax.c:1327-1334`, `fuse.h:486`/`:583`, and the `dax=inode`-without-a-window silent
fallback at `virtio_fs.c:1596-1604` all match v7.2 exactly as note 14 reports.

### 6.2 `tickets/17-per-inode-dax-decision.md` (closed)

| where | the claim (verbatim) | correction |
|---|---|---|
| Question, premise 2 | "the server replies `FUSE_ATTR_DAX` on the entry/attr of a chosen inode, **which only makes sense if the host file is on a filesystem that supports DAX**" | Delete the clause. The reply is meaningful whenever the server has a window and advertised `HAS_INODE_DAX`; the file's filesystem is irrelevant (§1, §2). |
| Resolution, bullet 1 | "So even a perfect server-side policy would enable **zero** inodes." | False for a size or path policy; true only for the `FS_XFLAG_DAX` policy. Correct to: "so the host-xflag policy would enable zero inodes; the file-size policy would mark most of a cargo tree". |
| Resolution, bullet 2 | "it needs the window anyway ... `dax=inode` degrades silently to `never`" | Correct and worth keeping - this is the one real cost, and it is unchanged. |
| Resolution, bullet 4 | "**The decisive check is one command:** `statx(STATX_ATTR_DAX)` on a `/workspace` file returns 0 here, permanently." | Not decisive (§6.1); it settles only the xflag policy. |
| "**What would reopen it**" | "a DAX-capable host filesystem (ext4 with `-O dax` on a pmem/brd-with-dax device, xfs on a dax device)" | Wrong lever: a DAX-capable host would only make the *xflag policy* non-empty, and note 14's own §5.1 says that capability "would not be needed by the mechanism anyway". What reopens the question is a benefit result - the A/B in §5.3 - or a workload whose working set no longer fits the guest page cache. |
| Resolution, penultimate bullet | "virtiofsd has no per-inode DAX either (both the Rust and the retired C daemon carry `ATTR_DAX`/`HAS_INODE_DAX` as unused constants)" | Half wrong: the Rust daemon carries both constants unused (and used to carry the mapping plumbing); QEMU's C virtiofsd carries **neither** - zero occurrences of any `dax` identifier in `tools/virtiofsd/*` at `v7.2.0`. |
| Resolution, last paragraph | "The fork is one small patch away ... - but there is nothing on this host to point it at." | The last clause is the error. What the fork would point DAX at is the **window** (a VMM-exported shm region), and the policy decides which inodes get it. Nothing on the host needs to change. |

**Disposition unchanged:** closed, don't ship. The reason to quote from now on is
§5.2 (benefit), not host capability.

### 6.3 One-line answer to bob

Yes - "virtiofsd decides DAX by file size" is a real design (Jeffle Xu's
`dax=filesize`, threshold 32 KiB = the 512 x 64 B of `struct page` a 2 MiB window
chunk costs the guest, v7 2021-12 / v8 2022-05, never merged). It needs nothing
from the host filesystem; per-inode DAX never did. cang doesn't ship it because a
rustc build has almost no reuse for it to win, not because the host can't host it.



## 7. Verification log

### 7.1 Fetch ledger (every source read this session)

| source | bytes/chars | how |
|---|---|---|
| `fs/fuse/dax.c` @ `v7.2` | 36 071 | `git.kernel.org/.../plain/fs/fuse/dax.c?h=v7.2` |
| `fs/fuse/inode.c` @ `v7.2` | 53 875 | same, `inode.c` |
| `fs/fuse/file.c` @ `v7.2` | 80 240 | same, `file.c` |
| `fs/fuse/virtio_fs.c` @ `v7.2` | 45 644 | same, `virtio_fs.c` |
| `fs/fuse/fuse_i.h` @ `v7.2` | 34 972 | same, `fuse_i.h` |
| `include/uapi/linux/fuse.h` @ `v7.2` | 31 930 | same, `include/uapi/linux/fuse.h` |
| `Documentation/filesystems/dax.rst` @ `v7.2` | 11 405 | same, `Documentation/filesystems/dax.rst` |
| `include/uapi/linux/fuse.h` @ `v5.16` / `v5.17` | 23 196 / - | merge-point check: `HAS_INODE_DAX` absent at v5.16, present at v5.17 |
| kernel log `fs/fuse/dax.c`, `qt=grep&q=inode+DAX` | 1 351 | commit ids/dates in §4.1 |
| OASIS `virtio-v1.2-csd01.html` | 536 221 | §5.11.6.4 quoted in §3 |
| `deps/libkrun` `fs/server.rs` | 55 760 | read from `/home/dev/cang/cang/deps/libkrun` @ `7160f48f` |
| `deps/libkrun` `fs/linux/passthrough.rs` | 78 558 | same |
| virtiofsd `main` `src/server.rs` / `src/fuse.rs` | 61 613 / 40 550 | `gitlab.com/virtio-fs/virtiofsd/-/raw/main/...` |
| virtiofsd `v1.10.0` `src/passthrough/mod.rs` / `src/server.rs` | 88 029 / 60 792 | `-/raw/v1.10.0/...` |
| virtiofsd commit `87f67e84` | commit JSON | `gitlab.com/api/v4/projects/virtio-fs%2Fvirtiofsd/repository/commits/87f67e84...` |
| QEMU `v7.2.0` `tools/virtiofsd/{passthrough_ll,fuse_virtio,fuse_lowlevel}.c` | 121 610 / 32 667 / 74 564 | `gitlab.com/qemu-project/qemu/-/raw/v7.2.0/...` |
| lore: `[PATCH v8 0/3] virtiofsd: support per inode DAX` | 6 257 | `lore.kernel.org/all/20220516033906.58771-1-jefflexu@linux.alibaba.com/` |
| lore: `[PATCH v8 2/3] virtiofsd: implement DAX policies ...` | 9 648 | `.../20220516033906.58771-3-jefflexu@linux.alibaba.com/` |
| lore: `DAX support in virtiofsd` thread (2025-08-19 .. 09-09) | 4 498 / 6 697 / 4 035 | `lore.kernel.org/all/CAOvQCn5H3X...`, `9783ed13-...@redhat.com`, `CAOvQCn7fjO=...` |

### 7.2 Counts relied on

- OASIS spec text: `dax` (case-insensitive) **23**, `ATTR_DAX` **0**,
  `INODE_DAX` **0**, `per-inode`/`per inode` **0**.
- QEMU `v7.2.0` `tools/virtiofsd`: `dax`/`setupmapping`/`vu_req`/`FS_MAP` **0** in
  all three files.
- Rust virtiofsd `main`: `dax` in `src/passthrough/mod.rs` **0**,
  `setupmapping` in `src/server.rs` **3** (all in the `ENOSYS` stub and its
  dispatch arms); `ATTR_DAX`/`HAS_INODE_DAX` present in `src/fuse.rs` **only** as
  definitions/bitflags entries.
- Rust virtiofsd `v1.10.0` `src/server.rs` init `supported` mask: no DAX bit
  (`:970-981`).
- the fork: `setupmapping` in `linux/passthrough.rs` = one implementation
  (`:2045-2095`); `statx(2)` mask at `:211` = `STATX_BASIC_STATS | STATX_MNT_ID`,
  i.e. `STATX_ATTR_DAX` is not even requested.
- host probes re-run this session: `findmnt -T /home/dev/cang/cang` →
  `/dev/sdb2[/home] ext4 rw,nosuid,nodev,noatime`; `/sys/bus/dax/devices` empty;
  no `pmem`/`nvdimm`/`dax` module in `/proc/modules` (118 modules); no
  `/dev/dax*`, no `/dev/pmem*`. (The `statx(STATX_ATTR_DAX)` reading that note 14
  calls "the decisive check" is implied by this - no dax device means ext4 never
  sets `EXT4_FLAGS_BDEV_IS_DAX`, so no inode is `S_DAX` - and is *moot* for the
  mechanism either way: §1.4.)

### 7.3 Quote integrity

Every `NNN | text` line in this note was generated programmatically from the
fetched source, not retyped, and re-checked by a script that extracts each
contiguous numbered run from this file and asserts it matches the named source
byte for byte at that line number (all runs verified; the spec and patch quotes
are prose/hunk text and are not line-numbered by construction).
