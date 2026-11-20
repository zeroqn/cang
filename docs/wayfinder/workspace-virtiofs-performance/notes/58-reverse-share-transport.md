# 58 - What can carry a live host mount of the guest's btrfs workspace

Ticket: [22-reverse-share-transport](../tickets/22-reverse-share-transport.md).
The map redrew the destination on 2026-11-20: `/workspace` becomes btrfs on a
block device, the guest is authoritative, and the host needs a **live read/write
mount** of that tree back (`MAP.md`, `## Destination`). This note prices the
transport, not the contract.

Read-only work: no VM boot, no `nix` build, no cang launch, no source edit. One
*reversible* host probe was run (a temporary `fuse-overlayfs` mount under `/tmp`,
unmounted in the same command) because "can this host mount FUSE at all" is the
hinge of every candidate below.

Sources, and what each is:

- `deps/libkrun` at **`v2.0.0-cang.22`** (`2fad450a`), `deps/libkrunfw` at
  **`v5.6.2-cang.6`** (`KERNEL_VERSION = linux-7.3.5`,
  `deps/libkrunfw/Makefile:1`) - re-read at these tags, not taken from older notes.
- This host: kernel **`7.3.4-xanmod1`**, `uid=1000(dev)`.
- Kernel source fetched from `git.kernel.org` (torvalds HEAD) on 2026-11-20 for
  the filesystem-type and FUSE facts in §1.2/§2 - quoted by identifier, because
  those files have no stable line numbers at HEAD.
- LKML / LWN / crates.io / man-pages URLs inline for every web fact.

---

## Verdict

1. **Exactly one candidate can become a mount on this host: FUSE.** NFS, 9p and
   SMB are not merely awkward, they are unreachable: **none is registered in the
   running host kernel** (`/proc/filesystems` has `nodev fuse`, `fuseblk`,
   `virtiofs`, `overlay` - and no `nfs`, no `9p`, no `cifs`), the client modules
   that exist on disk (`fs/nfs/nfs.ko.xz`, `fs/9p/9p.ko.xz`,
   `fs/smb/client/cifs.ko.xz`) are unloaded and an unprivileged user cannot
   `modprobe` them, and the kernel source gives **NFS `FS_USERNS_DELEGATABLE`,
   9p no user-namespace flag at all, and only FUSE `FS_USERNS_MOUNT`** (§1.2).
   Verified live: an unprivileged FUSE mount works on this host today
   (`fuse.fuse-overlayfs`, `user_id=1000`, mounted by `/run/wrappers/bin/fusermount3`
   with no root and no `unshare`; write through it succeeded; unmounted cleanly).
2. **The transport is already in the tree, in the right direction.** cang drives
   one vsock device and registers unix-port channels; `listen = true` makes
   libkrun **accept a host-side connection and carry it into a guest listener** -
   the primitive `cang exec` uses (`launcher.rs:838`,
   `unix_proxy/unix.rs:435-459`, `muxer_thread.rs:196-213`). A host FUSE daemon
   connecting to such a socket reaches a guest agent with **no new device and no
   fork change**; the next free port is `50_430` (used: 50 426 attach, 50 427
   waypipe, 50 428 exec, 50 429 pulse).
3. **No project does host-FUSE <-> virtio-vsock <-> guest filesystem, but each
   half exists somewhere else.** `vspfs` is the same transport shape
   with the halves **swapped** (FUSE client *in the guest*, `AF_VSOCK`, server on
   the host); sshfs can mount **over vsock directly** (`-o vsock=CID:PORT`, no
   bridge) against a guest `socat VSOCK-LISTEN:… EXEC:sftp-server`; and 9p's
   `trans=vsock` is **merged in mainline** (`net/9p/trans_fd.c`
   `p9_fd_create_vsock`, `CONFIG_NET_9P_VSOCK`, documented in
   `filesystems/9p.rst`). None of them is a candidate here, for the reasons in
   §2/§3 - the 9p one because it needs a patched *host* kernel, a 9p server in
   the guest and a privileged mount.
