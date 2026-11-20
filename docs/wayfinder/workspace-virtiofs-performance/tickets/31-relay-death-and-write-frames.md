---
label: wayfinder:prototype
title: The relay's death semantics for mmap, and the 4 KiB write cap
status: open
blocked_by: []
claimed_by: "pi (2026-11-20)"
---

## Question

[Note 62](../notes/62-fuse-relay-spike.md) proved the relay works and measured two
things ticket 23's contract does not survive as written. Neither is a transport or
design question - both are bounded measurements in the existing
`.scratch/fuse-relay/` harness.

1. **Death semantics for `mmap`.** Ticket 23 decision 4(a) says a dead serving guest
   makes host syscalls return `EIO` and the stale mount stays. Measured: that holds
   for `ls`/`cat`/`stat`/`mkdir`/`rm`/`dd`, but a page fault on an already-mapped
   file **never returns** - the pump answered 1 676 753 `-EIO` replies in ~15 s with
   no `SIGBUS`, and closing `/dev/fuse` instead left the reader spinning ~1 core
   inside the kernel, also with no `SIGBUS`. `git` mmaps its index; `rustc` and
   `rust-analyzer` map everything. Decide: normal `fusermount3 -u`, lazy
   `fusermount3 -z`, or `umount -f`, each measured against a fault-blocked reader -
   whichever gives a deterministic, fast failure. Then amend decision 4(a) to match
   what the kernel actually does.
2. **The write cap.** `max_write=4096` because the guest server's INIT reply advertises
   neither `FUSE_BIG_WRITES` nor `FUSE_MAX_PAGES`: a 4 MiB write is 998 frames at
   12.8 MB/s. Find the least invasive way to advertise both (configuration if the
   crate exposes it, a vendored patch if not), then measure the same 4 MiB write and
   a warm `git status` on both arms to prove the metadata path does not regress.

## Resolution

(to follow)
