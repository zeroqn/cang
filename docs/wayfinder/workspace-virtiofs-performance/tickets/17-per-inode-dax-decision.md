---
label: wayfinder:grilling
title: Is per-inode DAX worth shipping here, or does the host filesystem decide it for us?
status: closed
blocked_by: []
claimed_by: pi research child-6 (2026-10-26)
---

## Question

bob (2026-10-26) asks for the per-inode form rather than the mount-wide DAX that
ticket 05 dropped: "let virtiofs decide which [files] should be served through
DAX by inode".

Ticket 05 already established: mount-wide `dax=always` is bad here (it covers the
whole guest root, every first fault becomes a `SETUPMAPPING` on the single worker,
no fallback when the window is exhausted, failure modes brick the boot). Per-inode
DAX (`dax=inode`) needs three things that all have to be true:

1. the server advertises `FUSE_HAS_INODE_DAX` in INIT;
2. the server replies `FUSE_ATTR_DAX` on the entry/attr of a chosen inode, which
   only makes sense if the **host file** is on a filesystem that supports DAX;
3. the guest kernel has `FUSE_DAX` and accepts the flag.

The per-inode-DAX research (`notes/14-per-inode-dax-viability.md`) answers
whether (2) is even possible on this host and what virtiofsd does. This ticket
turns that into a decision:

- **If the host filesystem cannot report DAX per file:** close it as impossible
  here, with the one-line reason, and say what would change it (a DAX-capable
  host filesystem, e.g. ext4 with `-O dax`/pmem, or xfs on a dax device).
- **If it can:** decide the policy ("which inodes"), the fork work to advertise
  and reply, the window size, and whether the *cache* ticket and this one ship in
  the same fork tag (they touch the same INIT path).
- **Either way:** say what measurement would prove DAX helped this workload, given
  that rustc reads each source once and `mmap`s rlibs (ticket 05's analysis), and
  whether the answer is "measure after the cache change" instead of now.

## Resolution (2026-10-26, pi research child-6)

**No - and not because of cang: no file on this host can ever report DAX.**
Note: [14-per-inode-dax-viability](../notes/14-per-inode-dax-viability.md).

- `/dev/sdb2` is ext4 on a plain SCSI disk; `/sys/bus/dax/devices` is empty, there
  is no pmem/nvdimm/virtio-pmem device, ext4 gates per-inode DAX on
  `EXT4_FLAGS_BDEV_IS_DAX`, and btrfs has no DAX at all. So even a perfect
  server-side policy would enable **zero** inodes.
- Per-inode DAX is not a cheaper variant of the mount-wide window: it needs the
  window anyway (`fuse_should_enable_dax()` bails when `fc->dax == NULL`), it is
  just *selective* use of it. `dax=inode` degrades silently to `never` instead of
  failing the mount, which is the one advantage over ticket 05's `dax=always`.
- virtiofsd has no per-inode DAX either (both the Rust and the retired C daemon
  carry `ATTR_DAX`/`HAS_INODE_DAX` as unused constants); the only per-inode policy
  in existence is an unmerged 2021 patchset (host-xflag and >=32 KiB rules).
- The decisive check is one command: `statx(STATX_ATTR_DAX)` on a `/workspace`
  file returns 0 here, permanently.

**What would reopen it:** a DAX-capable host filesystem (ext4 with `-O dax` on a
pmem/brd-with-dax device, xfs on a dax device). The fork is one small patch away
(`FUSE_HAS_INODE_DAX` is already defined and unused at `fuse.rs:178`; the missing
pieces are the INIT advertisement, the `ATTR_DAX` constant and flags on the
GETATTR reply, ~8 files) - but there is nothing on this host to point it at.