4. **The price floor is worse than the forward share** - and it has since been
   measured on cang's own transport rather than borrowed: **143 µs median per
   64-byte round trip** through libkrun's userspace muxer, 6.6 k ops/s in one
   stream, p99 273 µs, with the muxer itself only 3.0 µs of it
   ([note 61](61-reverse-share-transport-spike.md)). This paragraph originally
   quoted **~224 µs** from LWN's `vhost-vsock` table
   (https://lwn.net/Articles/1102567/); cang is faster than that, so treat 143 µs
   as the floor. Either way it is ~1.2-2x the forward share's ~120 µs wall per
   request (note 46 §1), so plan on **~6 k ops/s per in-flight stream**; 8 streams
   reach 15.5 k ops/s aggregate but 483 µs per op, and an 8-vCPU guest is 3x worse
   than a 2-vCPU one, so concurrency is not the large lever this note assumed.
   Measured on this host's own repo, `git status` issues **4 100 path
   operations** (2 458 `newfstatat`, 915 `openat`, 727 `getdents64`; 52 ms
   native) -> **0.2-0.45 s** through a reverse share at one stream; an editor save
   or a `pi` file read is 3-6 ops (**~1 ms**); a rust-analyzer tree read ~3 k ops
   (**~0.5-1 s**). A host-side *build* would pay the share's own additive ~32 s
   (464 193 requests, note 51) on top of that - which is why the destination puts
   the build in the guest.
5. **Recommendation: candidate (a), a FUSE relay, with the guest half built on
   `fuse-backend-rs`** (Apache-2.0 AND BSD-3-Clause, 0.14.0, 2026-02-26, 2.0 M
   downloads, Cloud Hypervisor org - "a rust library for Fuse servers and
   virtio-fs devices" with a pluggable transport) and the host half a thin
   `/dev/fuse` <-> vsock **frame pump**, not a from-scratch FUSE server. Not the
   fork's own 10.9 k-line FUSE server: its message entry point is welded to
   virtqueue `Reader`/`Writer` over `GuestMemoryMmap` (`server.rs:218-226`,
   `descriptor_utils.rs:187/394`), and its crosvm headers carry a BSD-style
   licence note with no BSD text in the tree (§5.2). Fallback if that transport
   seam turns out to be virtqueue-bound: `fuser` (MIT, 0.18.0) on the **host**
   plus a translated op RPC instead of raw frames (§6).
6. **The smallest spike measures the round trip before any filesystem
   semantics**: one vsock port, an echo protocol, 1 and 8 concurrent clients.
   **Gate: if a single op exceeds ~250 µs** a reverse-share `git status` is >1 s
   and this cannot be the product surface - take that to bob rather than
   building the filesystem half (§6).

---

## 1. What exists today, measured against the ticket's ground truth

### 1.1 The ground truth holds at the pin

`deps/libkrun/src/devices/src/virtio/mod.rs:13-41` declares exactly
`balloon block console fs gpu input net rng rtc vhost_user vsock` (the rest are
infrastructure modules). `grep -F '9p'` over `deps/libkrun/src` and over
`crates/` returns **0 hits**; nothing in the tree opens `/dev/fuse`
(`grep -F 'dev/fuse'` over `deps/libkrun/src` and `crates` = 0), so there is a
FUSE *server* (the virtio-fs frontend) and no FUSE *client*. `cang exec`
(`crates/cang/src/cli/mod.rs:631`) and `cang attach` remain the only host-side
file operations.

### 1.2 Why only FUSE can be mounted here

Three gates, in order:

| filesystem | registered in the running host kernel? | user-namespace mount? | verdict |
|---|---|---|---|
| `fuse` | **yes** - `nodev fuse` and `fuseblk` in `/proc/filesystems`; `/dev/fuse` is `crw-rw-rw- 10,229` | **`FS_USERNS_MOUNT`** - `fuse_fs_type.fs_flags = FS_HAS_SUBTYPE \| FS_USERNS_MOUNT \| FS_ALLOW_IDMAP` (`fs/fuse/inode.c`) | mountable unprivileged; **proven live** below |
| `nfs` | no - `nfs.ko.xz` present, unloaded | **`FS_USERNS_DELEGATABLE`** only (`nfs_fs_type.fs_flags = FS_RENAME_DOES_D_MOVE \| FS_BINARY_MOUNTDATA \| FS_USERNS_DELEGATABLE`, `fs/nfs/fs_context.c`) - needs an initial-namespace *delegation* | out |
| `9p` | no - `9p.ko.xz` present, unloaded | **no user-namespace flag**: `v9fs_fs_type.fs_flags = FS_RENAME_DOES_D_MOVE` (`fs/9p/vfs_super.c`) | out |
| `cifs` | no - `fs/smb/client/cifs.ko.xz` present, unloaded | (not reached) | out |

The live arm, run once and cleaned up in the same command:

```
$ fuse-overlayfs -o lowerdir=…,upperdir=…,workdir=… /tmp/rsprobe/mnt &
$ grep rsprobe/mnt /proc/self/mountinfo
734 70 0:393 / /tmp/rsprobe/mnt rw,nosuid,nodev,noatime … - fuse.fuse-overlayfs
    fuse-overlayfs rw,user_id=1000,group_id=993,default_permissions
$ echo hello > /tmp/rsprobe/mnt/x.txt   # succeeds
$ stat -f -c "type=%T" /tmp/rsprobe/mnt # type=fuse
$ fusermount3 -u /tmp/rsprobe/mnt       # clean
```

So the host can mount FUSE as `uid 1000` **without root, without a userns and
without `mount(2)` rights**, through the setuid `fusermount3` helper. What it
cannot do is mount anything else.

### 1.3 The guest<->host channel, and the direction that matters

`configure_vsock` (`crates/cang/src/runtime/vm/libkrun/launcher.rs:771-873`)
registers, on one vsock device (`CANG_VSOCK_GUEST_CID = 3`, `launcher.rs:39`):

| channel | guest port | host path | `listen` | who connects first |
|---|---|---|---|---|
| managed attach | 50 426 | `/tmp/cang-<uid>/a-<hash>.sock` | `true` (`:850-855`) | **host** |
| waypipe | 50 427 | `/tmp/cang-<uid>/wc-…` | `false` (`:820-825`) | guest |
| exec | 50 428 | `/tmp/cang-<uid>/e-<hash>.sock` | `true` (`:838`) | **host** |
| pulse | 50 429 | `…/pulse-…` | `false` (`:803-808`) | guest |

`listen = true` is the direction a reverse share needs: the socket is bound by
libkrun's muxer thread (`muxer_thread.rs:196-213` ->
`unix_proxy/unix.rs:435-459`), and when a host process `connect()`s the acceptor
hands the connection to the muxer as a new proxy into the guest
(`unix_proxy/unix.rs:475-490`); a guest `OP_REQUEST` to a *listening* port is
refused with RST by construction (`muxer.rs:595-601`), so the directions cannot
be confused. Socket paths live under the per-user runtime dir
(`managed_attach_socket.rs:12-53`), not under the task dir.

Two properties to carry into any design:

- **a failed bind is silent**: `create_listening_ipc_sockets` only `warn!`s when
  it cannot create the socket (`muxer_thread.rs:204-208`), so a spike must
  *check that the socket exists* rather than trust the launch;
- **credit is not the wall**: `CONN_TX_BUF_SIZE = 8 * 1024 * 1024`
  (`deps/libkrun/src/devices/src/virtio/vsock/mod.rs:81`), so bulk is fine and
  **latency**, not bandwidth, is the question (§4).

### 1.4 The guest side of a channel already has a template

`cang exec`'s guest half is 464 lines plus an 88-line socket helper:
`VsockListener::bind` is a raw `AF_VSOCK` `socket`/`bind`/`listen`
(`crates/cang-guest-init/src/guest_init/runtime/vsock.rs:31-84`), started in a
thread (`runtime/exec.rs:39-72`), **running as the guest's root identity**
(in-code, `exec.rs:84-86`), one handler thread per client, per-command credential
drops (`exec.rs:151-165`). The framing precedent also exists:
`cang-exec-protocol` is a versioned frame protocol (`PROTOCOL_VERSION = 3`,
`lib.rs:4`) with a **1 MiB payload cap** (`lib.rs:7`) - the same order as the FUSE
maximum message (`MAX_BUFFER_SIZE = 1 << 20`, `fs/server.rs:35`).

---

## 2. Candidate table

`+` served, `-` not served, `~` served with a stated caveat.

| # | candidate | is it a mount? | needs on the host | needs in the guest |
|---|---|---|---|---|
| **a** | host FUSE server/relay over vsock to a guest agent that executes the ops | **yes** - a real FUSE mount at the launch dir | a daemon process; `/dev/fuse`; `fusermount3` **or** an in-namespace mount | a vsock listener agent, root, **no new package** |
| **b1** | 9p over vsock (mainline `trans=vsock`) | yes, *if* the host kernel has `CONFIG_NET_9P_VSOCK` and the mount is privileged | a host kernel with `CONFIG_NET_9P_VSOCK` loaded (module present here, unloaded) + a root mount | a 9p server on `AF_VSOCK` - **none found** (`QEMU 9pfs` is virtio-only; `u9fs`/`diod` are TCP/unix) |
| **b2** | NFS: guest `nfsd`/ganesha, host mounts it | yes, *if* the host has an NFS client and a delegation | root to load `nfs.ko` + `FS_USERNS_DELEGATABLE` grant | `nfs-utils`/ganesha - **not in the image**; guest kernel has no `CONFIG_NFSD` |
| **b3** | SMB: guest `ksmbd`/samba, host `cifs` | yes, *if* the host has `cifs` and privilege | root to load `cifs.ko` | `samba`/`ksmbd` - **not in the image**; guest kernel has no `CONFIG_SMB_SERVER` |
| **b4** | sshfs: host `sshfs -o vsock=CID:PORT` straight to a guest `sftp-server` | **yes - and it works today as a tool combination** (`socat VSOCK-LISTEN:12345 EXEC:sftp-server`, then `sshfs -o vsock=…`) | `sshfs` (not installed; nix package) | `openssh`'s `sftp-server` - **not in the image** |
| **c** | userspace btrfs reader on the image file (`btrfs-fuse`) | yes (FUSE), **read-only** | `btrfs-fuse` | nothing - the guest is not involved |
| **d** | `cang exec`-mediated copy/sync | **no** | existing CLI | existing CLI |

The op matrix. For (a) the answer is "whatever the guest agent serves", so the
column states what the FUSE protocol/kernel allow and what the fork's own server
shows is achievable:

| op | (a) FUSE relay | (b1) 9p | (b2/b3) NFS/SMB | (b4) sshfs | (c) btrfs reader | (d) exec copy |
|---|---|---|---|---|---|---|
| `open` | + (`FUSE_OPEN`, or none under `FUSE_NO_OPEN_SUPPORT`) | + | + | + | + | - (a copy is not an open) |
| `mmap` | + shared and private: reads via `filemap_fault`, writes via `fuse_page_mkwrite` (`fs/fuse/file.c`, `fuse_file_vm_ops`) | ~ (v9fs mmaps; cache mode decides coherence) | ~ (NFS mmap, readahead-driven) | ~ (SFTP cache; shared writable mmap unverified) | + (read-only) | - |
| `rename` | + | + | + | + | - | - |
| `renameat2` (`EXCHANGE`/`NOREPLACE`/`WHITEOUT`) | + - the fork sanitises exactly those three flags (`fs/server.rs:1640`) and calls the real `renameat2` (`linux/passthrough.rs:1935`) | **-** (`v9fs_vfs_rename` rejects any flags) | **-** (`nfs_rename`: `if (flags) return -EINVAL`) | ~ (`RENAME_NOREPLACE` only) | - | - |
| `fsync`/`fdatasync` | + (`FSYNC`; the fork answers it, `passthrough.rs:2149`) | + | + | ~ (SFTP has no fsync op; sshfs flushes on close) | - | - |
| `xattr` | + (`SET/GET/LIST/REMOVEXATTR`, `server.rs:257-260`) | ~ | + | **-** (SFTP v3 has no xattr) | - (btrfs-fuse lists xattrs as unsupported) | - |
| `inotify` | ~ **local events only**: inotify "does not catch remote events that occur on network filesystems" and misses mmap-driven writes (`inotify(7)`, *Limitations and caveats*); FUSE has no far-side notify that maps to inotify (`FUSE_NOTIFY_*` is invalidate/store/delete only, `include/uapi/linux/fuse.h`) | - same | - same | - same | n/a | - |
| `hardlink` | + (`LINK`, `server.rs:250`) | + (`.link = v9fs_vfs_link`) | + | ~ (OpenSSH `hardlink@openssh.com` extension only) | - | - |
| `O_TMPFILE` | **-** kernel opcode is `FUSE_TMPFILE = 51` (`include/uapi/linux/fuse.h`) and the fork's `Opcode` enum (`fs/fuse.rs:674-722`, 47 variants) has no `TMPFILE`, so a mount built on that server answers `ENOSYS`; a new guest agent can add it | - (no `.tmpfile`) | + (SMB2) | - (`O_TMPFILE` absent from `sshfs.c`) | - | - |

Three consequences that decide things:

- **(c) is dead on arrival for this map**, not just for writes: `btrfs-fuse` is
  read-only by construction and cannot represent subvolumes ("one FUSE must has
  the same `stat::st_dev`… in kernel btrfs returns different `stat::st_dev` for
  different subvolumes", README), while the planned layout **is**
  subvolume-based (ticket 24) - and a second writer on a mounted btrfs image
  corrupts it, so a userspace reader can never coexist with the live guest.
- **(b1/b2/b3) are absent, not slow**: two of the three needed guest kernel
  pieces are compiled out (`# CONFIG_NET_9P is not set`, no `CONFIG_NFSD`, no
  `CONFIG_SMB_SERVER` in `deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto`), the
  host has none of the clients registered, and all three need root to mount.
- **(b4) is the interesting near miss**: it is the only candidate that needs *no
  new transport code at all* (sshfs speaks `AF_VSOCK` itself) and it fails on
  exactly the ops a git/agent workspace needs - `RENAME_EXCHANGE`/`WHITEOUT`, no
  `fsync` op, no xattr, no `fallocate`/`copy_file_range` (`sshfs.c` has none of
  them), hardlinks only through an OpenSSH extension - and upstream says it "does
  not have any active, regular contributors". Good as a *spike reference*, not as
  the product.
- **(d) is not a mount** and cannot be made into one by degree: `cang exec` is a
  one-shot frame session, it loses the exit status (ticket 09), and it has no bulk
  file primitive.

---

## 3. Prior art

**The exact shape - host kernel mount of the guest's real filesystem over vsock -
does not exist.** Searched "FUSE over vsock", "vsock FUSE server", "9p over
vsock", "NFS over vsock", "reverse virtio-fs", "guest exports filesystem to host
KVM", "AF_VSOCK 9p", "mount guest filesystem on host via vsock FUSE daemon", plus
the Firecracker vsock docs and the virtio-fs documentation. What exists:

1. **`vspfs` - FUSE-over-vsock, halves swapped.** "A lightweight user-space file
   system solution based on vsock, inspired by sshfs design. Mount host
   directories in a Guest virtual machine"
   (https://github.com/dogonthemoon/vspfs):
   the diagram is `guest: Linux VFS -> FUSE -> /dev/fuse -> vsp_fs <-> AF_VSOCK
   <-> vsp_server` on the host. cang needs the two sides transposed; the protocol
   shape ("VSP") is the template, the code is not reusable, and its licence is
   not stated in the repo root (no `LICENSE` at `master`), with no releases.
2. **sshfs over vsock** (GPL, 3.7.7) - a *shipped* FUSE mount over `AF_VSOCK`
   (`-o vsock=CID:PORT`, README "Using vsock"), which is why (b4) is in the table:
   it proves the mount half and the transport half separately, and its op gaps
   (above) are exactly what disqualify it.
3. **9p `trans=vsock` - merged in mainline.** `net/9p/trans_fd.c` has
   `p9_fd_create_vsock()` behind `CONFIG_NET_9P_VSOCK` with a `p9_vsock_trans`
   module named `vsock`, and `Documentation/filesystems/9p.rst` documents
   `mount -t 9p -o trans=vsock[,port=<port>] <CID> /mnt/9` where "CID is …
   2 for host from a guest VM, 3+ for guest VMs from the host" (it was a Jul-2026
   patch series under review - https://lkml.iu.edu/hypermail/linux/kernel/2607.0/01729.html,
   review https://lkml.iu.edu/2607.0/05540.html - and has since landed). It is
   the *client* transport only: no userspace 9p server was found that listens on
   `AF_VSOCK` (QEMU's 9pfs is virtio-only, `u9fs`/`diod` are TCP/unix), and 9p
   cannot be mounted unprivileged
   (§1.2), so for cang this is the transport half of a candidate that still fails
   three other gates.
4. **NFS/RPC over vsock: proposed twice, never upstream.** `[RFC 00/10] NFS: add
   AF_VSOCK support to NFS client` (2015, https://lwn.net/Articles/647516/) and
   `draft-stefanha-nfsv4-rpc-over-vsock` (expired Internet-Draft, 2017). No
   `AF_VSOCK` transport in `net/sunrpc` was found.
5. **`libguestfs guestmount`** (GPL-2.0+) - host FUSE mount of a guest filesystem,
   but **offline**: its man page warns write mode on a live VM is dangerous and
   "the virtual machine must be shut down". That is candidate (c)'s ancestor and
   the same trap.
6. **A FUSE-over-vsock design doc for Windows HCS** (nanosandboxai, "Superseded /
   historical") describes `guest /dev/fuse <-> vsock <-> host
   Server<PassthroughFs>` - i.e. the forward direction tunnelled over vsock - and
   its one durable observation is that "the FUSE protocol is naturally
   self-framing" (`InHeader` 40 bytes, `OutHeader` 16, length-prefixed). That is
   the framing argument for the relay in §6.

**What is reusable, and what cang should not write itself:**

| piece | artefact | licence / maturity | for |
|---|---|---|---|
| FUSE server framework with a **pluggable transport** ("Linux Fuse device" and "virtio-fs protocol") + a passthrough backend | `fuse-backend-rs` 0.14.0 (Cloud Hypervisor org) | **Apache-2.0 AND BSD-3-Clause**, 2.0 M downloads, 2026-02-26 | the **guest** half of (a) |
| a FUSE **client-side** server for the host (`/dev/fuse` + mount; `Session::from_fd` mounts separately) | `fuser` 0.18.0 (cberner) | **MIT**, 6.8 M downloads, 2026-07-22; "most features of libfuse up to 3.10.3" | the **host** half, if the relay shape needs a translated RPC instead of raw frames |
| read-only btrfs | `btrfs-fuse` (adam900710) | root MIT, libs GPL-2.0(+); subvolume-blind | nothing here (see (c)) |
| 9p mount over vsock | kernel `trans=vsock` | GPL-2.0 (kernel) | nothing here (see §1.2) |

---

## 4. Price

### 4.1 The recorded unit of cost (forward share, virtio-fs, this map)

| quantity | value | source |
|---|---|---|
| metadata ceiling, 26-process storm | **~7 k creates/s**, one round trip per path component | notes/44 §3, notes/45 §6 |
| compile-test iteration | **8.2 s / ~92 k requests = ~8.5 k req/s, ~120 µs wall per request** | notes/46 §1 |
| cold-build request mix | **464 193 requests**, priced at **"10 k requests = 1 s of cold build"** (~69 µs each) | notes/51:45, :52 |
| host CPU per create | **516 µs** = 48 % guest vCPUs, 27 % fs server, 24 % VMM | notes/46 §1 |
| one request at one thread | **~360-470 µs wall**, 160-205 µs accounted guest CPU | notes/22:98 |
| the transport's own share of it | **~8-18 µs**, three VM exits per round trip | notes/22:58 |

### 4.2 What vsock actually costs, and what the relay adds

> **Superseded by measurement** ([note 61](61-reverse-share-transport-spike.md)):
> on cang's own fork a 64-byte round trip through the userspace muxer is **143 µs
> median** (p99 273 µs), and the muxer itself is 3.0 µs of it. The table below is
> third-party `vhost-vsock`, kept as the reasoning this section was written on;
> every "at ~224 µs" column under it is now pessimistic by ~1.6x.

Measured, third-party (KVM guest, `vhost-vsock`, receiver pinned, IRQ on another
vCPU, 10 000 requests):

| message | ping-pong RTT |
|---|---|
| 64 B | **224.148 µs** (224.151 µs with an RX-batching patch) |
| 256 B | 126.968 µs (earlier series) |
| 4 KiB | 130.362 µs (earlier series) |

- https://lwn.net/Articles/1102567/ (table "Request-response latency (Ping-Pong
  RTT, 10 000 requests)"), https://lwn.net/Articles/1099769/ (earlier series).
- Throughput is not the problem: 5.5-5.8 Gbit/s guest-RX at 64 K buffers (same
  table), and `iperf3-vsock`'s sample run is 14.3 GB in 10 s = 12.3 Gbit/s
  (https://github.com/stefano-garzarella/iperf-vsock).

Those figures are **optimistic for cang**: `vhost-vsock` does the muxing in the
host kernel, while libkrun's vsock is a **userspace muxer thread** per device
(`muxer_thread.rs`, one epoll loop) reached through a unix socket, so a cang
round trip has at least one more userspace hop than the measured one. Take
**≥224 µs per op** as the floor, i.e. **~4.4 k ops/s for one in-flight request**;
a reverse share's per-op total also has the host daemon's two `/dev/fuse`
syscalls, the guest agent's syscalls and a second transport crossing (~8-18 µs of
exits, note 22:58) on top.

Concurrency is the only lever that moves that number, and it has a hard
precondition: the guest agent must serve requests **concurrently** (thread pool
or io_uring), because a serialized agent caps the whole mount at 4.4 k ops/s
regardless of client parallelism. The forward share's own ceiling (~7-8.5 k/s
aggregate) is that same shape of arithmetic with more parallelism behind it.

### 4.3 What that means for the host tools bob named

VFS operation counts measured on this host against this repo (read-only,
`strace -f -c`); `git status` here took **52 ms** native.

| host workload | VFS ops measured | FUSE requests (est.) | at ~224 µs/op, 1 stream |
|---|---|---|---|
| `git status` (758 tracked files) | 4 100 (2 458 `newfstatat`, 915 `openat`, 727 `getdents64`) | 1.5-3 k after dentry/attr cache hits | **0.4-0.9 s** |
| `git diff --stat HEAD~1` | 3 614 (2 434 stat, 431 open, 257 read, 245 `mmap`) | ~1.5-2.5 k | 0.4-0.7 s |
| `pi` reads one 48 KB doc | 4 for the file itself (LOOKUP/OPEN/READ/RELEASE) | 4 | **< 1 ms** |
| editor save (write + close) | 3-5 (CREATE/WRITE/FLUSH/RELEASE) | 3-5 | **< 1 ms** |
| rust-analyzer cold scan of the tree | 758 files x ~4 | ~3 k | 0.7-1.3 s |
| a *host-side* cold build | 464 193 (note 51) | 464 193 | **~100-140 s of pure transport** (the share's own additive 32 s, multiplied by the worse per-op cost) |

Read that table as the shape of the answer: **the reverse share is fine for the
agent harness and an editor, visible-but-bearable for `git`, and wrong for
anything build-like** - the same conclusion the map reached from the other end.

---

## 5. Where the guest side lives, and what the sandbox says about it

### 5.1 Not an existing package

Every relevant package is absent from the image and from the repo's nix
expressions (`grep -F` counts, all **0**: `fuse3`, `libfuse`, `nfs-utils`,
`nfsd`, `samba`, `smbd`, `ksmbd`, `ganesha`, `virtiofsd`, `fuser`, `sshfs`,
`openssh`). What is there: `pkgs.btrfs-progs` (`nix/image/layers.nix:424`),
`pkgs.socat` (`:433`, wrapped as `cang-sidecar-proxy` at `:158-194`),
`pkgs.util-linux` (`:456`, so guest `mount`/`unshare` exist), `python3` with a
small package set (`:298-303`). So a guest server is **new cang code in the
image**, whichever candidate wins; nothing can be "just installed".

### 5.2 Reusing the fork's FUSE server is not the cheap path

The fork owns a complete FUSE server - `server.rs` 1 969 lines, `fuse.rs` 1 391,
`linux/passthrough.rs` 2 929, `filesystem.rs` 1 244 (10 948 Linux-relevant
lines), 47 dispatched opcodes (`server.rs:238-320`) - and the backend *is* behind
a trait (`FileSystem`, `filesystem.rs:380`; `Server<F: FileSystem + Sync>`,
`server.rs:78`). But the message entry point is not transport-neutral:

```rust
pub fn handle_message(&self, mut r: Reader, w: Writer, … )   // server.rs:218-226
```

where `Reader`/`Writer` are virtqueue types built as
`Reader::new(&GuestMemoryMmap, DescriptorChain)` (`descriptor_utils.rs:187`,
`:234`; `Writer` at `:394`) inside the device worker (`fs/worker.rs:300-320`).
Reuse therefore means either a ~1 900-line signature refactor in the fork (a
carried diff, forever) or copying the code into cang and swapping the transport -
and those files carry crosvm's *"Use of this source code is governed by a
BSD-style license"* header (`server.rs:1-3`) while the tree's only licence file
is **Apache-2.0** (`deps/libkrun/LICENSE:1-3`) with **no** `THIRD-PARTY` file, so
a copy would first have to settle which terms apply. `fuse-backend-rs` is the
same capability with the transport seam already designed in, under a two-licence
grant cang can take (Apache-2.0 AND BSD-3-Clause).

### 5.3 The recommended split

| half | where | why |
|---|---|---|
| FUSE **server** for the guest's tree | **guest**: a new cang-owned agent binary built on `fuse-backend-rs` (its passthrough backend over the guest's own `/workspace` mount) + a **new vsock transport** for that crate's transport layer | the guest is root and owns the filesystem; the crate's `/dev/fuse`-and-virtio-fs transports show the seam |
| FUSE **frame pump + mount** | **host**: a small cang daemon that mounts the launch dir (libfuse3's/`fuser`'s mount helper, or an in-namespace mount) and copies frames between `/dev/fuse` and the vsock socket | "the FUSE protocol is naturally self-framing" (§3.6); no semantics on the host |
| protocol | the FUSE wire format itself, versioned by `INIT` | no second dialect to keep in sync, unlike the exec channel's frames |
| the port | `CANG_SHARE_PORT = 50_430` + `CANG_SHARE_PROTOCOL_VERSION`, registered `listen = true` | mirrors `CANG_EXEC_PORT` (`config/model.rs:51`) and the exec registration (`launcher.rs:838`) |

Fallback if `fuse-backend-rs`'s transport cannot be extended to a socket without
its virtqueue abstractions leaking: invert the split - `fuser` on the **host**
(its `Filesystem` trait gives `rename(…, RenameFlags)`, `copy_file_range`, the
whole op set) and a **translated op RPC** to a thin guest agent (frames modelled
on `cang-exec-protocol`, path/fd ops, inode table on the host). More host code,
less guest code; same one-round-trip-per-op price.

### 5.4 Sandbox implications (where the render-server landmine lives)

- **Guest side: no sandbox at all.** `cang-guest-init` applies no seccomp filter
  and no Landlock ruleset to itself or its children - the only `prctl` uses are
  `PR_CAPBSET_DROP` (`guest_init/process.rs:77`, `:82`) and one in
  `cang-granted` (`granted.rs:94`). Task confinement is uid/gid drop plus the
  capability bounding set. A guest share agent therefore runs **unconfined as
  root**, and its safety is a *contract* question (ticket 23), not a sandbox one.
- **Host side: the worker cannot mount.** The VM worker is a forked child
  re-executed inside `unshare --user --mount --fork … --setuid 0 --setgid 0
  --keep-caps` (`runtime/session/supervisor/identity.rs:118-136`), so it is
  **uid 0 in its own user namespace** - and FUSE is `FS_USERNS_MOUNT`, so a mount
  inside that namespace is *legally* possible. What blocks it is cang's own
  policy: seccomp `Enforce` by default (`launch/plan.rs:228-230`) with
  `"mismatch_action": "trap"` (`assets/seccomp/default.json:3`), 102 syscalls,
  and **no `mount`, no `umount2`, no new-mount-API, no `unshare`, no `setns`**;
  Landlock `Relax` is the default (`landlock.rs:29-36`) and its ruleset grants
  `/dev/kvm` but **no `/dev/fuse` rule** (`landlock.rs:465`); and
  `PR_SET_NO_NEW_PRIVS` is set before the filter (`runtime/seccomp.rs:681-694`),
  so the setuid `fusermount3` route is dead inside the worker even if `execve`
  were allowed.
- **The precedent for "one more host process" is the venus render server**: it is
  spawned as its own host process *before* the network-enter helper
  (`landlock.rs:798-812`), with its own Landlock ruleset that **already grants
  `/dev` (ReadFile/WriteFile/Execute/Truncate/IoctlDev + ReadDir), `/tmp` and
  `/dev/shm` writable** (`landlock.rs:844-866`, `file_access_rights` at `:794`) -
  i.e. `/dev/fuse` and the vsock socket are already inside it - and its own
  seccomp policy (`assets/seccomp/render-server.json`, 110 syscalls with
  `socketpair`/`execve`/`readv`/`writev`) which still has **no mount syscall**.
- **The landmine is real and measured**: the render server's policy once lacked
  `fallocate`, the SIGSYS trap killed the render server mid-venus-init, and the
  VM was reported as `exit 0` (memory #331; the same class of failure hit the
  guest-share seccomp path, notes/52). A reverse-share daemon's policy must be
  audited syscall-by-syscall **before** the first live run (`mount`/`umount2`, plus
  `epoll`/`read`/`write`/`pread`/`pwrite` on the `/dev/fuse` fd and the socket
  syscalls the pump uses), and the spike must fail loudly rather than silently.
- **Where the mount happens**: either in the daemon's own user namespace (needs
  `mount` in its policy) or *before* `no_new_privs` in the spawning process (the
  render-server shape: mount, hand the `/dev/fuse` fd over, then confine). Either
  way one new file under `crates/cang/assets/seccomp/`.

### 5.5 The six host-side sites a new channel touches

1. the port constant and env name (`config/model.rs:12-13`, `:51`);
2. the guest config env writing (`runtime/launch/config/mod.rs:96`);
3. the vsock registration in `configure_vsock` (`launcher.rs:771-873`) with
   `listen = true`;
4. the socket allocation beside `managed_attach_socket::allocate_exec`
   (`runtime/session/mod.rs:302-306`);
5. the session record + codec (`runtime/session/task_control/mod.rs:826-940`) so
   `cang ps`/`attach` find it;
6. the guest-side listener start in the `enter` path next to
   `super::exec::start` (`guest_init/runtime/cang.rs:336-346`), plus
   `IMAGE_CANG_ENV_ALLOWLIST` if a new variable crosses the image boundary
   (`config/model.rs:65-90`).

---

## 6. Recommendation and the smallest spike

**Ship candidate (a): a FUSE relay, host frame pump + guest FUSE server over one
vsock port.** Reasons, in order: it is the only mount this host can make (§1.2);
it is the only candidate that can serve `renameat2`, `xattr`, `hardlink`, `fsync`
and (with a new op) `O_TMPFILE` (§2); the transport it needs is already registered
on a primitive `cang exec` exercises in production (`listen = true`); and its
price lands where the destination needs it - host tools fine at ~1 ms/op, `git`
visible at ~0.5 s, builds staying in the guest (§4.3).

**Reject**: the fork's-server reuse for the client half (§5.2), NFS/SMB/9p
(absent kernel pieces, no unprivileged mount, §1.2), sshfs (right transport,
wrong ops, §2), the userspace btrfs reader (read-only, subvolume-blind, corrupting
as a second writer), exec-mediated copy (not a mount).

**Smallest spike** - one boot, no fork change, no new image package, two steps
that must not be merged:

1. **Transport only.** Register port `50_430` with `listen = true`, start a guest
   listener that echoes a framed 64-byte message back (mirroring `exec.rs:39`),
   and from the host connect to `/tmp/cang-<uid>/…sock` and measure **RTT at 1 and
   8 concurrent clients** plus 8 MiB bulk throughput. This is a few hundred lines
   and answers the only question that can kill the design. **Gate: single-op RTT
   > ~250 µs** (the measured vsock floor is 224 µs, §4.2) means a `git status`
   over 1 s and a rust-analyzer scan over 2 s, i.e. the reverse share cannot be
   the product surface the map promised - stop and take that number to bob.
2. **One real op set through a real mount.** Host pump mounting `/tmp/rs-spike`
   (`fusermount3`, or a userns mount), guest server (fuse-backend-rs passthrough) over the guest's
   `/workspace`; prove a host `cat` of a guest-written file and a guest `cat` of a
   host-written file - the two directions ticket 23's acceptance asks for. Then
   `git status` and one editor-style save through the mount, reported as wall time
   and request count against the native numbers already recorded here (52 ms).

Deliberately not in the spike: `rename`/`xattr`/`hardlink`/`O_TMPFILE` (additive
once the transport is priced), guest-write invalidation (the fork's server has no
outgoing notify path at all - only `NotifyReply`, `server.rs:277`/`:1421` - and it
cannot be represented as inotify anyway, §2), and any `--workspace=image` change
(ticket 18's half).

---

## 7. What this note did not establish

- ~~**No cang-local vsock round-trip measurement.**~~ **Measured on 2026-11-20**
  ([note 61](61-reverse-share-transport-spike.md)): 143 µs median per 64-byte
  round trip on cang's own fork, muxer 3.0 µs of it. It is *better* than the
  borrowed 224 µs, so the "userspace muxer should be worse" hypothesis was wrong;
  §4.2's table and the §6 gate are kept as the reasoning history, not as the
  current number.
- **No live mount of a guest filesystem was attempted** (read-only ticket); the
  FUSE arm proves only that *this host* can mount FUSE unprivileged.
- The op matrix's (a) column is read from the FUSE opcode set, the fork's
  dispatch and `fuse-backend-rs`/`fuser` documentation - not from building a
  daemon; the `O_TMPFILE` gap is proven for the *fork's* server only.
- `fuse-backend-rs`'s transport seam was read from its description and API, not
  from a build; whether a vsock transport can be added without virtqueue
  abstractions leaking is the one open implementation risk (§5.3).
- Prior art is bounded by the queries in §3; a private implementation of this
  exact shape could exist unadvertised.
- Everything about the host (loaded filesystems, module tree) is one host's state
  on 2026-11-20, not a cang requirement.
