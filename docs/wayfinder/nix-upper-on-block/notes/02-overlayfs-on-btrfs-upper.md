---
label: wayfinder:note
ticket: 02-overlayfs-btrfs-upper
---

# A btrfs upper for the guest `/nix` overlay: does the pinned kernel take it?

**Verdict: yes, unconditionally — and the btrfs upper is *less* constrained than
the virtio-fs upper it replaces.** Overlayfs's only fatal "upper fs missing
required features" branch is gated on the upper being a *remote* filesystem
(`fs/overlayfs/super.c:759-769`), and btrfs is local, so every requirement it
cannot meet would be a warning, not a mount failure — and in this config btrfs
meets all of them. What btrfs forces instead is a **placement** rule: upper and
work must be one mount AND must not share a superblock with the lower, because
overlayfs's same-filesystem shortcut trusts inode numbers to be unique inside a
superblock and btrfs subvolumes are separate inode spaces. That is exactly the
recorded `257`/`257` wrong-object bug, and `xino=on` is not a fix there (the
kernel explicitly ignores it in that case, `super.c:1144-1147`). Two
independent btrfs filesystems — the proposed shape — are structurally safe with
`xino` off, which is also the default in this config. The prices: no reflink
copy-up across the two filesystems (`-EXDEV`, `fs/remap_range.c:384-385`), no
`trusted.overlay.*` if you keep `userxattr` (optional, and not free to change on
an existing upper), and `nodatacow` must not be used.

## Provenance of every citation

- **Kernel = linux 7.3.1, as this repo builds it.**
  `deps/libkrunfw/tarballs/linux-7.3.1.tar.xz`
  (sha256 `fa595aad93734f829c963c2a90ef7c3673224fd7fdae7b49406e397cde2de231`)
  is the tarball `MakefileLto:11` names and `MakefileLto:58-63` unpacks
  (`KERNEL_VERSION = linux-7.3.1`, `MakefileLto:1`). Every `fs/...:N`
  citation below is a line number in a member of that tarball.
- **Hardened delta.** `deps/libkrunfw/tarballs/linux-hardened-v7.3.1-hardened1.patch`
  (sha256 `c79c8364dfd3e6a4268f1dfc6e9b220c94776d6aace0a6d61dce135dcbfaead8`)
  has 82 `diff --git` paths, **0** of them under `fs/btrfs` and **2** under
  `fs/overlayfs`: `fs/overlayfs/Kconfig` (adds `OVERLAY_FS_UNPRIVILEGED`) and
  `fs/overlayfs/super.c`, whose only hunk is at `@@ -1574,7 +1574,11 @@`
  (`.fs_flags = FS_USERNS_MOUNT | FS_ALLOW_IDMAP` behind the new config). Every
  overlayfs/btrfs line cited here is below 1574, so it is vanilla 7.3.1.
- **Submodule revision.** `deps/libkrunfw` HEAD
  `4f628a4dcb0416a74cc46fc9a4b93716c1373d66` (`git describe` →
  `cang-4f628a4dcb04`), "kernel: rebase onto linux-7.3.1 + linux-hardened
  v7.3.1-hardened1".
- **Config =** `deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto`, the file
  `MakefileLto:24` builds the x86_64 asset with.
- **Repo =** `/home/dev/cang/cang` at HEAD `cf1b480`.
- **Web check (file-level, no line anchors).**
  `https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/fs/overlayfs/super.c?h=v7.3.1`
  resolves and is the same file (41287 bytes). A plain-URL fetch carries a
  one-line provenance prefix, so its displayed numbering is `tarball line + 1`;
  the tarball numbers are what is cited here.

## 1. The requirement list, probe by probe

`ovl_make_workdir()` is `fs/overlayfs/super.c:663-803`; it is called from
`ovl_get_workdir()` (`super.c:805-837`) after the upper is set up
(`ovl_get_upper`, `super.c:500-559`, called at `super.c:1435`).

**Structural preconditions, checked before the workdir is touched**

1. **Upperdir must be on a writable mount** — `__mnt_is_readonly()` at
   `super.c:508-512`, error text `"upper fs is r/o, try multi-lower layers
   mount"`, `-EINVAL`. btrfs: satisfied by any `rw` mount; a `ro` mount of the
   upper image fails the mount here and nothing else.
2. **Upperdir must not be a "real-file" filesystem** (`DCACHE_OP_REAL`, i.e.
   FUSE native-passthrough style) — `params.c:331-334`,
   `"filesystem on %s not supported as upperdir"`. Not applicable to btrfs.
   This is a second reason the current FUSE upper is the constrained one.
