---
label: wayfinder:research
title: Zero-message open - removing the OPEN/RELEASE round trip, the opcode mix of a real build, and what it costs
status: done
blocked_by: []
claimed_by: pi zero-message-open child (2026-10-26)
---

# 23 - Zero-message open: design, opcode histogram, A/B, correctness

Ticket 19 asked for the biggest remaining lever on a rustc build over virtio-fs:
answer `FUSE_OPEN`/`FUSE_OPENDIR` with `-ENOSYS` so the kernel stops sending both
them and their releases, and serve what the missing handle carried from a
per-*inode* descriptor instead. Note 22 priced the lever from the kernel side and
corrected the ticket's transition count; this note is the implementation, its
measurement, and the opcode histogram the map had never had.

> **Correction (2026-10-26, independent re-measurement).** Section 8 re-ran this
> whole arm matrix on this worktree. It reproduces the effect - same opcode mix,
> same request counts, same probe ratios - but **not** the seconds below (2.42x
> measured, not 2.66x), and it settles the "below the no-FUSE floor" question:
> the 232 s floor on record is a lever-OFF measurement, and a lever-ON share
> beats it by 34%. Read section 8 before quoting any number in sections 1-7.


## Verdict

1. **Ship it, on by default.** A full synthetic rustc arm (2401 crates, offline)
   went **372.8 s -> 140.2 s mean, 2.66x**, on four runs per side interleaved so
   host drift hit both arms (per-pair ratios 2.72 / 2.69 / 2.56 / 2.66 - the
   spread inside each side is 18%, the gap between the sides is 166%). The `open`
   probe cell went **1788 -> 4788 ops/s at 1 thread** (p50 542 -> 165 us, -70%)
   and **6714 -> 18563 ops/s at 64**; `stat`/`statwarm` are unchanged, which is
   the control that says the probe still measures what it did.
   **[Corrected in section 8: the ratio holds at 2.42x, not 2.66x, the absolute seconds are not reproducible, and 140 s is not below any same-state floor - read section 8 before quoting this paragraph.]**
2. **The opcode mix was the missing number, and it reframes the lever.** At the
   1,000,000th request of a real build the server had served OPEN 7.90% +
   RELEASE 8.48% + OPENDIR 0.75% + RELEASEDIR 0.75% - **17.9% of every request is
   a round trip that zero-message open deletes outright** - and READ 35.76%. With
   the lever on, the same build issues **no OPEN/RELEASE/OPENDIR/RELEASEDIR at
   all** and READ falls to **3.26%**, because keeping the page cache across opens
   stops rustc re-reading every `.rlib` and `.rmeta` over FUSE. The second half
   was not in the ticket's model and is the larger half of the win.
3. **The requests are gone in bulk, not shaved.** Milestone timestamps place a
   full build at **~3.6 M requests off vs ~1.7 M on** (interpolated: 3 M at
   07:40:24 of a 419 s build; 1 M at 07:33:25 of a 158 s build) - wall time
   tracked the request count almost exactly.
4. **It costs exactly one semantic: close-to-open consistency for in-place host
   writes.** A file the guest has read stays cached after the open that read it,
   so a host-side rewrite becomes visible when the attributes refresh
   (`attr_timeout`, 5 s) instead of at the next `open`. Measured: the guest saw
   the host's rewrite **after 6 s, not at 1 s**, where the arm without the lever
   saw it immediately. Files the host replaces by rename (every editor, `git
   checkout`, `sed -i`) are new inodes and unaffected, and a host-added **file in
   a listed directory is visible immediately** in both arms. The kernel's
   `ff->open_flags` default of `FOPEN_KEEP_CACHE | FOPEN_CACHE_DIR`
   (`fs/fuse/file.c:155-156`) is what buys the win and what costs the window.
5. **`readdirplus` falls out of the ceiling entirely**: 129 -> 4446 ops/s at one
   guest thread (34x, p50 8.2 ms -> 215 us), 277 -> 20832 at 64. The server served
   **0.0%** of a scan cell's CPU: repeated scans of an unchanged directory are
   now answered from the guest's own directory cache. The pool of note 21 was
   built for this class; this supersedes it for repeated scans (the pool is still
   what makes the *first* scan of a huge directory parallel).
6. **Data integrity is unaffected** - create/append/truncate/`O_TRUNC`-on-open/
   seek-overwrite/`cp -a` (symlinks included)/`git init && add && commit` twice/
   chmod/utime/rename/unlink/rmdir, all byte-identical between the arms, with the
   host seeing every write. The `O_TRUNC` hole the ticket warned about is real and
   had to be closed: this fork **does** negotiate `FUSE_ATOMIC_O_TRUNC` today, so
   the flag has to leave the `supported` set with the lever, or truncate-on-open
   would open the file and keep its contents.

## 1. The design

Both halves are per-device and switchable per launch, so an A/B needs no rebuilt
image (the guest rootfs is the same in both arms).

**Fork** (`deps/libkrun`, branch `wayfinder/fs-worker-pool` on top of `cang`,
uncommitted in the worktree at the time of writing):

