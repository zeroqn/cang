---
label: wayfinder:research
title: How can the host get a live read/write mount of the guest's btrfs workspace over the only transport cang has?
status: open
blocked_by: []
claimed_by: "pi research child-58 (2026-11-20)"
---

## Question

bob's v1 reverse share is a **live read/write mount of the guest's filesystem on
the host**, every operation served by the guest (map, 2026-11-20 Q2=(i)). What can
actually carry that, and what does each candidate cost?

Ground truth to start from (verified 2026-11-20): the fork has **no** guest->host
filesystem transport - its device kinds are
balloon/block/console/fs/gpu/input/net/rng/rtc/vhost_user/vsock
(`deps/libkrun/src/devices/src/virtio/`), no 9p and no FUSE client; **vsock** is
the only bidirectional transport cang already drives (`add_unix_port`, `add_port_forward`
in `deps/libkrun/src/libkrun/src/api/device_builders.rs`); the host is
unprivileged and cannot loop-mount a raw image; `cang exec`/`attach` are the only
host-side file operations today.

Answer, with sources and a priced comparison:

1. **Which candidates exist at all.** At minimum: (a) a host-side FUSE server
   (libfuse/crosvm-style) whose backend proxies each operation over vsock to a
   guest agent that performs the syscalls; (b) the guest running a real file server
   (NFS/SMB/9P) bridged to the host through a vsock unix-port and the host mounting
   it; (c) a userspace btrfs reader on the host reading the image directly
   (read-only, no guest involvement); (d) `cang exec`-mediated copy/sync. For each:
   does it produce an actual mount, which of `open/mmap/rename/fsync/xattr/inotify/
   hardlink/O_TMPFILE` it can serve, and what it needs on each side.
2. **Prior art.** Is there a project doing "host FUSE client <-> virtio-vsock <->
   guest filesystem" (or 9p-over-vsock) that cang could reuse rather than build?
   Name it with its licence and its maturity, or state plainly that none exists.
3. **Price.** Per-operation cost: what a round trip over vsock costs against the
   virtio-fs share's measured ~516 us host CPU per create (note 46) and the ~7 k
   requests/s ceiling (note 45), and what that implies for a `git status`, an
   editor save, and rust-analyzer's `mmap` traffic. State the floor, not a
   wish.
4. **Where the guest side lives.** Whether the guest agent/server is a new
   `cang`-owned binary in the image, an existing package (`nfsd`, `samba`,
   `virtiofsd`-style), or something the guest kernel already provides, and what its
   seccomp/landlock needs look like given `crates/cang/assets/seccomp/`.
5. **Recommendation**: one candidate, and the smallest spike that would prove it
   (what runs where, what mounts, what the first measured gate is).

Deliverable: a note under `notes/` (e.g. `58-reverse-share-transport.md`) with the
candidate table, the price table, and the recommendation; link it here. Do **not**
boot a VM and do not modify code.

## Resolution

(to follow)
