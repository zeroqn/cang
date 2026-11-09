---
label: wayfinder:research
title: Does a btrfs upper meet every overlayfs requirement in the guest kernel we ship?
status: closed
blocked_by: []
claimed_by: "pi research child-1 (2026-11-10)"
---

## Question

The plan is to compose `/nix` as overlayfs with a btrfs filesystem (inside a raw
image on virtio-blk) as `upperdir`/`workdir`, where today the upper is a
host btrfs directory presented over virtio-fs. Settle, from primary sources,
whether the *pinned guest kernel* will accept that pairing, and which mount
options it needs. Answer with `file:line` from the kernel tree this repo pins
(`deps/libkrunfw`, the 7.3.1 line) and with the upstream documentation at the
same release.

1. **The requirement checks.** Walk `fs/overlayfs/*.c` (`ovl_make_workdir`,
   `ovl_check_*`, `ovl_mount_dir_noesc`) and list every property the upper
   filesystem must have and how each is probed: `d_type`/readdir, `RENAME_WHITEOUT`
   + `RENAME_EXCHANGE` (rename2), xattr support for the overlay xattr namespace,
   the char-device 0/0 probe in the work dir, `FS_IOC_GETFLAGS`/`FICLONE`, sparse
   files, `O_DIRECT` writes, and the `upper fs missing required features` error
   path. For each: does btrfs satisfy it in this config, and what would the
   failure look like (`mount` exit code, dmesg line)?
2. **xattr namespace.** `userxattr` vs `trusted.overlay.*`: what cang's two
   existing overlays pass today (`userxattr`, because they are composed
   unprivileged), whether the guest - root, and privileged inside its own kernel -
   should keep `userxattr` or switch to `trusted.`, and what each costs.
3. **xino and inode identity.** `CONFIG_OVERLAY_FS_XINO_AUTO` is **not** set in
   the shipped guest config. Is `xino=on` still accepted as a mount option, what
   does it do to `st_ino`/`st_dev`, and is it *needed* when the lower (read-only
   btrfs image A) and the upper (btrfs filesystem B) are two independent
   filesystems whose roots both number inodes from 257? `tools/nix-generation-smoke`'s
   README records a wrong-object lookup from exactly that collision. Say which
   mode is safe here and what it costs (xino exhausts inode numbers on 32-bit
   `st_ino`, and its scaling is filesystem-size dependent).
4. **`CONFIG_BTRFS_FS_POSIX_ACL` is not set.** Does overlayfs care (does it copy
   or consult ACLs on the upper)? Do nix store paths carry ACLs at all? Would a
   store path built in the guest by `nix-daemon` set one?
5. **btrfs specifics for this use.** Whether `nodatacow` is wanted for a raw
   image holding a filesystem (the existing `cang-build.raw` does not set it),
   what `compress` does to a raw image's blocks (the host's own `/home/dev/disp`
   is mounted `compress=zstd:1`, the guest's mount of the same image would not
   be), whether `space_cache=v2`/`autodefrag`/`commit=` matter for the guest's
   mount options, and what a full btrfs looks like to the writer (ENOSPC vs
   `EDQUOT`, and whether overlayfs turns it into anything else).
6. **Prior art, briefly.** Anything in the overlayfs docs or btrfs docs that
   explicitly names this pairing (container runtimes use btrfs upper routinely);
   cite it rather than assert it.

Write the findings to
`docs/wayfinder/nix-upper-on-block/notes/02-overlayfs-on-btrfs-upper.md`, with a
short verdict at the top and every claim carrying a `path:line` or a URL at a
pinned tag. Do not edit code.

## Resolution

Assets: [the fact-find note](../notes/02-overlayfs-on-btrfs-upper.md)
(521 lines, every claim a `path:line` into `deps/libkrunfw/tarballs/linux-7.3.1.tar.xz`).

**Yes — with one placement rule and no new mount options.** A btrfs filesystem
inside a raw image is a fully supported overlayfs upper in the 7.3.1 line this
repo pins, and it is *less* constrained than the virtio-fs upper it replaces:
the only fatal `"upper fs missing required features."` branch is gated on the
upper being a remote (`d_revalidate`) filesystem, so for btrfs `d_type`,
`RENAME_WHITEOUT`, `O_TMPFILE` and the overlay xattr namespace are checks btrfs
passes anyway, and any miss would be a warning, not `-EINVAL`.