3. **Workdir must be on the same mount as upperdir, and a separate subtree** —
   `super.c:811-819` (`upperpath->mnt != workpath->mnt` → `"workdir and
   upperdir must reside under the same mount"`, `-EINVAL`; then
   `ovl_workdir_ok()`, `super.c:454-461`). btrfs: one mount containing
   `<mnt>/upper` and `<mnt>/work` satisfies this; two *bind* mounts of the two
   directories would not (bind mounts are distinct `struct mount`), and neither
   would nesting one inside the other.
4. **Upperdir must not be in use as another overlay's upper/work** —
   `ovl_inuse_trylock()` + `ovl_report_in_use()` (`super.c:487-498,548-554`):
   `-EBUSY` with `index=on`, a warning with `index=off`. Relevant on a host that
   re-mounts the same image in two guests.
5. **The inodes for the trap need `alloc_inode`/`destroy_inode`** —
   `ovl_setup_trap()` (`super.c:463-479,685-687`); `-ELOOP` prints
   `"conflicting %s path"`.

**Inside `ovl_make_workdir()`, in order**

6. **Write access actually obtainable** — `mnt_want_write(mnt)`, `super.c:674-676`.
7. **A `work` directory must be creatable** — `ovl_workdir_create(ofs,
   OVL_WORKDIR_NAME, false)` at `super.c:678` (implementation
   `super.c:320-395`). Two details that matter operationally:
   - it removes inherited POSIX-ACL xattrs from the new workdir and treats
     `-EOPNOTSUPP` as success (`super.c:356-375`) — **this is the ACL answer in
     one place** (see §4);
   - on failure it does **not** fail the mount: `pr_warn("failed to create
     directory %s/%s (errno: %i); mounting read-only")` and `NULL`
     (`super.c:389-394`), and `ovl_fill_super()` then sets `SB_RDONLY`
     (`super.c:1453-1454`). A guest `/nix` overlay with an unusable workdir on
     btrfs would come up **silently read-only**, not with a mount error.
8. **`d_type` must come back from readdir** — `ovl_check_d_type_supported()`
   (`super.c:695-701`; probe at `readdir.c:1137-1172`: iterate, ignore `.`/`..`,
   success if any entry is not `DT_UNKNOWN`). If unsupported it is a
   **warning** only (`"upper fs needs to support d_type."`, `super.c:700-701`),
   and whiteouts would become visible. btrfs fills the readdir type from the
   on-disk dir-item flags: `fs/btrfs/inode.c:6392`
   (`ftype = btrfs_dir_flags_to_ftype(...)`), `inode.c:6402`
   (`put_unaligned(fs_ftype_to_dtype(ftype), &entry->type)`),
   `fs/btrfs/accessors.h:524-528`, with `static_assert(BTRFS_FT_* == FT_*)` at
   `inode.c:6070-6077`. **Satisfied.**
9. **`O_TMPFILE`** — probed with `ovl_do_tmpfile()`; failure is a warning
   (`"upper fs does not support tmpfile."`, `super.c:703-709`) and copy-up falls
   back to a temp file. btrfs implements it: `fs/btrfs/inode.c:9413`
   (`btrfs_tmpfile`), `inode.c:10768` (`.tmpfile = btrfs_tmpfile`).
   **Satisfied** (the fast path is available).
10. **`RENAME_WHITEOUT`** — the one real capability probe:
    `ovl_check_rename_whiteout()` (`super.c:565-620`, called `super.c:712-719`)
    creates a temp regular file in the workdir, renames it with
    `rd.flags = RENAME_WHITEOUT` (`super.c:583-593`), and verifies the result is
    a whiteout (`ovl_upper_is_whiteout`, `super.c:607-612`); `-EINVAL` from the
    filesystem is decoded as "not supported" (`super.c:595-598`). btrfs accepts
    the flag (`fs/btrfs/inode.c:8863`, `flags & ~(RENAME_NOREPLACE |
    RENAME_EXCHANGE | RENAME_WHITEOUT)` → `-EINVAL`) and creates the whiteout
    inode itself (`inode.c:8825-8835`). **Satisfied** → no warning, and
    whiteout-creating renames (unlink a lower-only store path) work.
