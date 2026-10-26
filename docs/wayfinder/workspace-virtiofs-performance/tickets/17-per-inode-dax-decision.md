---
label: wayfinder:grilling
title: Is per-inode DAX worth shipping here, or does the host filesystem decide it for us?
status: open
blocked_by: []
claimed_by: ""
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

## Resolution

(to follow)