| file | what |
|---|---|
| `src/devices/src/virtio/fs/linux/passthrough.rs` | `Config::zero_message_open` (:429) and `PassthroughFs::zero_message_open` (:580); `InodeData::zero_open` (:63) holding two lazily opened, per-inode descriptors (`ZeroOpenFiles`, :76) - one opened `O_RDONLY`, one `O_WRONLY`, because a read must not force a writable descriptor and `FLUSH` on a read-only file must not fail; `handle_of` (:652) resolves `fh != 0` exactly as before and `fh == 0` through the inode; `any_handle_of` (:714) for the requests that only need *a* descriptor (`FLUSH`, `FSYNC`, `LSEEK`). Guards: `opendir` ENOSYS (:1291), `open` ENOSYS (:1397), `create` returns no handle, `do_release` accepts a zero handle (:912). Call sites moved onto the helpers: `read` (:1517), `write` (:1547), `do_readdir` (:772), `flush` (:1907), `fsync` (:1927), `fallocate` (:2191), `lseek` (:2218), `copyfilerange` (:2243, :2248). |
| `src/devices/src/virtio/fs/server.rs` | `ZERO_MESSAGE_OPEN`/`ZERO_MESSAGE_OPENDIR` added to `supported` (:1022) and `ATOMIC_O_TRUNC` made conditional on the lever being off (:1042); `getattr`/`setattr` treat `FH == 0` as "no handle" (:338, :370) - the kernel sets `FATTR_FH` with `ff->fh == 0` for a file it has open (`fs/fuse/dir.c:2210-2214`), and resolving that as a handle would fail every truncation; the opcode histogram (`OPCODE_SLOTS`, :76; milestones :112; `count_opcode` :148; `log_opcode_histogram` :161; `opcode_name` :1819). |
| `src/devices/src/virtio/fs/worker.rs` | `FsServer::set_zero_message_open` (:220) / `log_opcode_histogram` (:229) / `set_opcode_stats` (:238), `FsWorker::opcode_stats` (:365), teardown report (:550). |
| `src/devices/src/virtio/fs/device.rs` | `Fs::set_zero_message_open` (:188), `Fs::set_opcode_stats` (:200). |
| `src/libkrun/src/api/device_builders.rs` + `bindings/.../ffier-krun.json` + `include/libkrun.h` | `FsDevice::set_zero_message_open` (:703) and `set_opcode_stats` (:713); regenerated with `make gen-libkrun-bindings FEATURE_FLAGS="--features blk,net,gpu,input,timesync"` (`ffier-gen-c-header` < 1 s; the committed diff is exactly the two new symbols). |
| `CANG.md` | the fork-only-work entries. |

**cang**: `LibkrunApi::fs_device_set_zero_message_open` / `..._set_opcode_stats`
(`crates/cang-libkrun/src/api.rs`, `linked.rs`), the launch-time defaults
`CANG_FS_ZERO_MESSAGE_OPEN` (true) and the env overrides
`CANG_FS_ZERO_MESSAGE_OPEN` / `CANG_FS_STATS`
(`crates/cang-libkrun/src/lib.rs`), and the two calls in
`crates/cang/src/runtime/vm/libkrun/launcher.rs` right after
`fs_device_set_worker_threads` (+ the fake's two `Call`s and the exact
call-order test).

### Why each piece is necessary, from the kernel's side

- **`-ENOSYS` is the grant.** `fuse_file_open()` sends `FUSE_OPEN` only while
  `!fc->no_open`, and `-ENOSYS` sets it (`fs/fuse/file.c:139`, `:166-178`);
  `fuse_file_put()` then completes every `FUSE_RELEASE` locally (`:110-113`), and
  an opendir's `ff->args` is freed outright (`:170-173`), which is why
  `FUSE_READDIRPLUS` arrives with `fh == 0` and no release follows.
- **`FUSE_ATOMIC_O_TRUNC` has to go with it, and this fork had it.** This is the
  one place note 22's correction overshot: the fork's `supported` set *does*
  include `ATOMIC_O_TRUNC`, the guest negotiated it, and truncate-on-open
  therefore rode the `OPEN` request (`fs/fuse/file.c:34-35`, `:257-295`) - the
  kernel then keeps its own size and page cache in step and sends no `SETATTR`.
  With no `OPEN` the truncation would silently not happen. Dropping the flag puts
  the VFS back on the `SETATTR` path (`copy_file_range` of the open: the
  truncation is one extra request, not two), and the correctness arm's
  `printf 'z\n' > t` over a 15-byte file lands at 2 bytes in both arms.
- **`FUSE_CREATE` must answer without a handle.** `fuse_file_put`'s local-release
  branch keys on `fc->no_open`, not on where the handle came from, so a handle
  handed out by a create reply is *never* released: one leaked descriptor and one
  leaked map entry per created file. Returning `fh = 0` from `create` puts new
  files on the same inode-descriptor path as everything else.
- **The descriptor lifetime moves to the inode**, which is the one real risk
  (`FORGET`/`destroy` remove the inode, dropping the `Arc<InodeData>` and closing
  what it holds). Two slots rather than one, and `O_NONBLOCK` on the lazy open:
  the open now happens outside the guest's `open(2)`, so a FIFO in the share would
  otherwise block a server thread with no request left for the guest to
  interrupt.