11. **The overlay xattr namespace must be writable** — `ovl_setxattr(workdir,
    OVL_XATTR_OPAQUE, "0", 1)` (`super.c:724-757`). On failure: `ofs->noxattr =
    true` plus a cascade of downgrades (`redirect_dir`, `metacopy`, `index`,
    `uuid`, and `xino=auto`→`xino=off`), and for `-EPERM` the hint
    `"try mounting with 'userxattr' option"` (`super.c:752-753`). btrfs handles
    `trusted.`, `user.`, `security.` and `btrfs.` namespaces —
    `fs/btrfs/xattr.c:458-487` (`btrfs_trusted_xattr_handler`,
    `btrfs_user_xattr_handler`, …); `trusted.*` then needs `CAP_SYS_ADMIN`
    (`fs/xattr.c:146-150`), which guest root in the initial user namespace has.
    **Satisfied** for either namespace.
12. **The fatal branch, and why it does not apply.** `super.c:759-769`:
    `if (ovl_dentry_remote(ofs->workdir) && (!d_type || !rename_whiteout ||
    ofs->noxattr)) { pr_err("upper fs missing required features.\n"); err =
    -EINVAL; }`. `ovl_dentry_remote()` is
    `dentry->d_flags & (DCACHE_OP_REVALIDATE | DCACHE_OP_WEAK_REVALIDATE)`
    (`fs/overlayfs/util.c:162-167`) — i.e. a networked/`d_revalidate` upper,
    which is what a virtio-fs/FUSE upper is and what btrfs is not. **A btrfs
    upper can never reach this error.**
13. **File-handle decoding** — `ovl_can_decode_fh()` (`super.c:783-793`, and for
    each lower at `super.c:427-449`). btrfs has export ops
    (`fs/btrfs/export.c:301-307`). This only affects `index`/`nfs_export` and
    sets `ofs->xino_mode = -1` when the fs does not use 32-bit inode numbers
    (`super.c:447-449,791-793`) — see §3.
14. **Overlapping layers** — `ovl_check_overlapping_layers()` (`super.c:1269-1300`,
    called `super.c:1483`): upper/work/index must not be inside a lower layer.
    Two separate btrfs filesystems cannot overlap; a subvolume of the lower's
    own filesystem can — see §3.

**Not probed at mount — runtime-only requirements.** Case-sensitive counts
inside `ovl_make_workdir()` (`super.c:663-803`): `RENAME_EXCHANGE` **0**,
`FICLONE` **0**, `O_DIRECT` **0**, `FS_IOC` **0**, `O_TMPFILE` **1** (the
comment). These are exercised only when used:

- **`RENAME_EXCHANGE`** — overlayfs uses it for its own whiteout install
  (`dir.c:136,433,562`) and passes it through on an upper-upper rename
  (`dir.c:1142-1170`); btrfs dispatches it (`fs/btrfs/inode.c:8866-8868`).
  Required for exchange, so a filesystem without it would fail those renames,
  not the mount.
- **`FICLONE`/`copy_file_range`** — `ovl_remap_file_range()` (`file.c:605-630`)
  and `ovl_copyfile()` (`file.c:527-595`) forward to
  `vfs_clone_file_range()`, which returns **`-EXDEV` when the two files are on
  different superblocks** (`fs/remap_range.c:384-385`). btrfs implements it
  (`fs/btrfs/reflink.c:926`, `.remap_file_range` at `fs/btrfs/file.c:3835`).
- **Copy-up's own clone attempt** — `ovl_copy_up_file()` tries
  `vfs_clone_file_range()` "within the same fs" and otherwise byte-copies
  (`copy_up.c:283-286`). With lower and upper on **two different btrfs
  filesystems this is always `-EXDEV`** — see the traps in §5/§7.
- **`O_DIRECT`** — `ovl_change_flags()` rejects `O_DIRECT` on the overlay file
  unless the *real* file has `FMODE_CAN_ODIRECT` (`file.c:63-90`,
  `-EINVAL`); the real-file comment is at `inode.c:779`. btrfs has direct I/O
  (`fs/btrfs/direct-io.c:878` `btrfs_direct_write`, reached from
  `fs/btrfs/file.c:1427`).
- **`FS_IOC_GETFLAGS`/`FS_IOC_SETFLAGS`** — forwarded to the real inode
  (`inode.c:632-634`); btrfs implements fileattr (`inode.c:10769-10770`,
  `.fileattr_get = btrfs_fileattr_get`; `ioctl.c:273`).
- **Sparse files** — copy-up skips holes if the *lower* supports
  `SEEK_DATA` (`copy_up.c:295-340`; `ovl_llseek` hands seeks to the real file,
  `file.c:261-281`). btrfs does (`fs/btrfs/file.c:3756` `btrfs_file_llseek`,
  `3464-3548`), so holes are preserved rather than zero-filled.

## 2. `userxattr` vs `trusted.overlay.*`

