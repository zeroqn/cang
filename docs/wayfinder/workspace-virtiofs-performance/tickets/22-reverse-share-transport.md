---
label: wayfinder:research
title: How can the host get a live read/write mount of the guest's btrfs workspace over the only transport cang has?
status: closed
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

## Resolution (2026-11-20, pi research child-58)

**Only FUSE can be a mount on this host, the transport for it already exists, and
the price is ~2x the forward share's per request - so the design is a FUSE relay,
gated on one measured round trip.**
[note 58](../notes/58-reverse-share-transport.md).

- **NFS/9p/SMB are unreachable, not merely slow**: none is registered in the
  running host kernel, the client modules cannot be loaded unprivileged, and only
  FUSE carries `FS_USERNS_MOUNT` in `fs/fuse/inode.c` (NFS has
  `FS_USERNS_DELEGATABLE`, 9p no userns flag at all). A live unprivileged FUSE
  mount was verified (`fusermount3`, uid 1000, no root, no userns). The userspace
  btrfs reader is read-only and subvolume-blind; `cang exec` copy is not a mount.
- **The transport needs no new device and no fork change**: libkrun's unix-port
  channel with `listen = true` is a host-initiated connection into a guest
  listener - exactly the primitive `cang exec` uses (`launcher.rs:838`,
  `unix_proxy/unix.rs:435-459`). Next free vsock port: 50 430.
- **Prior art**: nothing serves a guest's filesystem to a host mount over vsock.
  Closest are `vspfs` (same shape, halves swapped), sshfs `-o vsock=CID:PORT`
  straight to a guest `sftp-server` (right transport, wrong op set: no
  `RENAME_EXCHANGE`/`WHITEOUT`, no xattr, no fsync op), and 9p `trans=vsock`
  (merged in mainline, but client-only, no `AF_VSOCK` 9p server exists, and 9p
  cannot be mounted unprivileged). Reusable: `fuse-backend-rs`
  (Apache-2.0 AND BSD-3-Clause) for the guest server, `fuser` (MIT) for a host
  server if a translated RPC is preferred.
- **Price floor**: a 64-byte vsock ping-pong measures **~224 µs** (vhost-vsock,
  LWN) against the share's ~120 µs/request, i.e. ~4.4 k ops/s per in-flight
  stream; measured VFS counts put a host `git status` at 0.4-0.9 s, an editor save
  or `pi` read under 1 ms, and a host-side build at ~100-140 s of transport - so
  builders stay in the guest, as the destination already says.
- **Where the guest side lives**: new cang code in the image (no fuse3/libfuse/
  nfs-utils/samba/sshfs/openssh package exists there), listening on a vsock port
  as `cang exec`'s listener does, running unconfined as guest root (the guest
  applies no seccomp/Landlock at all). The host daemon must not be the VM worker:
  its default seccomp policy has no `mount`, its Landlock ruleset has no
  `/dev/fuse`, and `no_new_privs` kills the setuid `fusermount3` route - the
  render-server precedent (own process, own policy, `/dev` + `/tmp` already
  granted) is the shape, and its missing-`fallocate` SIGSYS is the landmine to
  audit against.
- **Smallest spike**: port 50 430 with an echo protocol, RTT at 1 and 8 clients
  (gate: >250 µs single-op means the product surface cannot exist - stop and ask
  bob), then one real mount serving LOOKUP/GETATTR/OPEN/READ/WRITE with a host
  `cat` and a guest `cat` crossing in both directions, then `git status` and one
  editor save measured against the recorded 52 ms native `git status`.
