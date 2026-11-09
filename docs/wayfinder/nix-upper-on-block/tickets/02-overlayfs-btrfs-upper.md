---
label: wayfinder:research
title: Does a btrfs upper meet every overlayfs requirement in the guest kernel we ship?
status: open
blocked_by: []
claimed_by: ""
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

(to follow)