- The namespace is a compile-time-per-mount table:
  `OVL_XATTR_TRUSTED_PREFIX`/`OVL_XATTR_USER_PREFIX`
  (`fs/overlayfs/overlayfs.h:30-39`) indexed by `ofs->config.userxattr`
  (`overlayfs.h:187-190`), table at `util.c:876-887`
  (`OVL_XATTR_TAB_ENTRY` expands `trusted.overlay.X` vs `user.overlay.X`,
  `util.c:871-874`). `ovl_is_private_xattr()` — which is what keeps overlay
  metadata out of copy-up and out of `listxattr` for users — is namespace-aware
  (`xattrs.c:31-34`).
- **What cang passes today**: both existing overlays pass `userxattr` —
  guest `crates/cang-guest-init/src/guest_init/components/nix/root.rs:440-447`
  (`"lowerdir={},upperdir={},workdir={},userxattr"`, used for the block lower at
  `root.rs:422-437`) and host
  `crates/cang/src/runtime/session/nix_overlay.rs:888-895`. The recorded reason
  is only right for the host one: the host overlay is mounted in a rootless
  Buildah namespace, while the guest's reason is the *virtio-fs* upper —
  `root.rs` tests, "with `userxattr` (the upper is a virtio-fs share, so
  overlayfs cannot use trusted xattrs there)".
