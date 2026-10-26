---
label: wayfinder:task
title: Zero-message open - remove the OPEN/RELEASE round trip from every file open
status: open
blocked_by: []
claimed_by: ""
---

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
