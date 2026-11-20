---
label: wayfinder:grilling
title: What does the host's view of its workspace promise - mount point, authority, staleness, failure?
status: closed
blocked_by: [22]
claimed_by: "pi (2026-11-20)"
---

## Question

Once the transport exists (ticket 22), the share's *contract* has to be written
down, because it replaces a promise cang made for free when the workspace was a
host directory: "the host tree is live and authoritative" (ticket 03).

Decide, with bob:

1. **Where the host sees it.** The launch directory is the natural mount point -
   but it is also the directory pi and the editor are already sitting in, so
   either the mount lands on it (and the pre-mount host copy becomes a shadow),
   or the view appears somewhere else and the launch directory stops meaning
   "the workspace". Pick one and say what the other becomes.
2. **Authority.** Settled: the image is authoritative (map, 2026-11-20 Q1). What
   is *not* settled: what happens to the host copy that existed before the image
   (ticket 25), and whether a host write is ever allowed to be served from a
   cache rather than the guest.
3. **Staleness and concurrency.** With both sides mounting the same btrfs tree
   (does the *host* mount it, or does every host operation go through the
   guest?), what does a host `stat` see after a guest `rm`, and what does a
   concurrent write from both sides do. If the host must not mount the same
   btrfs twice, say which side owns the mount and how the other side's operations
   are routed.
4. **Failure.** Guest dead, VM killed, `cang` exit 0 while the guest was still
   working (ticket 09): what a host tool calling `read()` sees, and whether host
   writes are queued, refused, or lost. "Refused, by name" is the default; a
   lost-write window is a decision, not an accident.
5. **What the first acceptance proves**: one host-side edit visible in the guest
   and one guest-side write visible on the host, with the tool being a real one
   (`git`, `rust-analyzer`, pi's file tools), not a `cat`.

## Evidence gathered before asking (2026-11-20, this host, no VM needed)

**What host tools actually ask a filesystem for** (`strace -f -c -e trace=%file,%desc`,
this repo unless stated):

| tool | syscalls | shape |
|---|---|---|
| `git status` | **6 037** total: `newfstatat` 2 459, `openat` 910, `getdents64` 727, `close` 614, `fstat` 547, `read` 291, `mmap` 255, `access` 135 | lookup/attr heavy, no writes, one `mmap` per pack/odb touch |
| `git diff --stat` | 2 947: `newfstatat` 2 399, `openat` 369, `access` 125 | the same shape, smaller |
| `git add -A --dry-run docs` | 559: `newfstatat` 434, `openat` 100 | index work, no data path |
| `vim` save (`-es`, in place) | 535: writes to the file **in place** (`O_WRONLY\|O_CREAT`), `.swp`/`.swx` create + `unlink`, `fchmod`, `ftruncate`, **no `rename`, no `fsync`** | so an editor's save is `open/write/truncate/setattr/unlink`, not a rename |
| `mv` (the temp+rename save shape nano/emacs/pi use) | tries **`renameat2(..., RENAME_NOREPLACE)` first** (`-EEXIST` here), then falls back to `renameat` | the mount must refuse unsupported `rename2` flags with a *fallback-able* errno, never silently do the wrong thing |

**Protocol capability, from this host's `include/linux/fuse.h`** (glibc 2.42 dev
headers): the kernel can take server-initiated invalidation -
`FUSE_NOTIFY_INVAL_INODE = 2`, `FUSE_NOTIFY_INVAL_ENTRY = 3`, plus
`FUSE_NOTIFY_DELETE`/`RESEND`/`INC_EPOCH`/`PRUNE` in newer revisions - so a guest
server *can* knock the host's page/dentry cache down without a remount.

**The mechanism gap that follows** (this is the question, not a finding): those
notifications do **not** synthesise `inotify` events for a host watcher. A guest-side
write - `cargo fmt`, a codegen step, `git checkout` inside the VM - therefore cannot
make a host editor reload, however fresh the read after it is. Only writes that
*arrive through* the host mount generate host-side events.

**The architectural consequence of ticket 24's inversion** (found while writing
this ticket, not on the map before): the image is authoritative and outlives any
single launch, so **between** launches nothing serves the host a view of it. Today
`cang -- cmd` binds the host directory for the life of one command; with
`--workspace-disk` the host directory is *hidden* for that command and reappears
afterwards - by which time the guest may have changed files the host will then read
from the old tree. So the contract covers a case the map has not decided: **who
serves the host's view when no task VM is running.**

## The four decisions this ticket owes bob

1. **When is the host mount there?** (a) only while a task VM runs - cheapest, and
   the host reads a stale tree between commands, which is what the inversion makes
   dangerous; (b) a long-lived *workspace server* VM (`cang workspace serve
   work.raw`) that owns the image whenever no task runs, with task VMs sharing it -
   one more VM lifetime to manage; (c) no VM at all between commands: mount the raw
   image on the host directly, which needs root (loop mount) or a read-only
   userspace btrfs reader. Recommendation: **(b)**, because it makes the host view
   continuous while keeping the guest the single kernel that touches the btrfs
   image, and a task VM can take the image over from the server instead of both
   mounting it.