- **For a guest-local btrfs upper, root could use `trusted.overlay.*`**:
  the btrfs handler is there (`fs/btrfs/xattr.c:464-468`) and the only gate is
  `CAP_SYS_ADMIN` on `trusted.*` (`fs/xattr.c:146-150`), which guest root has.
  What each choice costs:
  - `userxattr` (no change): `user.*` xattrs are writable by any user that can
    write the file, so overlay metadata on a shared store is forgeable by
    whoever can write the upper; upstream itself warns that "careful attention
    must be given to privileges needed to change the `user.overlay.redirect`
    xattr" (`Documentation/filesystems/overlayfs.rst:460-466`). In a
    single-tenant guest whose `/nix` upper is written by root and `nix-daemon`,
    that is theoretical. It is also **free to adopt**: nothing about the
    existing upper's contents changes.
  - `trusted.overlay.*`: metadata invisible and unwritable to non-root. **Not
    free on an existing upper**: the two namespaces are different xattr names,
    so a generation copied from a `userxattr` upper and remounted without
    `userxattr` loses every `user.overlay.opaque`/`origin`/`impure` marker —
    directories stop being opaque and whiteout-adjacent metadata is ignored
    (silently wrong, not an error). Switching is a migration: mount once with
    `userxattr`, let copy-up/`metacopy`-free reads repopulate, or rewrite the
    xattrs explicitly. See ticket 06 for the one-time migration this map already
    plans.
  - Verdict for this ticket: **keep `userxattr`** (it is what both existing
    mounts already pass and what makes the guest path independent of the
    guest's credential model). Revisit `trusted.` only as a deliberate,
    separate hardening step with its own migration.

## 3. xino and inode identity — the part that decides the layout

**Is `xino=on` accepted when `CONFIG_OVERLAY_FS_XINO_AUTO` is not set?** Yes.
`xino` is a plain enum mount option
(`fsparam_enum("xino", Opt_xino, ovl_parameter_xino)`, `params.c:173`;
values `off|auto|on` at `params.c:90-95`; parsed at `params.c:680-681`). The
config option only changes the *default*:
`ovl_xino_auto_def = IS_ENABLED(CONFIG_OVERLAY_FS_XINO_AUTO)` (`params.c:26-29`)
→ in this config the default is `OVL_XINO_OFF` (`params.c:102-105`, applied at
`params.c:826`). `deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto:2901` has
`# CONFIG_OVERLAY_FS_XINO_AUTO is not set`, so **today's guest mounts run
`xino=off`** (nothing on their command lines either).

**What it does to `st_ino`/`st_dev`.** With xino enabled and layers on
different filesystems, overlayfs reserves the top `ilog2(numfs-1)+2` bits of
`st_ino` for the layer id and reports the *overlay's* `st_dev` for everything
(`super.c:1135-1165`, `inode.c:103-136`). The width comes from the **number of
underlying filesystems, not the size of any filesystem**: for two layers,
`ilog2(2-1)+2 = 2` bits, leaving 62 bits of underlying inode number
(`super.c:1150-1160`); an inode that overflows the low 62 bits only produces a
rate-limited warning and falls back to the non-xino identity
(`inode.c:128-135`, `readdir.c:518-535`). The "exhausts inode numbers on 32-bit
`st_ino`" worry is about **32-bit userspace**; a 32-bit *kernel* simply disables
xino (`super.c:1414-1421`, `"xino not supported on 32bit kernel"`). The ticket's
"scaling is filesystem-size dependent" is not what the code says.

**Is it needed when lower and upper are two independent btrfs filesystems?**
No. With `xino=off` and non-same-fs layers, `ovl_get_fsid()` hands every
distinct superblock its own anonymous block device
(`fs/overlayfs/super.c:947-992`, `get_anon_bdev()` at `:981-988`), and
`ovl_map_dev_ino()` uses it as `st_dev` for non-directories
(`inode.c:151-159`), while directories always take the overlay's own `st_dev`
plus an overlay-allocated inode number (`inode.c:139-150`,
`ovl_next_ino()`/`ovl_map_ino()` at `inode.c:842-888`). So the pairs are:

| object | `st_dev` | `st_ino` |
|---|---|---|
| lower-only file | pseudo-dev of the lower sb | lower inode number |
| copy-up'd file (lower `nlink == 1`) | pseudo-dev of the lower sb (fsid = lower) | lower inode number, stable across copy-up (`inode.c:196-238`, with `ovl_hash_bylower()` at `inode.c:1170-1198` and `ovl_get_inode()` at `inode.c:1209-1281`) |
| upper-only file, or a broken lower hardlink | pseudo-dev of the upper sb (fsid 0) | upper inode number |
| any directory | the overlay's `st_dev` | overlay-generated (`inode.c:881-887`) |

Every row lands in a different `(st_dev, st_ino)` space, so **equal underlying
inode numbers do not collide** — which is why the ticket has to distinguish the
two failure modes. What `xino=on` would add is uniformity (`st_dev` identical for
files and directories, so `du -x`, `find -xdev`, `cp -a` device comparisons and
`d_ino == st_ino` behave like a normal filesystem; `overlayfs.rst:54-77`), at the
cost of the two high bits.

**Where the `257`/`257` bug actually came from — and why xino cannot fix it.**
`ovl_fill_super()` takes a shortcut when every layer is on **one** superblock
(`ofs->numfs - !ovl_upper_mnt(ofs) == 1`): it sets `xino_mode = 0`, reports the
overlay's `st_dev` for *everything*, and uses the real inode numbers
(`super.c:1144-1147`, `inode.c:110-117`, `ovl_same_fs()` =
`xino_mode == 0`, `overlayfs.h:684-688`). That shortcut assumes inode numbers are
unique inside a superblock. **btrfs subvolumes are separate inode spaces**: each
btrfs root allocates objectids from its own counter starting at
`BTRFS_FIRST_FREE_OBJECTID` = 256 (`fs/btrfs/disk-io.c:5075-5077,5083-5100`;
`include/uapi/linux/btrfs_tree.h:121`), and that objectid becomes `i_ino`
(`fs/btrfs/inode.c:6682-6685`, `btrfs_inode.h:379-382`), while
`Documentation/filesystems/btrfs.rst:17` describes subvolumes as "separate
internal filesystem roots". Two subvolumes of one btrfs therefore hand out
`257, 258, …` twice — and if they reach the same overlay through one
superblock, the shortcut is wrong. That is exactly what is recorded in
`tools/nix-generation-smoke/README.md:36-40` ("with the image's nix tree and the
workspace upper in two subvolumes, both number their inodes from 257 under one
synthesized `st_dev`, and an unverified inode cache answers a lookup with the
wrong object") and in `docs/build.md:344-355` (same statement, plus the
virtio-fs server's `(st_ino, st_dev, mnt_id)` cache as the thing that misfires).
`tools/nix-generation-smoke/README.md` mentions `xino` **0** times and `st_dev`
**1** time: that record is about a same-superblock overlay, not about a missing
xino.

Two consequences for this map:

- **Two independent btrfs filesystems (the proposal) are the safe shape.** Two
  distinct `btrfs_fs_devices` produce two superblocks
  (`fs/btrfs/super.c:1857-1863` decides sharing by `fs_info->fs_devices ==
  p->fs_devices`), hence `numfs = 2`, hence the pseudo-dev path above.
- **Never put the upper in a subvolume of the filesystem that holds the
  lower.** `btrfs_fc_test_super()` shares a superblock across mounts of the same
  btrfs regardless of `subvol=`, so two subvolumes of one image are *one*
  superblock to overlayfs → the shortcut fires → the `257` collision. And in
  that configuration `xino=on` is refused in so many words:
  `pr_info("\"xino=on\" is useless with all layers on same fs, ignore.\n")`
  (`super.c:1145-1147`). There is no mount option that repairs a
  subvolume-shared layout.

**One further reason to leave xino off here.** Upstream restricts offline
changes to the lower for these features: "Offline changes to the lower tree are
only allowed if the metacopy, index, xino and redirect_dir features have not
been used" (`overlayfs.rst:674-679`) — and this map's whole design replaces the
lower image out of band at every image move
(`crates/cang-guest-init/.../nix/root.rs:225-264` and the generation switch in
`docs/build.md:357-370`). The clause is written for the `index`/`metacopy`/
`redirect_dir` xattrs that persist on the upper; `xino` itself persists nothing,
so this is a documentation-conservative argument, not an observed failure. Take
the default: **`xino=off`, no `xino=` on the mount line.**

## 4. `CONFIG_BTRFS_FS_POSIX_ACL` is not set — does overlayfs care?

Not for xattrs, and not for a Nix store, but it does change one errno and one
copy-up path:

- **btrfs still has every non-ACL xattr namespace.** The handler table is
  unconditional (`fs/btrfs/xattr.c:458-487`); `CONFIG_BTRFS_FS_POSIX_ACL` only
  gates the ACL code and the `SB_POSIXACL` superblock flag
  (`fs/btrfs/Kconfig:39-47`; `fs/btrfs/super.c:2218-2220` sets
  `fc->sb_flags |= SB_POSIXACL` only `#ifdef CONFIG_BTRFS_FS_POSIX_ACL`, and
  `super.c:445-455` refuses the `acl` option with `"support for ACL not
  compiled in"` → `-EINVAL`; `show_options` prints `,noacl` when the flag is
  absent, `super.c:1108-1109`). So: **do not pass `acl` to the guest's btrfs
  mount**; `noacl`/default is right.
- **Overlayfs asks the superblock flag, not the mount option**:
  `IS_POSIXACL(inode)` is `__IS_FLG(inode, SB_POSIXACL)`
  (`include/linux/fs.h:2151`). Hence `do_ovl_get_acl()` returns `NULL` at
  `inode.c:465-466` (permissions fall back to mode bits, which is what the store
  uses anyway) and `ovl_set_acl()` returns `-EOPNOTSUPP` at `inode.c:547-550`.
- **The mount-time workdir probe already tolerates it**: removing the workdir's
  inherited ACL xattrs is accepted on `-EOPNOTSUPP`
  (`super.c:356-375`).
- **The one path that would fail** is copy-up of an *ACL-bearing lower file*:
  `ovl_copy_xattr()` copies ACL xattrs through `ovl_copy_acl()` and treats
  failure as fatal ("POSIX ACLs must be copied.", `copy_up.c:124-129`, `52-72`),
  so an upper that cannot store an ACL makes that copy-up fail with
  `-EOPNOTSUPP`. This needs a lower file that actually has an ACL: the lower
  here is the image's own `/nix`, built from the host store.
- **Do Nix store paths carry ACLs?** A bounded sample on this host says no:
  `getfattr -d -m -` over 100 `/nix/store/*` directories and 100 store files
  found **0** objects with *any* extended attribute, let alone
  `system.posix_acl_access` (store directories are mode `555`, store files
  `444`). A build in the guest by `nix-daemon` installs NAR contents through the
  same fixed-mode path, so it does not introduce one either — that second half
  is inference from the mode sample, not a Nix-source citation. Scope of the
  claim: **store objects imported or built normally on this ACL-less
  filesystem**. A foreign lower tree that already carries ACLs (a store copied
  out of an ACL-using filesystem with `getfacl`-preserving tools) would hit the
  copy-up failure above — worth one line in the migration ticket, not a blocker.

## 5. btrfs mount options and ENOSPC

- **`nodatacow`: do not use it.** `nodatacow` is the negated `datacow` token
  (`fs/btrfs/super.c:226`) and its parse branch clears `COMPRESS` and
  `FORCE_COMPRESS` and sets `NODATACOW | NODATASUM`
  (`super.c:391-399`). That throws away data checksums — the exact mechanism
  behind the `/nix` block-lower corruption campaign (memory #334) — and the
  checksums are what turn a lost write into a detected `csum failed` instead of
  silent corruption of a store path. It also buys nothing here: overlay writes
  are ordinary writes, and copy-up is a byte copy anyway (§1). The existing
  `cang-build.raw` also does not set it: the host `mkfs.btrfs -f -L <label>`
  (`crates/cang/src/runtime/launch/components/persistent_disks/raw_btrfs.rs:42-56`)
  and the guest `mount -t btrfs <disk> <mnt>` with no options at all
  (`crates/cang-guest-init/src/guest_init/components/disk/build.rs:159-161`).
  Note the level: the mount option applies to inodes created afterwards and is
  inherited by new inodes from the parent (`fs/btrfs/inode.c:6608-6629`), and
  `chattr +C` on the host's raw *file* is a third, unrelated thing (the host's
  btrfs deciding whether to COW the image file).
- **`compress=`**: an image is a filesystem, so it inherits nothing from the
  host's mount of the directory it sits in — the host's
  `compress=zstd:1,user_subvol_rm_allowed` on `/home/dev/disp` (MAP.md "Starting
  facts") does not follow into the guest. Pass `compress=zstd` (or
  `compress=zstd:1`) inside the guest if space matters: it shrinks what the store
  occupies in the 96 GiB image, costs CPU on write and read, and is
  incompatible with `nodatacow` in the direction above. No correctness effect on
  overlay.
- **`space_cache`**: **do not pass it.** Bare `space_cache` is v1 and now
  degrades to *no* space cache with a warning
  (`fs/btrfs/super.c:513-521`: `"v1 space cache is deprecated, falling back to
  no space cache"`); `space_cache=v2` sets `FREE_SPACE_TREE`
  (`super.c:521-536`). v2 is what `mkfs.btrfs` creates, so pass nothing (or
  `space_cache=v2` explicitly, which is a no-op on a modern fs).
- **`autodefrag`**: available (`super.c:218`, `1125`) and irrelevant — a store
  is write-once.
- **`commit=`**: default 30 s (`ctx->commit_interval =
  BTRFS_DEFAULT_COMMIT_INTERVAL`, `super.c:2215`). Leave it. A longer value
  loses more on a hard VM kill; a shorter one buys durability at the cost of
  more synchronous work on a store import.
- **`noatime`**: low-value but free. Overlayfs deliberately drops inherited
  atime flags from the upper mount (`super.c:530-531`), and every read of
  `/nix/store/...` by a build would otherwise be a candidate atime update on the
  real inode; `relatime` (the kernel default) already limits it to once a day
  per inode, so this is belt-and-braces.
- **ENOSPC.** Four points, and they are not interchangeable:
  1. *Inside the guest's btrfs* — **data reservations never overcommit**
     (`fs/btrfs/block-group.c:1459-1473`: "Data never overcommits, even in mixed
     mode"), so the writer gets `-ENOSPC` from the delalloc reservation
     (`fs/btrfs/delalloc-space.c:498-508`, "Return <0 for error(-ENOSPC or
     -EDQUOT)"); `-EDQUOT` only exists with quota groups enabled
     (`fs/btrfs/qgroup.c:3644`; qgroups are opt-in, off for these images).
  2. *Metadata* — metadata **is** overcommitted
     (`block-group.c:1474-1483`), so a full filesystem can instead fail a
     transaction, and btrfs's own comment describes the outcome: reclaim is
     made "urgent" "to avoid ENOSPC in a transaction leading to a readonly fs"
     (`space-info.c:2120-2124`, and the ENOSPC abort dumper at
     `space-info.c:1988`). A btrfs that goes read-only under a live overlay
     turns the `/nix` upper read-only; overlayfs neither notices nor re-checks.
  3. *Outside the guest* — the raw file on the host filling is **not** an
     ENOSPC inside the guest. The guest's virtio-blk request fails and is
     completed with `VIRTIO_BLK_S_IOERR`
     (`deps/libkrun/src/devices/src/virtio/block/worker.rs:263,306,309`), so the
     guest sees `-EIO` on the block device and btrfs aborts. Auto-grow must
     prevent this, not recover from it.
  4. **Overlayfs adds no translation at all**: `ENOSPC`/`EDQUOT` appear **0**
  times in `fs/overlayfs/*.c` (all `.c` and `.h` files counted). Whatever btrfs
  returns, the writer sees.

## 6. Prior art, and what the docs do *not* say

- `Documentation/filesystems/overlayfs.rst` **never names btrfs**:
  case-sensitive counts `btrfs` 0, `Btrfs` 0, `BTRFS` 0.
- `Documentation/filesystems/btrfs.rst` (43 lines) **never names overlayfs**:
  `overlay` 0, `Overlay` 0, `OVERLAY` 0.
- What the overlayfs doc *does* say is the general requirement, and btrfs meets
  it: "The upper filesystem will normally be writable and if it is it must
  support the creation of trusted.* and/or user.* extended attributes, and must
  provide valid d_type in readdir responses, so NFS is not suitable"
  (`overlayfs.rst:98-107`), plus "The workdir needs to be a directory on the
  same filesystem as upperdir" (`overlayfs.rst:126`). §1 shows each of those
  satisfied by btrfs in this config.
- `fs/overlayfs/*` contains the literal `btrfs` **0** times: nothing in
  overlayfs is btrfs-specific.
- **In this repo** the only overlay-on-btrfs code is legacy:
  `crates/cang-guest-init/.../nix/root.rs:225-264` (the `CANG_NIX_OVERLAY=1`
  path mounts a guest btrfs disk at `/run/cang/nix-disk` and composes `/nix`
  with the same option string), and it is not on the shipped launch path. The
  guest's Podman storage is the **btrfs driver, not overlay**
  (`crates/cang-guest-init/src/guest_init/components/podman/config.rs:41-47`,
  `driver = "btrfs"`, with a test asserting no `driver = "overlay"`). So "container
  runtimes do this routinely" cannot be sourced from this repo, and I did not
  find an upstream document that names the pairing — see the two counts above.
  The claim in this note rests on the kernel code, not on that folklore.

## 7. Traps worth carrying into the implementation tickets

1. **Same-superblock layouts are the trap, not btrfs.** Upper and work must be
   one mount (`super.c:811-819`) and must not share a superblock with the lower.
   Two subvolumes of one btrfs are one superblock to overlayfs
   (`fs/btrfs/super.c:1857-1863`) → `257`/`257` collisions and no `xino`
   rescue (`super.c:1145-1147`).
2. **No reflink copy-up across two btrfs filesystems.** `vfs_clone_file_range()`
   is `-EXDEV` across superblocks (`fs/remap_range.c:384-385`), so
   `ovl_copy_up_file()` byte-copies every first-write to a lower store path
   (`copy_up.c:283-286`). If reflink copy-up matters, the two layers have to be
   **one** btrfs — and then they must be distinct *directories inside one
   subvolume* (inode numbers unique there) rather than two subvolumes. That is a
   different design with a different cost; measure it in ticket 01 rather than
   assume it.
3. **Hardlinks cannot cross btrfs subvolumes** (`fs/btrfs/inode.c:7066-7068`,
   `-EXDEV`). If the upper is split across subvolumes, a Nix store hardlink farm
   breaks. Keep the upper's store tree inside one subvolume.
4. **A failed workdir silently degrades the overlay to read-only**
   (`super.c:389-394`, `1453-1454`) — no mount error. The guest's current
   error handling would swallow that too (`root.rs:472-478` keeps going on a
   failed lower mount), so the implementation should assert the overlay is
   mounted `rw` (`/proc/self/mountinfo` options) instead of trusting the exit
   status.
5. **`space_cache` (v1) is deprecated** and silently turns the cache off
   (`super.c:513-521`); pass nothing or `v2`.
6. **`nodatacow` costs checksums and compression** (`super.c:391-399`) for no
   gain.
7. **`acl` as a btrfs mount option is refused** (`super.c:445-455`,
   `"support for ACL not compiled in"`) if anyone tries to re-enable ACLs at
   mount time; the config's `# CONFIG_BTRFS_FS_POSIX_ACL is not set`
   (`deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto:2869`) is final.
8. **The upper's ENOSPC is the guest's ENOSPC** — data reservations do not
   overcommit (`block-group.c:1459-1473`), metadata does, and a metadata ENOSPC
   at commit can flip the filesystem read-only (`space-info.c:2120-2124`). The
   outer file filling is `-EIO`, not `-ENOSPC`
   (`deps/libkrun/.../block/worker.rs:263`).

## 8. The mount recipe this implies

```text
mount -t btrfs -o noatime,compress=zstd:1 <upper-raw-disk> /run/cang/nix-upper   # compress optional
mount -t overlay overlay \
  -o lowerdir=/run/cang/nix-lower,\
     upperdir=/run/cang/nix-upper/upper,\
     workdir=/run/cang/nix-upper/work,\
     userxattr \
  /nix
```

- one mount for both `upper/` and `work/` (no bind mounts),
- no `xino=`, no `index=`, no `metacopy=`, no `data=`,
- the upper's filesystem **must not** be the lower's,
- `upper/` and `work/` may be plain directories in the btrfs root (no subvolume
  needed); if a subvolume is used, it must be a subvolume of the *upper's* own
  filesystem, never of the lower's.

Nothing in the guest's current command line has to change except the filesystem
under `upperdir`/`workdir`: the options string is already
`lowerdir=…,upperdir=…,workdir=…,userxattr`
(`crates/cang-guest-init/src/guest_init/components/nix/root.rs:440-447`).
