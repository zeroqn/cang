---
label: wayfinder:prototype
title: The relay's death semantics for mmap, and the 4 KiB write cap
status: closed
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

Both questions answered by measurement (2026-11-21,
[note 63](../notes/63-fuse-relay-death-semantics-and-write-cap.md)):

1. **Death semantics: keep the mount, answer `-EIO`, never unmount.** The `mmap`
   livelock in note 62 was a probe defect (a `SIGBUS` handler that returned, so the
   load was re-executed in a loop); with the signal at its default disposition a
   fault on an unread page gets **`SIGBUS` in 8/8 boots**, ~20 ms after that page's
   READ is answered `-EIO`, and it is still owed for as long as the connection stays
   silent (measured: blocked, 0 CPU ticks, 6.2 s). **No unmount delivers it**: `-u`
   is `EBUSY` while the mapping holds the file; `-u -z` detaches the name but the
   superblock survives, so the reader stays blocked *and* path tools start seeing an
   empty directory (`ls` rc=0, `cat` `ENOENT`); `umount -f` and `umount2()` are refused
   for an unprivileged user. Ticket 23's decision 4(a) therefore stands, with
   "never lazily unmount on death" added.
2. **The 4 KiB write cap is a guest-side INIT flag, lifted by 8 added lines in a
   vendored `fuse-backend-rs` 0.12.1**: `max_write` 4096 -> 1 048 576, `max_pages`
   0 -> 256. A 4 MiB `dd conv=fsync` goes from **1 024 WRITE frames at 9.6-11.2 MB/s**
   to **4 WRITE frames at 47.7-107.5 MB/s** (8.6-9.6x wall, 256x fewer frames); a
   64 KiB write drops from 16 frames to 1; warm `git status` keeps the same frame
   count (810/811, 0 WRITEs).

Also recorded: the pump is serial, so one stalled request stalls every other client -
a concurrency lever for later, not a v1 requirement. Harness defects found and named
in note 63 §5 (the `SIGBUS` handler, `opname()`'s shifted table, `stats.tsv` rows lost
on `SIGKILL`, missing `coreutils` symlinks in the rootfs).