- **`fh == 0` is not handle zero.** `next_handle` starts at 1, so 0 is free to mean
  "none", and the server has to say so for `GETATTR_FH`/`FATTR_FH` too.

### The instrument

`CANG_FS_STATS=1` makes the server count requests per opcode (one relaxed
increment on the request path) and log the histogram at 5 k/20 k/100 k/300 k/1 M/
3 M/10 M/30 M/100 M requests and when the device stops. It has to be read from the
**manager helper's** stderr log, not from cang's: the VMM and its device threads
run inside `cang internal libkrun-network-enter` under `unshare`, whose stderr cang
captures to `<state-root>/tasks/<task>/helper.stderr.log` and deletes at teardown.
During these arms that file was tailed for the VM's lifetime (the capture script is
`/home/dev/cang-bench-zmo/fullone.sh`); a launch that wants the histogram after the
fact will need either a file sink or a cang-side copy of that log. The teardown
report does fire on a normal `cang kill` - it is how the default-path check above
was read (`opcode histogram [teardown]: 157635 requests served`) - so the tail is
only needed for a VM that is killed hard.

## 2. The opcode mix of a real build (the table the map was missing)

Both columns are the milestone at **1,000,000 requests served**, from one real
`cargo build --offline` of the 2401-crate synthetic workload
(`guest-virtiofs`, 26 vCPUs, image `localhost/cang:latest`).

| opcode | lever off | % | lever on | % |
|---|---|---|---|---|
| READ | 357561 | 35.76 | 32646 | 3.26 |
| LOOKUP | 246600 | 24.66 | 540628 | 54.06 |
| RELEASE | 84761 | 8.48 | - | - |
| FLUSH | 83988 | 8.40 | 212052 | 21.21 |
| OPEN | 79018 | 7.90 | - | - |
| GETATTR | 57230 | 5.72 | 29320 | 2.93 |
| WRITE | 16083 | 1.61 | 42321 | 4.23 |
| GETXATTR | 15477 | 1.55 | 40565 | 4.06 |
| READLINK | 9280 | 0.93 | 23393 | 2.34 |
| OPENDIR | 7537 | 0.75 | - | - |
| RELEASEDIR | 7536 | 0.75 | - | - |
| CREATE | 6719 | 0.67 | 17615 | 1.76 |
| READDIRPLUS | 6071 | 0.61 | 10620 | 1.06 |
| READDIR | 5508 | 0.55 | 11323 | 1.13 |
| MKDIR | 4437 | 0.44 | 7706 | 0.77 |
| FORGET | 4014 | 0.40 | 10551 | 1.06 |
| RMDIR | 1995 | 0.20 | 5262 | 0.53 |
| UNLINK | 1988 | 0.20 | 5265 | 0.53 |
| IOCTL | 1399 | 0.14 | 3568 | 0.36 |
| RENAME | 1360 | 0.14 | 3536 | 0.35 |
| SETATTR | 732 | 0.07 | 1826 | 0.18 |
| LINK | 661 | 0.07 | - | - |
| FSYNC | 25 | 0.00 | - | - |
| STATFS | 11 | 0.00 | - | - |
| SETXATTR | 4 | 0.00 | - | - |
| MKNOD, INIT, POLL, RENAME2, opcode 52 | 1 each | | | |

Three facts to carry forward:

- **The OPEN/RELEASE class is 16.4% of a build's requests** (17.9% counting the
  directory pair), which is what a lever that removes two round trips per open is
  up against. `FLUSH` - which zero-message open does *not* remove, by design -
  stays and becomes the second-largest opcode once OPEN/RELEASE are gone.
- **The READ column collapses (35.76% -> 3.26%)**, and that is the page cache, not
  fewer reads: with `FOPEN_KEEP_CACHE` the guest keeps `.rlib`/`.rmeta` pages
  across the constant open/read/close cycle cargo runs over the same files. This
  is the half of the win the ticket did not model, and it is why a 17.9%
  request-count reduction turns into a 2.66x wall-clock win.
- **`LOOKUP` dominates both mixes** (24.7% / 54.1%) and is untouched by this
  lever. Whatever the next ticket is, it should read this column first: the fork's
  `lookup` is still `openat(O_PATH) + statx` per entry
  (`linux/passthrough.rs:1080-1130`), and note 19 already priced that as the
  server's per-entry cost.
