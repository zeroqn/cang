---
label: wayfinder:task
title: Why does a cold cargo build hit ENOENT on freshly created target dirs in /workspace?
status: closed
blocked_by: []
claimed_by: pi + diagnosis child (2026-10-25)
---

## Question

The first baseline run found a blocker the map did not anticipate, and bob has
made it a **prerequisite of this effort**: with the identical tree, the
guest-local arm builds and the guest-virtiofs arm fails, so `cargo`/`rustc` hits
`ENOENT` on directories under `target/` that it has just created inside
`/workspace`. Ticket 03 ruled out write caching, so no decision of ours can
explain it; the suspicion is the fork's virtio-fs itself.

Establish the mechanism, then the fix:

1. **Reproduce minimally and deterministically.** A guest-side shell loop is
   enough if it isolates the operation: `mkdir` a directory then immediately
   `openat`/`create` a file in it; `readdir` right after `mkdir`; `rename` into a
   just-created directory; nested `mkdir -p`. Say which of those fails, and
   whether the failure depends on parallelism (rustc spawns many writers) or
   happens single-threaded too.
2. **Name the layer.** Candidate mechanisms, in the fork
   (`deps/libkrun/src/devices/src/virtio/fs/`): the `DO_READDIRPLUS`/
   `READDIRPLUS_AUTO` replies, the negative-lookup/entry cache with a 5 s
   `entry_timeout` and no `EXPLICIT_INVAL_DATA`, a stale `InodeAltKey`
   (`ino`,`dev`,`mnt_id`) reuse in the inode map, a missing
   `FUSE_NOTIFY_INVAL_ENTRY` on host-side changes, or the single `FsWorker`
   ordering a `CREATE` reply after a dependent `LOOKUP` was already answered from
   cache. Also check the guest kernel side: is a negative dentry being cached
   across a create (the classic "host created it, guest still has a negative
   dentry" pattern) - and does that make this a *general* `/workspace`
   correctness bug rather than a cargo-specific one?
3. **Prove it.** The mechanism must predict a second observable that you then
   check (not just "it failed once"): e.g. if caches are the cause, a
   `mount -o attr_timeout=0`-style guest remount, or touching the entry from the
   host, changes the failure; if it is ordering, a single-threaded writer
   succeeds while parallel writers fail; if it is an inode-key collision, the
   failure depends on two files with the same host `ino` under one mount.
4. **The fix.** Where it belongs (fork vs cang), how large, and whether it must
   ship before any measurement can be trusted. If it is a fork fix:
   tag/pin consequences per `cang-libkrun-fork-maintenance`; if it is cang-side,
   name the file. Include the minimal repro as a runnable check.
5. **Report** the diagnosis in `notes/08-enoent-on-new-dirs.md` with the raw
   logs, and hand ticket 02/07 the verified working arm.

A guest arm may not be run in a session shell; use the delegated recipe
(`cang-live-guest-evidence-harness`), one arm at a time, per-arm logs.

## Resolution (2026-10-25/26, pi + diagnosis child-4)

**The premise this ticket was opened on was wrong, and the investigation
overturned it - recorded here so it is not re-derived.**

`virtio_fs_enqueue_req() failed -5` is **not** a virtio-fs failure and not the
reason a build dies. It is the guest's own **shutdown race**: on poweroff the
kernel's generic `virtio_device_shutdown()` (`drivers/virtio/virtio.c:412`) marks
every virtqueue broken *before* the virtio-fs connection is quiesced, so the
handful of FUSE requests still in flight are refused with `-EIO`
(`virtio_ring.c:674`, which `virtio_fs_send_req()` logs at `virtio_fs.c:1535`).
The fork's device never sees them. **Measured: the burst is orthogonal to the
guest's work** - 3 of 5 identical probes printed it and all 5 wrote their output
file; 6 benchmark arms printed none and all passed. Silence it only with a
`virtio_fs.shutdown` op in the pinned kernel (a libkrunfw patch, upstreamable,
buys a quieter log and nothing else).

What the burst *did* reveal is a real latent defect, now fixed in the fork:
`worker.rs` turned guest-supplied descriptor chains into `Reader`/`Writer` with
`unwrap()` (and `(dis|en)able_notification(..).unwrap()`), so one malformed
chain from the guest **panicked the single FsWorker thread**, which in cang is
the guest's root filesystem - the share then wedges forever. Fork commit
`f3167a42` ("fs: keep the worker thread alive on guest protocol errors", 1 file,
+52/-30 on top of the pinned `79228a72`) fails that one request with a
zero-length used entry instead, and logs notification errors rather than
aborting the thread. Verified live: a cold 81-crate guest build completes with
the patched device, and compile gates pass. It is a latent-defect fix: the `-5`
burst happens with or without it.

Two things this ticket carried are **moved out**, because they are cang-side and
not virtio-fs:

- a dead or still-working guest looks like a **clean exit** - `cang` returns 0
  about a second after boot while the guest keeps working, and a guest that
  powers off without reporting a status becomes exit 0
  (`vmm/mod.rs:502-525`, `exit_code` only written by a FUSE ioctl no cang code
  sends) - [Why does a guest that is still working, or already dead, look like a
  clean exit?](09-cang-exit-status-lies.md);
- the original `ENOENT` on freshly created `target/` directories was never
  reproduced in six clean arms, so it is either guest-side and workload-shaped, or
  an artefact of the earlier session's overlapping VMs -
  [Does a cold full-size cargo build in /workspace still hit ENOENT, and if so
  why?](10-enoent-still-real.md).

Full analysis, with the kernel citations, the burst-vs-success table and the live
transcript: [note](../notes/08-enoent-root-cause.md).

### Evidence so far (2026-10-25, harness side)

The mechanism is **not** a cargo-level ENOENT: the VM itself dies. In
`<state-root>/tasks/<task>/helper.stderr.log` the run ends with a burst of
`virtio-fs: virtio_fs_enqueue_req() failed -5` (guest-side `-EIO`), the console
device is reset mid-flight, and the VMM stops with exit code **0** - so every
layer above reports success, which is why the symptoms are an empty log, an
orphan VM, or an `ENOENT` inside cargo. Full note, with the raw log and the open
questions: [note](../notes/08-enoent-on-new-dirs.md).