1. **Requirement checks.** `ovl_make_workdir()` probes, in order: the upper is
   on a writable mount; upper/work are on *one* mount and separate subtrees; the
   `work` directory can be created; `d_type` comes back from readdir; `O_TMPFILE`
   opens; a real `RENAME_WHITEOUT` rename of a temp file yields a char-device
   0/0 whiteout; and `<ns>.overlay.opaque` can be written on the workdir
   (`fs/overlayfs/super.c:663-803`). btrfs satisfies all of them (dir-item
   ftype, `btrfs_tmpfile`, `btrfs_rename2` with `RENAME_WHITEOUT`, trusted/user
   xattr handlers). `RENAME_EXCHANGE`, `FICLONE`, `O_DIRECT`, `FS_IOC_GETFLAGS`
   and sparse handling are *not* mount-time probes (literal counts 0 in
   `ovl_make_workdir`) — they are runtime-only and btrfs has each. Failure modes
   that matter: a `ro` upper is `-EINVAL`; an uncreatable workdir is **not** an
   error — the overlay comes up silently read-only; a non-remote upper never
   trips the fatal branch.
2. **xattr namespace.** Keep `userxattr` (it is what both existing overlays pass,
   `root.rs:440-447`, `nix_overlay.rs:888-895`, and the guest's recorded reason —
   a virtio-fs upper cannot use trusted xattrs — stops applying but the flag
   stays harmless). Guest root *could* use `trusted.overlay.*` (btrfs handler
   plus `CAP_SYS_ADMIN`, which it has), but switching namespace on an existing
   upper silently drops every `user.overlay.*` opaque/origin/impure marker, so
   it is a migration, not a flag flip.
3. **xino.** `xino=on` is accepted regardless of `CONFIG_OVERLAY_FS_XINO_AUTO`
   (that option only sets the *default*, which is off here); it reserves
   `ilog2(numfs-1)+2` = 2 bits of `st_ino` for the layer id and reports one
   `st_dev`. It is **not needed** for two independent btrfs filesystems: with
   `xino=off` overlayfs already gives each superblock its own anonymous
   `st_dev`, so equal underlying inode numbers cannot collide. The recorded
   `257`/`257` bug is the *same-superblock* case — two btrfs **subvolumes**
   reached through one mount are one superblock to overlayfs, the single-fs
   shortcut trusts inode uniqueness inside it, and btrfs subvolumes are separate
   inode spaces. `xino=on` is explicitly ignored there (`super.c:1145-1147`), so
   the fix is placement, not a mount option: never put the upper in a subvolume
   of the lower's filesystem. Keep `xino=off`, which is also the safe default
   for a design that swaps the lower image out of band.
4. **ACLs.** `IS_POSIXACL` is the superblock flag, and btrfs sets `SB_POSIXACL`
   only with `CONFIG_BTRFS_FS_POSIX_ACL`, so overlayfs reports no ACLs
   (`do_ovl_get_acl` → `NULL`) and returns `-EOPNOTSUPP` on `setfacl`; the
   workdir probe already tolerates that. Nix store objects carry none (sampled:
   no xattrs at all, dirs 0555, files 0444). The one exposure is copy-up of a
   *foreign* lower file that does have an ACL: "POSIX ACLs must be copied",
   so that copy-up fails.
5. **btrfs options.** No `nodatacow` (it clears `COMPRESS` and sets
   `NODATASUM` — it would throw away the checksums that make a lost write
   detectable, for no gain). No bare `space_cache` (v1 degrades to no cache;
   `v2` is what `mkfs.btrfs` makes). `compress=zstd` optional, guest-side only —
   the host's `compress=zstd:1` mount does not follow into the guest.
   `noatime` optional. ENOSPC: data reservations never overcommit, so writes get
   `-ENOSPC`; `-EDQUOT` needs qgroups (off); metadata overcommit means a full fs
   can instead abort a transaction and go read-only; overlayfs translates
   nothing (0 `ENOSPC`/`EDQUOT` in `fs/overlayfs/*`). The host raw file filling
   is not ENOSPC inside the guest — it is `VIRTIO_BLK_S_IOERR` → `-EIO`, so
   auto-grow must prevent it.
6. **Prior art.** Neither doc names the pairing: `btrfs`/`Btrfs`/`BTRFS` appear
   0 times in `overlayfs.rst`, `overlay`/`Overlay`/`OVERLAY` 0 times in
   `btrfs.rst`, and `btrfs` 0 times anywhere in `fs/overlayfs/*`. The doc's
   general statement (`overlayfs.rst:98-107`: xattrs + valid `d_type` + same fs
   for workdir) is what btrfs meets. In-repo, the only overlay-on-btrfs code is
   the legacy guest path (`root.rs:225-264`); guest Podman uses the btrfs
   storage driver, not overlay.

**Cost the map should carry:** no reflink copy-up across two btrfs filesystems
(`vfs_clone_file_range` is `-EXDEV` across superblocks, `fs/remap_range.c:384-385`),
and btrfs hardlinks cannot cross subvolumes (`fs/btrfs/inode.c:7066-7068`) — so
the upper's store tree must stay inside one subvolume, and a reflink-copy-up
design would have to put both layers in one btrfs, which is exactly the layout
that re-introduces the subvolume inode hazard.

Note: `docs/wayfinder/nix-upper-on-block/notes/02-overlayfs-on-btrfs-upper.md`
carries every claim with `path:line`, the tarball/patch hashes and the pinned
config.