- `opcode 52` = `FUSE_STATX` (newer than this fork's `Opcode` enum): the kernel
  probes it once, gets `-ENOSYS`, sets `no_statx`, and falls back to `GETATTR` -
  which is why the `stat` cell costs one `GETATTR`, not one `STATX`.

## 3. The probe A/B

`tools/rust-compile-bench/concurrency-scaling.sh --threads "1 16 64" --rounds 3
--files 4096` (4096 x 4 KiB files, 26 vCPUs, one cell = 2 s), one VM per arm, the
same binary both sides, differing only in `CANG_FS_ZERO_MESSAGE_OPEN`.

| fs | mode | thr | ops/s off | ops/s on | ratio | p50 off us | p50 on us |
|---|---|---|---|---|---|---|---|
| virtiofs | open | 1 | 1788 | **4788** | 2.68x | 541.7 | **165.0** |
| virtiofs | open | 16 | 6349 | **18329** | 2.89x | 4096 | **848** |
| virtiofs | open | 64 | 6714 | **18563** | 2.76x | 16384 | **4096** |
| virtiofs | read | 1 | 1593 | 3022 | 1.90x | 632.3 | 322.7 |
| virtiofs | read | 16 | 4609 | 8233 | 1.79x | 4096 | 2048 |
| virtiofs | read | 64 | 4863 | 8269 | 1.70x | 16384 | 8192 |
| virtiofs | stat | 1 | 6191 | 6268 | 1.01x | 133.0 | 144.0 |
| virtiofs | stat | 16 | 18645 | 16507 | 0.89x | 809.3 | 880.3 |
| virtiofs | stat | 64 | 20057 | 18571 | 0.93x | 4096 | 4096 |
| virtiofs | statwarm | 1 | 378701 | 377417 | 1.00x | 2.0 | 2.0 |
| virtiofs | statwarm | 64 | 7096241 | 6644369 | 0.94x | 3.0 | 3.0 |
| virtiofs | readdirplus | 1 | 129 | **4446** | 34.5x | 8192 | **215.3** |
| virtiofs | readdirplus | 16 | 272 | **10849** | 39.9x | 87381 | **2048** |
| virtiofs | readdirplus | 64 | 277 | **20832** | 75.2x | 349525 | **4096** |
| shm (control) | open | 1 | 320735 | 281352 | 0.88x | 3.0 | 3.0 |
| shm (control) | read | 1 | 207839 | 185796 | 0.89x | 5.0 | 5.0 |
| shm (control) | stat | 1 | 485772 | 429893 | 0.89x | 2.0 | 2.0 |
| shm (control) | statwarm | 1 | 489300 | 438892 | 0.90x | 2.0 | 2.0 |
| shm (control) | readdirplus | 1 | 2077 | 1949 | 0.94x | 467.7 | 497.3 |

The `shm` control moved **-6% to -12%** between the two runs, i.e. the VM carrying
the lever ran slightly *slower* on identical tmpfs work (host load, not the
lever); every virtiofs gain above is measured against that headwind, and the
`stat`/`statwarm` rows sit inside it, which is what makes them a control rather
than a regression. All cells passed `errs=0`, and the scan cell really scanned:
`entries=34148769` at 1 thread in the lever-on arm (8331 scans x 4096 entries) vs
`entries=1041146` in the lever-off arm - the 34x is the directory cache, not an
empty directory.

Server CPU per cell (`fs_worker%` of one core): `open@1` 13.4 -> 8.2, `open@64`
27.5 -> 24.7, `readdirplus@1` **73.0 -> 0.0**, `readdirplus@64` 3.7 -> 0.1 (the
pool threads did the scans in the lever-off arm - 392% of a core at 16/64 threads
- and nothing at all in the lever-on one).

**Caveat on absolute numbers**: the worktree carries an uncommitted `lookup`
fast-path change that was not mine (`statx_at` for already-known inodes, plus a
temporary `fsdbg` counter in `lookup`). It is present in *both* arms, so the A/B
is apples to apples, but the lever-off `open` cell (541.7 us p50) is not directly
comparable to note 20's 934 us.

## 4. The full arm

`tools/rust-compile-bench/run.sh --arms guest-virtiofs --runs 1`, 2401-crate
synthetic workload, `cargo build --offline`, interleaved `off,on,on,off,on,off`
plus one later `on,off` pair (the second pair is the run that captured the `on`
histogram; all four of its timings are in the table).

| arm | run | bench (s) | fs_worker% of a core |
|---|---|---|---|
| off (ZMO=0) | 1 | 362.2 | 23.5 |
| on (ZMO=1) | 1 | **133.0** | 27.5 |
| on | 2 | **132.0** | 27.6 |
| off | 2 | 355.5 | 23.6 |
| on | 3 | **138.1** | 27.6 |
| off | 3 | 354.2 | 24.0 |
| on | 4 | **157.7** | 27.6 |
| off | 4 | 419.4 | 22.6 |

| side | n | mean | min | max | spread |
|---|---|---|---|---|---|
| off | 4 | 372.8 s | 354.2 | 419.4 | 18% |
| on | 4 | **140.2 s** | 132.0 | 157.7 | 19% |

Per adjacent pair: 362.2/133.0 = **2.72x**, 355.5/132.0 = **2.69x**,
354.2/138.1 = **2.56x**, 419.4/157.7 = **2.66x**. Mean ratio **2.66x**, median
2.66x. The spread within a side (18-19%, host drift: the last pair ran while this
session's own builds were finishing) is an order of magnitude smaller than the
gap between the sides, so no reading of these numbers lands anywhere near parity.

Two things this arm does *not* reproduce and must not be read as: the lever-off
side here (354-419 s) is slower than note 21's recorded 298-323 s band, so the
comparison to make is **within this table**, not against the recorded baseline.
And the server's *relative* CPU share went up (23.5 -> 27.6% of a core) while its
absolute CPU went **down** (0.235 x 362 = 85 s vs 0.276 x 140 = 39 s): the build
finished in less than half the time, so the same thread is a larger fraction of a
shorter run. Note 21 used the dispatcher's share as evidence the pool was engaged;
here the same metric would tell the opposite story, so don't use it that way.

Request totals for the same build, interpolated from the milestone timestamps
(300 k at 07:35:23 -> 3 M at 07:40:24 for the lever-off run; 300 k at 07:32:26 ->
1 M at 07:33:25 for the lever-on run, both extrapolated to the build's end):
**~3.6 M requests off vs ~1.7 M on**, i.e. wall time tracks request count almost
linearly (2.1x fewer requests, 2.66x less time). Every request-removing lever in
this map can be priced against that line now.

## 5. Correctness (the part that matters more than the timing)

Both arms ran the same guest payload in a fresh VM
(`/home/dev/cang-bench-zmo/correctness.sh`); the transcript is the guest's own, read
back from the share.

| check | lever off | lever on |
|---|---|---|
| `echo a > f; echo b >> f` | `content: a/b/` | `content: a/b/` |
| `: > f` (truncate in place) | `after-truncate-size: 0` | `after-truncate-size: 0` |
| `O_TRUNC` open of a 15-byte file | `size: 2 content: z` | `size: 2 content: z` |
| 200 appends | `lines: 200 size: 1492 tail: line200` | same |
| `dd seek=4 conv=notrunc` | `s: 0123XX6789` | same |
| `cp -a` tree in and out (symlink) | `entries: treeout ... treeout/link`, `contents: xy link: a` | same |
| `git init && add && commit` x2 | `commits: 2 x: hi/more/`, `status-lines: 0` | same |
| chmod / utime / rename / unlink / rmdir | `mode: 640`, `mtime: 981173106`, `renamed: m2`, `unlinked: gone`, `rmdir-ok` | same |
| 300 small files | `files: 300 bytes: 1200` | same |
| host sees every write | `-- f size: 0`, `-- t size: 2 content: z`, `-- ap lines: 200 tail: line200`, `-- repo log: ... second|first`, `-- repo status: 0 lines`, `-- many: 300 files` | same |

And the two coherence probes, which are where the arms differ:

| probe | lever off | lever on |
|---|---|---|
| guest `cat` a file, **host** rewrites it in place, guest `cat` again ~1 s later | `coherence-immediate: V2-host-rewrite` | `coherence-immediate: V1-original` (stale) |
| the same after `sleep 6` | `coherence-after-attr-timeout: V2-host-rewrite` | `coherence-after-attr-timeout: V2-host-rewrite` |
| 2000-line file, guest sums it, host rewrites in place, guest sums again ~1 s later | `bigmap-after: lines: 1743 sum: 2948048277` (fresh) | `lines: 2000 sum: 115710639` (stale) |
| host adds a file to a directory the guest already listed, guest lists again ~1 s later | `listing-immediately-after-host-add: one two` | `listing-immediately-after-host-add: one two` |

So: data written through the share is correct in both arms, and the one behavioural
difference is **host-side in-place rewrites of a file the guest has already read**
being invisible for up to `attr_timeout` (5 s). Directory contents are *not*
affected: a host-added entry appears immediately in both arms, which also means the
`FOPEN_CACHE_DIR` half of the open-flags default is invalidated by the directory's
own attribute changes rather than blindly retained.

## 6. Gates

| gate | result |
|---|---|
| `nix develop . -c cargo fmt --check` | clean |
| `nix develop . -c cargo clippy --all-targets --all-features -- -D warnings` | clean |
| `nix develop . -c cargo test` (whole workspace, incl. `cang-repository-tests` and the launcher's exact call-order test) | pass |
| `cargo test -p krun-devices` in the fork | 101 passed |
| `nix build .#cang --override-input libkrun-src path:$PWD/deps/libkrun` | builds; `--version` reports 0.14.0 |
| the **default** launch path (no `CANG_FS_ZERO_MESSAGE_OPEN` set) | verified from the server's own counter: a 157,635-request guest payload shows `OPEN 3`, `RELEASE 1`, `OPENDIR 1` (the one-time probes) and no other open/release traffic - the shipped default really is the lever-on arm, not just a const |

## 7. What this does not settle, and what to do next

- **The coherence window is the price, and it is inherent.** `FOPEN_KEEP_CACHE` is
  the kernel's default once there is no `OPEN` reply to carry open flags
  (`fs/fuse/file.c:155-156`); a server cannot ask for close-to-open per open
  without an open. Options, in order of cost: document it (done, in
  `crates/cang-libkrun/src/lib.rs` and the ticket's map); shorten `attr_timeout`
  on the share (every file's attrs then revalidate more often, eating requests);
  or split the device so the image rootfs - which the host never rewrites in place
  - keeps the win while `/workspace` keeps close-to-open (a cang-side change: two
  Fs devices today would be two request queues and two pools, so it is not free).
- **`CACHE_SYMLINKS` is still unacked** (note 22's one surviving one-line item:
  `fuse.rs:145` defines the bit, the fork never ORs it into `supported`). Not
  measured here; a symlink-heavy build would save one `READLINK` per repeated
  path.
- **The histogram's sink is awkward.** `CANG_FS_STATS=1` writes to the manager
  helper's stderr, which cang deletes at teardown; reading it needs a tail during
  the run (what these arms did). A file sink under the task dir, copied out before
  teardown, would make it a first-class diagnostic.
- **`LOOKUP` is now the biggest opcode in both mixes** (24.7% / 54.1%) and nothing
  in this map has priced a cheaper lookup yet - the natural next ticket.
- **The pool's readdirplus ceiling is superseded for repeated scans** (0.0% server
  CPU in the scan cell). The pool is still what makes one first scan of a huge
  directory parallel, so keep it; but note 21's 154 -> 325 ops/s headline is now
  the *smaller* number for that class.

## 8. Independent re-measurement (2026-10-26, zmo-verify session)

This section was added by a second session whose job was to verify - not assume -
the claim above, because the headline number (140.2 s) was *below* the no-FUSE
floor on record (232 s for the guest-local `/dev/shm` arm), which is impossible
for a virtio-fs arm unless one of the two measurements measured something else.
Both were re-measured. **The lever is real; the seconds in sections 1-4 are not
reproducible; and the floor paradox was an artifact of comparing a lever-ON
number against a lever-OFF floor.**

### 8.1 Method

- **One binary, both arms.** `nix build .#cang --override-input libkrun-src
  path:$PWD/deps/libkrun -o result-on` ->
  `/nix/store/9fh66is6lfc4azrd4pfygip1lsgav5pg-cang-0.14.0` (`bin/cang` md5
  `efd2b7a4e3a5a5634882728b41799697`, `cang 0.14.0`, carries
  `krun_fs_device_set_zero_message_open`). The lever was toggled with the knob
  the change added - `CANG_FS_ZERO_MESSAGE_OPEN=0|1`, plus `CANG_FS_STATS=1` for
  the histogram - so no code was patched and no second build was needed.
- **The workload is this map's**, byte-identical to the one note 23 used
  (`diff -rq` silent against `/home/dev/cang-bench-zmo/workload-synthetic`):
  2401 crates, 16801 `.rs` files, same generator defaults.
- `JOBS=26`, not the 8 the assignment proposed: every recorded number in this map
  (299-311, 232, 372.8, 140.2) is a `CARGO_BUILD_JOBS=26` measurement, and the
  guest has 26 vCPUs (read from inside a guest: `nproc` 26, `MemTotal` 15.6 GiB,
  `/dev/shm` 7.9 GiB). Any other value would have made the comparison to the
  recorded seconds meaningless.
- **Interleaved, one VM at a time, fresh workspace per run.** Round 1
  `host -> guest-local -> virtiofs-off -> virtiofs-on`; round 2 the reverse
  (`on -> off -> local -> host`); then two more interleaved virtio-fs pairs and
  two guest-local pairs. Each run is one `run.sh --arms <one> --runs 1`
  (`prepare_arm` deletes the arm dir; the inner script `rm -rf target`s before
  starting the clock, with `CARGO_INCREMENTAL=0` and `RUSTC_WRAPPER` unset).
- **Conditions recorded per run**: `cang ps` empty before every arm, no co-tenant
  VM at any point; host steal 0.9% averaged over the whole session (`/proc/stat`
  sampled at 5 s); `MemAvailable` 18-20 GB; `load1` 0.9-12 (the high values are
  the arm that had just ended). Two strays from other sessions ran throughout at
  ~6-7% of one core each (a leftover `mon-threads.sh` and an idle pi).
- **Each arm proved it ran its own lever** from the fork's opcode histogram,
  tailed out of the manager helper's stderr log while the VM lived (that log is
  deleted at teardown, so a post-hoc read finds nothing):
  - lever **on**: `OPEN 3`, `RELEASE 1`, `OPENDIR 1`, `RELEASEDIR 0` at 1,000,001
    requests - the one-time probes only;
  - lever **off**: `OPEN 244,157-248,131`, `RELEASE 264,438-269,510`,
    `OPENDIR ~24k`, `RELEASEDIR ~24k`.
  A `guest-local` arm is not exempt from that check - see 8.4.
- **Each arm did the work**: 2401 `Compiling` lines and **0** `Fresh` lines in
  every surviving inner log; `target/debug/deps` = 7202 entries (2401 x
  `.rlib`/`.rmeta`/`.d`) totalling 182-183 MB; `Finished 'dev' profile` present.
  The host arm and both virtio-fs arms produced the same artifact count.

### 8.2 The four arms, two interleaved rounds (bench seconds)

| arm | r1 | r2 | mean (n) | min | max | spread |
|---|---|---|---|---|---|---|
| host (native, the machine's yardstick) | 140.9 | 119.5 | **130.2** (2) | 119.5 | 140.9 | 16% |
| guest-local `/dev/shm`, lever ON (the floor) | 164.9 | 125.8 | **131.5** (4) | 111.0 | 164.9 | 41% |
| guest-virtiofs, lever OFF | 563.0 | 383.6 | **427.1** (4) | 366.2 | 563.0 | 46% |
| guest-virtiofs, lever ON | 185.3 | 185.1 | **176.0** (4) | 162.6 | 185.3 | 13% |

The extra rounds behind the n=4 means are `guest-local` 124.4 / 111.0 (on) and
337.7 / 313.8 (off), and `virtiofs` 162.6 / 171.0 (on) and 395.5 / 366.2 (off).

The shape of the noise is itself a result: **the lever-on arm is the stable one**
(185.3 vs 185.1, 0.1%), while the lever-off arm swings 366-563 s. An arm whose
time is dominated by ~3.6 M round trips inherits every hiccup on the way; an arm
that serves ~1.5 M mostly cached requests does not.

### 8.3 The two ratios, and the comparison that reconciles the sessions

| ratio | this session | note 23 |
|---|---|---|
| virtiofs ON / virtiofs OFF (mean of 4 interleaved pair ratios: 3.04, 2.07, 2.43, 2.14) | **2.42x** (mean of means 2.43x) | 2.66x |
| virtiofs ON / guest-local floor, both same-state | **1.34x** (pairs 1.12, 1.47, 1.31, 1.54) | below the floor |
| virtiofs ON / host | **1.35x** | 1.35x |
| virtiofs OFF / host | **3.28x** | 3.58x |
| guest-local floor / host | **1.01x** | (232/104.2 = 2.23x, lever off) |

**The absolute seconds are session-scoped, and the host arm proves it.** The host
arm builds the same tree natively with the devshell toolchain - no VM, no FUSE,
and no line of this change in its path - and it took **130.2 s here against the
104.2/104.7 s recorded in note 15**, i.e. this box was ~1.25x slower for the whole
of this session (the same factor appears in every arm: `guest-local` moved
232 -> 325.8 s, 1.4x). Normalised to the host arm, the two sessions agree to the
digit on the side that matters: **ON/host 1.35x here and 1.35x in note 23**,
OFF/host 3.28x vs 3.58x. So the defensible statement of the change is "2.4x
(2.4-2.7x across sessions), ON is 1.35x a native build", not an absolute second
count.

### 8.4 Why 140 s looked like it was below the no-FUSE floor (it was not)

The floor on record (232 s) is **a lever-off measurement**: it predates the lever,
and the lever is not confined to `/workspace`. The guest mounts virtio-fs twice
(`/proc/mounts` in a cang guest: `/dev/root / virtiofs` and
`none /workspace virtiofs`), so a 2401-crate build launches `rustc` and `ld` ~2401
times and every one of those launches reads the same binaries, `std` rlibs and
linker inputs off that share. Cutting OPEN/RELEASE there is worth as much to a
tmpfs build as to a `/workspace` build.

Measured directly, by running the guest-local arm both ways (same binary, only
`CANG_FS_ZERO_MESSAGE_OPEN` differs):

| guest-local `/dev/shm` arm | OPEN served | RELEASE served | bench (s) |
|---|---|---|---|
| lever OFF | 87,871 / 89,374 | 87,316 / 88,876 | 337.7, 313.8 -> **325.8** |
| lever ON | 3 | 1 | 124.4, 111.0 -> **117.7** (mean of 4: 131.5) |

The lever alone is **2.48x** (means) to **2.77x** (pairs) on the arm with no FUSE
in its `target/` path. So the floor that belongs next to a lever-on number is
~131 s (this session) / ~118 s (the later pairs), **not 232 s** - and virtiofs-ON
at 176.0 s sits **1.34x above it**, which is the ordering the hardware requires:
no share can beat the same build with no share in front of it. Note 23's "140.2 s
is below 232 s" compared a lever-ON number against a lever-OFF floor, and note
24's pricing of the remaining levers (which is built on that 232 s floor) inherits
the same error.

### 8.5 The probe cells, re-run both ways

`concurrency-scaling.sh --threads "1 16 64" --rounds 3 --files 4096 --modes "open
stat statwarm readdirplus"`, one boot per arm, same binary, `errs=0` everywhere:

| fs | mode | thr | ops/s off | ops/s on | ratio | p50 off us | p50 on us |
|---|---|---|---|---|---|---|---|
| virtiofs | open | 1 | 1892 | **5454** | 2.88x | 518.7 | **135.3** |
| virtiofs | open | 16 | 6551 | **19203** | 2.93x | 4096 | **799.3** |
| virtiofs | open | 64 | 6789 | **19115** | 2.82x | 16384 | **4096** |
| virtiofs | stat | 1 | 6277 | 7132 | 1.14x | 129.3 | 129.3 |
| virtiofs | statwarm | 1 | 451481 | 463280 | 1.03x | 2.0 | 2.0 |
| virtiofs | readdirplus | 1 | 131 | **4927** | 37.6x | 8192 | **195.3** |
| virtiofs | readdirplus | 64 | 327 | **23361** | 71.4x | 262144 | **4096** |
| shm (control) | open | 1 | 341068 | 338458 | 0.99x | 3.0 | 3.0 |
| shm (control) | statwarm | 1 | 496440 | 463280 | 0.93x | 2.0 | 2.0 |
| shm (control) | readdirplus | 1 | 2102 | 1949 | 0.93x | 465.0 | 497.3 |

The `shm` control moved 0.93-0.99x, i.e. the two VMs ran under the same conditions
(note 23's control happened to sit 6-12% *against* the lever-on arm; this one is
neutral). The `open` claim is confirmed at least as strongly as stated - p50
518.7 -> 135.3 us (note 23: 541.7 -> 165.0) - and `stat`/`statwarm` sit at ~1.0x,
which is the control that the probe still measures what it did.

### 8.6 What reproduced exactly

- **The opcode mix.** Lever off, at 3,000,000 requests: READ 35.85% (note 23:
  35.76), LOOKUP 22.24 (24.66), RELEASE 8.97 (8.48), FLUSH 8.81 (8.40), OPEN 8.27
  (7.90), GETATTR 6.18 (5.72), OPENDIR 0.81 (0.75), RELEASEDIR 0.81 (0.75) ->
  **18.9% of every request is the open/release class** (note 23: 17.9%). Lever on,
  at 1,000,000 requests: LOOKUP 54.00 (54.06), FLUSH 21.23 (21.21), WRITE 4.24
  (4.23), GETXATTR 4.06 (4.06), **READ 3.28 (3.26)**, and no
  OPEN/RELEASE/OPENDIR/RELEASEDIR beyond the three one-time probes.
- **The request totals**, interpolated from the milestone timestamps the way note
  23 did: off 3.59-3.63 M, on 1.47-1.63 M - ~3.6 M -> ~1.5-1.6 M (note 23:
  3.6 M -> 1.7 M). Wall time tracks it: 2.2-2.3x fewer requests, 2.4x less time.

### 8.7 What to quote from now on

1. **Effect: ship it.** 2.42x on this session's interleaved pairs (2.07-3.04 per
   pair), 2.4-2.7x across sessions; ON = 1.35x a native build of the same tree.
2. **Do not quote 372.8 s / 140.2 s as measurements of anything but that
   session**, and do not quote 140.2 s as "below the floor": with both sides
   measured together, virtiofs-ON is 1.34x the same-state floor.
3. **The 232 s floor in notes 15, 18, 20 and 24 and in `MAP.md` is a lever-off
   number** - and a faster-machine number (the same arm measures 325.8 s here).
   Anything priced against it is priced against a floor that a lever-on share
   beats by 34%.
4. The lever pays on the **rootfs** share too, not just `/workspace`, so the
   workspace-only cache of ticket 16 would collect only part of this win.

### 8.8 Artifacts

| what | where |
|---|---|
| arm driver (interleaved, per-run logs, histogram capture) | this session's scratch: `arms.sh`, `arms2.sh`, `phaseA.log`, `phaseB.log`, `phaseC.log` |
| per-run table (seconds, deps, target size, OPEN/RELEASE counts, Finished line) | `/home/dev/cang-bench-vfio/jobs26/RESULTS.tsv` |
| fork-side opcode histograms, tailed live | `/home/dev/cang-bench-vfio/jobs26/logs/fork-*.log` |
| per-arm cang/run logs and thread sampler captures | `/home/dev/cang-bench-vfio/jobs26/logs/` |
| probe cells both ways | `<scratch>/conc-zmo{0,1}.log`, `/home/dev/cang-bench-vfio/conc-zmo{0,1}/` |
| guest facts (vCPUs, RAM, `/dev/shm`, mounts) | `/home/dev/cang-bench-vfio/facts-cangfacts/facts.txt` |
| the binary behind every arm | `/nix/store/9fh66is6lfc4azrd4pfygip1lsgav5pg-cang-0.14.0` |

## Sources

| what | where |
|---|---|
| ticket | `docs/wayfinder/workspace-virtiofs-performance/tickets/19-zero-message-open.md` |
| the kernel mechanics this implements | `docs/wayfinder/workspace-virtiofs-performance/notes/22-guest-side-round-trip-reduction.md` §2.1, `fs/fuse/file.c:110-113,139,154-178,257-295`, `fs/fuse/dir.c:1430-1435,2210-2214` (v7.2, `git.kernel.org` `plain/?h=v7.2`) |
| the pool it supersedes for scans | `notes/21-worker-pool-results.md` |
| per-op costs and the probe | `notes/20-concurrency-scaling-probe.md` §1-2 |
| fork source, this change | `deps/libkrun/src/devices/src/virtio/fs/{linux/passthrough.rs,server.rs,worker.rs,device.rs}`, `src/libkrun/src/api/device_builders.rs` |
| cang wiring | `crates/cang-libkrun/src/{api,lib,linked}.rs`, `crates/cang/src/runtime/vm/libkrun/{launcher,tests}.rs` |
| arms and their logs | `/home/dev/cang-bench-zmo/{arms,fullone,correctness,dirstale}.sh`, `logs/`, `summary-synthetic.tsv`, `concurrency-zmo{0,1}/`, `correct-zmo{0,1}/`, `dirstale{0,1}/` |