2. **Staleness for guest-side writes.** (a) accept: a host reader may see a stale
   file until it reopens, and no host `inotify` fires for guest writes; (b) the
   guest server watches its own tree and pushes `FUSE_NOTIFY_INVAL_INODE` for paths
   it did not write through the relay (costs a tree watcher, and `inotify` on a
   build-heavy tree is not cheap); (c) zero cache timeouts everywhere so every
   `stat`/`open` reaches the guest (143 us each, ~0.2-0.45 s for a `git status`).
   Recommendation: **(a) as the written promise, (b) as a follow-up ticket** -
   inotify is unfixable either way, so (b)'s value is only cache freshness.
3. **Concurrent writes to one file** from host and guest. (a) undefined and
   documented as such ("last writer wins, no locking"); (b) host write path takes a
   guest-side per-inode lock; (c) the contract forbids it: while a task VM runs,
   the host mount is read-only. Recommendation: **(c) for v1** - it is enforceable
   in one flag and removes the only correctness risk that a 143 us round trip
   cannot paper over.
4. **Failure when the guest serving the mount dies.** (a) host syscalls return
   `EIO` and the mount stays (stale, refusing); (b) the mount is unmounted and the
   launch directory reverts to the host tree; (c) writes are queued in the host
   daemon and replayed on the next server. Recommendation: **(a) + a named error
   line from `cang`**, because (b) silently changes what the path means and (c)
   invents a durability guarantee cang does not have.

## Round answers (2026-11-20, bob)

Verbatim: *"Q1: (b) Q2: agree (a) first (b) as a follow-up ticket Q3: (c) agree for v1,
CRDT in the future Q4: (a)"*.

- **1 (b)** - a **long-lived workspace server VM** owns the image whenever no task
  VM is running, and a task VM takes the image over rather than both mounting it.
  New ticket: [29](29-workspace-server-vm.md).
- **2 (a)** - the written promise is: **a host reader may see a stale file until it
  reopens, and no host `inotify` fires for guest-side writes.** (b) becomes
  [30](30-guest-side-invalidation-watcher.md), a follow-up.
- **3 (c)** - **while a task VM runs, the host mount is read-only**: the contract
  forbids concurrent writes to one file. A CRDT is named as the future direction and
  is **out of scope for this map** (see the map's Out of scope).
- **4 (a)** - if the serving guest dies, host syscalls get **`EIO`** and the stale
  mount stays, plus a named error line from `cang`; the launch directory never
  silently reverts to the host tree.


## Acceptance arms, measured (2026-11-20, [note 62](../notes/62-fuse-relay-spike.md))

A real relay exists and was measured against decision 5, so each of these is evidence
rather than intent:

| arm | result |
|---|---|
| a file only the guest ever wrote, read on the host | ✔ (`GUEST_NONCE 12c3cadcf0307c5f`, matched to this boot's console line) |
| a guest write, read back on the host | ✔ |
| a host write through the mount, read back inside the guest | ✔ (guest `cat` + `ls -l` in the guest's own shell) |
| decision 4(a): `kill -9` the guest, then host `ls`/`cat`/`stat`/`mkdir`/`rm`/`dd` | ✔ **`EIO` every time**, stale mount stays, one named pump line |
| cost of the same work on both sides (`git status` over 764 files) | **0.211 s through the relay vs 0.040 s native** (5.3x warm, 41x cold when the index stat cache is re-hashed) |
| **decision 4(a) for an `mmap`ed file** | ✘ **not `EIO`**: a fault on an already-mapped page spins forever (`-EIO`-answering pump: 1.67 M replies in 15 s, no `SIGBUS`; `close(/dev/fuse)`: ~1 core of kernel spin, still no `SIGBUS`). `git` mmaps its index, `rustc`/`rust-analyzer` map everything - so 4(a) needs an amendment (ticket 31), not a code tweak |

Two more measured facts that constrain the implementation: writes are capped at
**4 KiB per frame** (`max_write=4096`, 4 MiB in 998 frames = 12.8 MB/s) because the
guest server's INIT reply advertises neither `FUSE_BIG_WRITES` nor `FUSE_MAX_PAGES`;
and **one `println!` per relayed request costs ~90 µs per frame**, so any later
measurement must say whether the measured path logged.

## Resolution

The contract, settled 2026-11-20 (bob):

1. **The host sees the workspace at the launch directory**, and it is served
   *continuously*: by the task VM while one runs, and by a **workspace server VM**
   (`cang workspace serve`) in between - one kernel touches the btrfs image at a
   time, and the image never goes unwatched by the host.
2. **Authority is the image; the host's view may be stale.** The promise is written
   down: a host reader can see an older version of a file until it reopens it, and
   **no host `inotify` event is generated for a guest-side write** (the FUSE
   protocol can invalidate cache, `FUSE_NOTIFY_INVAL_INODE`/`INVAL_ENTRY`, but it
   cannot synthesise an `inotify` event). Host-issued writes are coherent because
   they arrive through the mount.
3. **Concurrency is forbidden, not arbitrated**: while a task VM runs, the host
   mount is mounted **read-only**. A future CRDT is named as the direction and is
   out of scope here.
4. **Failure is `EIO`**: the servicing guest dying makes host syscalls fail with
   `EIO`, the stale mount stays until unmounted, and `cang` prints a named error.
   No queueing, no silent revert to the host tree.
5. **Acceptance** for the implementation: a host-side edit visible in the guest and
   a guest-side write visible on the host after reopen, with real tools (`git`
   status/add, an editor save, pi's file tools), plus the read-only refusal while a
   task VM holds the image, plus the `EIO` arm. Measured, not asserted.

Carried: ticket 29 (the server VM), ticket 30 (the invalidation watcher), and the
measured op set above is what the relay must serve.
