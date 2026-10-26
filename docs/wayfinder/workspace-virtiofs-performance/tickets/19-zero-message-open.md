---
label: wayfinder:task
title: Zero-message open - remove the OPEN/RELEASE round trip from every file open
status: done
blocked_by: []
claimed_by: pi zero-message-open child (2026-10-26)
---

## Outcome

**Implemented and measured** - [note 23](../notes/23-zero-message-open-results.md).
A full synthetic rustc arm went **372.8 s -> 140.2 s mean (2.66x)**, four
interleaved runs per side; the `open` probe cell 1788 -> 4788 ops/s at one thread
(p50 542 -> 165 us). The opcode histogram the ticket asked for is in the note: at
the 1,000,000th request of a real build, OPEN 7.90% + RELEASE 8.48% + OPENDIR
0.75% + RELEASEDIR 0.75% = **17.9% of every request is the class this lever
deletes**, and READ fell 35.76% -> 3.26% because the page cache now survives an
open (the half of the win the ticket did not model). `readdirplus` went 129 ->
4446 ops/s at one guest thread, so the pool's headline from note 21 is superseded
for repeated scans.

Two corrections to the ticket's own assumptions, both implemented: this fork
**did** negotiate `FUSE_ATOMIC_O_TRUNC`, so `O_TRUNC` needed the flag dropped from
`supported` or truncate-on-open would have silently kept the old contents; and a
`CREATE` reply must stop handing out a handle once `no_open` is set, because the
kernel then never sends `RELEASE` for it (one leaked descriptor per created file).

The implementation is on by default with `CANG_FS_ZERO_MESSAGE_OPEN=0` as the
hatch, and it owes one user decision:
[ticket 20](20-zero-message-open-coherence-contract.md).

## Question

The pool (ticket 04) fixes the `readdirplus` class. The other half of rustc's
traffic - `open`/`close` of ordinary files - cannot be helped by parallelism,
because note 20 measured that the cost is the round trip itself, ~60% of it spent
in the guest: **three** vCPU exits per round trip (one for the 4-byte MMIO kick, two
for the userspace `INTERRUPT_STATUS` read + `INTERRUPT_ACK` write that serve the
used-ring interrupt - note 22, §3), plus a workqueue hop in
`virtio_fs_requests_done_work`.

The kernel already has a mechanism to halve it, and it needs no new feature bit
negotiation beyond a `-ENOSYS`:

- With `FUSE_NO_OPEN_SUPPORT` (1<<17) / `FUSE_NO_OPENDIR_SUPPORT` (1<<24), the
  server answers the **first** `FUSE_OPEN`/`FUSE_OPENDIR` with `-ENOSYS`; the
  kernel sets `fc->no_open`/`fc->no_opendir` (`fs/fuse/file.c:166-177`) and never
  sends another: the `FUSE_OPEN` block is skipped (`:139-157`) and `FUSE_RELEASE`
  is completed locally (`:112-113`), likewise `FUSE_OPENDIR`/`FUSE_RELEASEDIR`
  (`:140`, `:170-173`). That is **two round trips removed per file open+close**
  and two per directory, at the cost of one probe each.

What it costs on our side: with no `OPEN` there is no `fh`, so `READ`/`WRITE`
arrive with `handle == 0` and the server must serve them from a **per-inode fd**
it keeps open itself (upstream virtiofsd's `zero_message_open` does exactly this,
keeping the file open on the inode). The fork's `read`/`write`/`getattr` paths
resolve handles out of `self.handles` only, so this is the real work.

Do:

1. **Inode-level fd cache** in `linux/passthrough.rs`: on first `read`/`write` with
   `handle == 0`, open the inode's file (the inode already holds an `O_PATH` fd)
   and cache it on the inode entry, with the same lifetime/refcount discipline as
   handles (and the same `O_NOFOLLOW`/`O_CLOEXEC` flags).
2. **`fh == 0` branch** in `read`, `write`, `getattr`, `flush`, `fsync`,
   `lseek`(if implemented): resolve by `inode` instead of `handle`. `FORGET` must
   drop the cached fd.
3. **Advertise it**: answer `-ENOSYS` to `OPEN`/`OPENDIR` (config-gated, so an A/B
   run needs no rebuild of the image - a per-device knob like the pool's).
4. **Correctness checks that matter**: a writable open (`O_TRUNC`) still must reach
   the server (the kernel sends `SETATTR` with size 0 in that case - verify what
   v7.2 does before claiming a win for `O_TRUNC`), `flush`/`fsync` must still see
   the file, and a guest that `mmap`s must not lose coherence (mmap goes through
   `READ`/page cache, so it should be unaffected - prove it).
5. **Measure** with note 20's tool: the `open` cell should lose roughly 60-80% of
   its 934 us, and the full synthetic arm should move. Same A/B discipline as the
   pool: one VM at a time, per-arm logs, kill leftovers.

Sequencing: same files as the pool (`worker.rs`-adjacent, `passthrough.rs`), so it
lands after ticket 04's pool is in and measured, not in parallel.
