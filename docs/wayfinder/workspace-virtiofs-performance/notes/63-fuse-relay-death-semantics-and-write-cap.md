# 63 - FUSE relay, second round: what a host `mmap` really gets when the guest dies, and lifting the 4 KiB write cap

**Measured 2026-11-21 (the evening after note 62)**, same host (28 cores), same
harness `.scratch/fuse-relay/`, same libkrun build (`v2.0.0-cang.22`, `2fad450a`,
`--features ffi,blk`), firmware `libkrunfw-v5.6.2-cang.6`, 2 vCPUs, 1 GiB guest RAM,
one VM at a time. **This note supersedes note 62 §7(b)** - the "an existing `mmap`
livelocks after `kill -9`" verdict was an artifact of the probe, not of the relay.

Raw evidence: `results/death/` (round 1, kept but superseded),
`results/death2/` (round 2, the corrected probe), `results/death3/` (the same with
the pump `SIGSTOP`ped so a reader is genuinely blocked), `results/write/{control4k,lifted1m}`
(ARM B, including each arm's `stats.tsv` and `phases.tsv`).

## Verdict

- **A host fault on an unread page after `kill -9` of the guest gets `SIGBUS`, in
  every arm: 8/8 boots, `wait_status=135` (128+`SIGBUS`), always on the first page
  that was never read (page 1).** There is no livelock, and **no unmount is what
  delivers it**: it arrives 20 ms after the FUSE connection answers the page's READ
  with `-EIO`, and it is still owed to the reader for as long as the connection
  stays silent (measured: blocked, 0 CPU ticks, 6.2 s).
- **No unmount delivers a deterministic error to a fault-blocked reader.**
  `fusermount3 -u` is refused `EBUSY` while the mapping holds the file open;
  `fusermount3 -u -z` succeeds in 22 ms and detaches the name, but the superblock -
  and with it the FUSE connection - survives the open mapping, so the reader stays
  blocked exactly as before (`wchan=request_wait_answer`, 0 CPU ticks over 6.2 s)
  and gets its `SIGBUS` only when the pump resumes and answers `-EIO`.
  `umount -f` is refused (`must be superuser`), and `umount2()` with any of
  `0 / MNT_FORCE / MNT_DETACH / both` returns `EPERM` for an unprivileged user.
- **The v1 death contract therefore is: answer every request `-EIO` immediately and
  never unmount.** Path syscalls get `EIO` and a faulting reader gets `SIGBUS`;
  if the mount is lazily detached instead, path tools stop failing and start seeing
  an *empty* directory (`ls` rc=0, `cat` `ENOENT`) - a silent "your workspace is
  empty" failure, which is worse than `EIO`.
- **The 4 KiB write cap is a guest-side INIT flag, lifted by 8 added lines in a
  vendored `fuse-backend-rs` 0.12.1.** `max_write` 4096 -> 1 048 576,
  `max_pages` 0 -> 256, a 4 MiB `dd conv=fsync` goes from **1024 WRITE frames,
  0.373-0.437 s (9.6-11.2 MB/s)** to **4 WRITE frames, 0.039-0.088 s
  (47.7-107.5 MB/s)**: **8.6-9.6x wall, 256x fewer frames**, no transport change.
  A 64 KiB write goes from 16 frames / ~0.024 s to 1 frame / ~0.021 s, and warm
  `git status` is unchanged (810-811 frames both sides, 0 WRITEs, 0.235 s vs
  0.215 s median - same frame count, so this is not a metadata regression claim).

## 1. Why note 62 §7(b) was wrong

`src/mmapwatch.c` installs a `SIGBUS` handler that sets a flag and **returns**, i.e.
returns to the faulting load. The kernel re-executes the load, faults again, and the
handler runs again - forever. So "the reader never returned and no `SIGBUS` was
observed" cannot distinguish *blocked in the kernel* from *`SIGBUS` being delivered
in a tight loop*. The two numbers that looked like evidence for the livelock are
exactly what that loop produces:

- `1 676 753` `-EIO` replies in ~15 s: one READ per refault, ~110 k/s.
- round 1 of this note, same probe, `eio` arms: `486 202`-`508 910` replies in
  11.2 s and only **4.5 s of CPU** (the reader blocks in `request_wait_answer`
  between refaults); `abort` arms: **0** replies, >1 core of CPU (refault loop with
  nothing to answer it). Both look like a hang, both are a signal loop.

The corrected probe (`src/mmapwatch2.c`, sha256 `3cbc6ab6...`) leaves `SIGBUS` at its
**default disposition**, so the parent's `wait` status is unambiguous: 135 means the
signal was delivered. It also removes the three other ways to fool yourself:

- `madvise(MADV_RANDOM)` before the first touch, so the kernel does not read ahead
  past the page we care about;
- `mincore()` before touching anything and again after page 0, printing
  `resident_before_any_fault`, `resident_after_page0` and `first_nonresident_page` -
  the witness that the later fault really is on an unread page
  (`resident_before_any_fault=0`, `resident_after_page0=1`,
  `first_nonresident_page=1` in every arm);
- the page index written to fd 2 **immediately before the load**, so the last marker
  in the file is the page that faulted.

## 2. ARM A - exact commands

```bash
cd /home/dev/cang/cang/.scratch/fuse-relay
# round 2: 8 boots, action taken BEFORE the reader reaches page 1
for m in eio abort; do for a in umount umountz umountf none; do bash armA2.sh $m $a; done; done
# round 3: the reader is genuinely blocked (pump SIGSTOPped before the fault phase)
for a in stall_none stall_umount stall_umountz; do bash armA3.sh $a; done
```

Each arm is a fresh boot: `vm.sh` (launcher) -> wait `GUEST_READY` -> `relay-host`
mounts `/tmp/fr/mnt` with `RELAY_STATS` and `RELAY_ON_DEATH=<mode>` -> the guest
(`ctlc`) creates and `sync`s `/mnt/ws/COLD.bin` (5 MiB, `dd if=/dev/urandom`) -> the
host starts `bin/mmapwatch2 /tmp/fr/mnt/COLD.bin 4|5` -> `kill -9` of the launcher ->
the action -> 10 s (round 2) or 6 s (round 3) of observation of the reader's
`/proc/<pid>/stat` state and CPU ticks, then the non-mapped tool sweep.

### 2.1 Round 1 (superseded - the returning handler)

| mode | action | reader | frames | reader CPU |
|---|---|---|---|---|
| eio | none | spinning, 10.8 s, `wchan=request_wait_answer` | 486 202 in 11.3 s (43 k/s) | 4.57 s |
| eio | `fusermount3 -u` | same | 508 910 | 4.59 s |
| eio | `fusermount3 -u -z` | same | 507 672 | 4.59 s |
| eio | `-u -z` then kill pump | same | 3 891 then 0 | 12.05 s (>1 core) |
| abort | none / `-u` / `-u -z` | same, no frames at all | 0 | 12.2-12.3 s |

### 2.2 Round 2 - action taken before the first unread page (8 boots)

`wait_status` is the reader's; `last` is the last page marker written by the probe.

| mode | action | action rc | stderr | mounts after | reader outcome | last |
|---|---|---|---|---|---|---|
| eio | none | 0 | - | 2 | **SIGBUS 135** | page 1 |
| eio | `fusermount3 -u` | 1 | `failed to unmount /tmp/fr/mnt: Device or resource busy` | 2 | **SIGBUS 135** | page 1 |
| eio | `fusermount3 -u -z` | 0 (25 ms) | - | 0 (`mountpoint` -> 32 = `ENOTCONN`) | **SIGBUS 135** | page 1 |
| eio | `umount -f` | 32 | `umount: /tmp/fr/mnt: must be superuser to unmount.` | 2 | **SIGBUS 135** | page 1 |
| eio | `umount2(…)` (all four flag sets) | - | `errno=1 (Operation not permitted)` for `0`, `MNT_FORCE`, `MNT_DETACH`, `FORCE|DETACH` | 2 | **SIGBUS 135** | page 1 |
| eio | `-u -z` then kill pump | 0 (541 ms) | - | 0 | **SIGBUS 135** | page 1 |
| abort | none / `-u` / `-u -z` / `umount -f` | 0 / 1 `EBUSY` / 0 / 32 | as above | 2/2/0/2 | **SIGBUS 135** (all four) | page 1 |

In the `eio -u -z` arm the pump exited cleanly on `ENODEV` (after the reader died and
released the superblock) so its TSV survived: `INIT`, `LOOKUP 0`, `OPEN 0`,
`READ 0` (page 0), **`READ -5`**, **`READ -5`**, `FLUSH -5`, `RELEASE -5` - the two
`-EIO` READs are the page the reader was killed on.

### 2.3 Round 3 - the reader genuinely blocked (the question that was actually asked)

The pump is `SIGSTOP`ped after the reader's `hold t=2s` marker, i.e. before it
reaches its first unread page, so the request cannot be answered by anyone. Proof
that it is blocked rather than looping: `state=S`, `wchan=request_wait_answer`,
`/sys/fs/fuse/connections/63/waiting = 1`, `ticks_delta = 0` over 1 s.

| action while blocked | rc | mounts after | reader 6.2 s later | after `SIGCONT` of the pump |
|---|---|---|---|---|
| none | 0 | 2 | ALIVE, 0 CPU ticks, `wchan=request_wait_answer` | **SIGBUS 135, 20 ms after the `-EIO` answer** |
| `fusermount3 -u` | 1 `EBUSY` | 2 | ALIVE, 0 ticks | **SIGBUS 135** |
| `fusermount3 -u -z` | 0 (22 ms) | 0 (`mountpoint` -> `ENOTCONN`) | ALIVE, 0 ticks, same `wchan` | **SIGBUS 135** |

So: the lazy unmount detaches the *name* and not the connection, and it hands the
faulting reader nothing at all. The error comes from the FUSE reply, and only from it.

### 2.4 What a non-mapped `ls`/`cat` sees afterwards

| state | `ls /tmp/fr/mnt` | `cat /tmp/fr/mnt/GUEST_NONCE` | `dd` a new file there |
|---|---|---|---|
| guest dead, mount kept, `eio` | rc=2 `Input/output error` | rc=1 `Input/output error` | rc=1 `Input/output error` |
| guest dead, mount kept, `abort` (pump closed `/dev/fuse`) | rc=2 `Transport endpoint is not connected` | rc=1 `Transport endpoint is not connected` | rc=1 |
| after `fusermount3 -u -z` (either mode) | **rc=0, empty listing** | **rc=1 `No such file or directory`** | **rc=0, writes the local directory** |

The last row is the hazard: a detached mount leaves the underlying (empty) directory
in place, so path-based tools stop failing loudly and start reporting an empty
workspace. Note 62 §7(b) recorded `-u -z` as "the only escape hatch"; it is the one
action that must **not** be in the supervisor's death path for v1.

**Scope (2026-11-25):** this hazard is the *live* phase, where a dead guest must keep
answering `EIO`. When cang itself exits the mount's job is over and the directory is
not empty - it is the host's own tree - so a refused unmount is completed as a lazy
detach; see [note 65](65-workspace-server-verified.md)'s change block.

## 3. ARM B - the 4 KiB write cap, lifted

### 3.1 Where the cap is decided (not in the transport)

`fuse-backend-rs 0.12.1`, `src/api/server/sync_io.rs::init()`:

```rust
let enabled = capable & want;                      // want = Filesystem::init()
max_write: MIN_READ_BUFFER - BUFFER_HEADER_SIZE,   // 8192 - 4096 = 4096
...
if enabled.contains(FsOptions::BIG_WRITES) { out.max_write = MAX_REQ_PAGES as u32 * pagesize() as u32; }  // 256*4096 = 1 MiB
if enabled.contains(FsOptions::MAX_PAGES)  { out.max_pages = MAX_REQ_PAGES; out.max_write = MAX_REQ_PAGES as u32 * pagesize() as u32; }
```

`PassthroughFs::init()` (`src/passthrough/sync_io.rs`) returned only
`DO_READDIRPLUS | READDIRPLUS_AUTO`, so `enabled` could never contain either flag
whatever the kernel advertised: `capable & want` is empty for them. The kernel side
was already willing - `BIG_WRITES` and `MAX_PAGES` are both in its INIT request.

### 3.2 The patch, exactly

`vendor/fuse-backend-rs` is a copy of the crates.io `0.12.1` sources;
`vendor/bigwrites.patch` (17-line diff, **8 added lines**, 6 of them comment):

```rust
        let mut opts = FsOptions::DO_READDIRPLUS | FsOptions::READDIRPLUS_AUTO;
+       // cang fuse-relay spike: advertise large WRITE frames. ... (6 lines of comment)
+       if self.cfg.do_import {
+           opts |= FsOptions::BIG_WRITES | FsOptions::MAX_PAGES;
+       }
```

`guest-lifted/Cargo.toml` is `guest/Cargo.toml` with
`fuse-backend-rs = { path = "../vendor/fuse-backend-rs", default-features = false, features = ["fusedev"] }`;
`guest-lifted/src/main.rs` is byte-identical to `guest/src/main.rs`. Nothing else
changed - not the pump, not the launcher, not the vsock framing. Build command is
note 62 §3 step 2 with `cd guest-lifted` (musl, `RUSTC=<musl wrapper 1.95.0>`,
`CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=<musl gcc wrapper>`, plus `--offline`).

Negotiated INIT reply, parsed from the pump's own log line:

| field | control | lifted |
|---|---|---|
| flags | `0x6000` (`DO_READDIRPLUS|READDIRPLUS_AUTO`) | **`0x406020`** (+`BIG_WRITES` 0x20, +`MAX_PAGES` 0x400000) |
| max_write | 4096 | **1 048 576** |
| max_pages | 0 | **256** |
| max_readahead / max_background | 131072 / 65535 | same |
| INIT out hex (first 24 B after the 16 B out-header) | `07000000 21000000 00000200 00600000 fffffdbf 00100000` | `07000000 21000000 00000200 20604000 fffffdbf 00001000` |

A full 1 MiB WRITE frame is 1 048 656 B (`in_header` 40 + `fuse_write_in` 40 + data),
which fits the pump's `MAX_MSG` (1 MiB + 4096) and the guest's request buffer
unchanged, and is one contiguous `write()` on the vsock stream - the transport never
had a 4 KiB limit.

### 3.3 Measurements (one boot per arm, same image, same guest tree)

`bin/armB.sh <label>`: phase boundaries on the pump's `CLOCK_MONOTONIC`, guest-side
`ctlc` for the checks, `dd … conv=fsync` and `git status --porcelain` on the host.
Bytes below are the sum of `req_len - 80` over numeric opcode **16** (`FUSE_WRITE`),
and they land exactly on 4 194 304 and 65 536, so the frame count is the whole story.

**4 MiB `dd if=/dev/zero of=/tmp/fr/mnt/BIG.bin bs=1M count=4 conv=fsync`**

| rep | control: frames | control: wall | control: MB/s | control: WRITE median | lifted: frames | lifted: wall | lifted: MB/s | lifted: WRITE median |
|---|---|---|---|---|---|---|---|---|
| 1 | 1024 x 4096 | 0.373 s | 11.2 | 284 us | 4 x 1 048 576 | 0.088 s | 47.7 | 13.4 ms |
| 2 | 1024 x 4096 | 0.411 s | 10.2 | 293 us | 4 x 1 048 576 | 0.048 s | 87.4 | 5.66 ms |
| 3 | 1024 x 4096 | 0.437 s | 9.6 | 295 us | 4 x 1 048 576 | 0.039 s | 107.5 | 2.94 ms |

Phase duration (pump clock, same three reps): control 379/417/444 ms, lifted
93/54/44 ms. Total frames in the phase (all opcodes): 1030 vs 10.

**64 KiB `dd if=/dev/urandom of=/tmp/fr/mnt/SMALL.bin bs=64K count=1 conv=fsync`**

| rep | control: frames | control: wall | control: WRITE median | lifted: frames | lifted: wall | lifted: WRITE median |
|---|---|---|---|---|---|---|
| 1 | 16 x 4096 | 0.024 s | 208 us | 1 x 65 536 | 0.021 s | 950 us |
| 2 | 16 x 4096 | 0.025 s | 265 us | 1 x 65 536 | 0.020 s | 340 us |
| 3 | 16 x 4096 | 0.024 s | 200 us | 1 x 65 536 | 0.022 s | 739 us |

The wall time barely moves because 6 of the frames are metadata in both arms (22
frames control = 16 WRITEs + 6; 7 frames lifted = 1 WRITE + 6) - LOOKUP, OPEN,
FLUSH, RELEASE, GETATTR; 65 KiB is too small for the frame count to matter.

**Warm `git -C /tmp/fr/mnt status --porcelain` (5 reps, after one warm-up run)**

| arm | reps (s) | median | frames per rep | WRITE frames |
|---|---|---|---|---|
| control | 0.251 / 0.251 / 0.235 / 0.228 / 0.232 | 0.235 s | 810-811 | 0 |
| lifted | 0.203 / 0.215 / 0.209 / 0.240 / 0.224 | 0.215 s | 810-811 | 0 |

Identical stdout md5 (`d91bf421816fa88927c90cebfc17c23d`) in all ten runs. Same frame
count on both sides and no WRITEs at all, so lifting the cap cannot have slowed
metadata; I am **not** claiming a speed-up from a 20 ms difference between two boots.

**Guest agreement.** After the host writes: guest `wc -c` = `4194304` / `65536`,
`ls -l` sizes match, and `md5sum /mnt/ws/BIG.bin` = `b5cfa9d6c8febd618f91ac2843d50a1c`
= the host's own md5 of the file it wrote through the mount.

## 4. Recommended death semantics for v1 (one line)

**Keep the mount, answer every request `-EIO` immediately (`RELAY_ON_DEATH=eio`),
never unmount: path syscalls then get `EIO` and a `mmap` fault on an unread page gets
`SIGBUS` 20 ms after the reply - proven by the round-3 `stall_none` arm (blocked
reader, 0 CPU ticks for 6.2 s, `SIGBUS 135` 20 ms after the pump was resumed) and by
all 8 arms of round 2.**

Consequences to write into ticket 23: (a) `mmap` is *no longer* the blocker - the
"5.3x native `git status`, but `mmap` tools are not viable" verdict in note 62 keeps
its cost argument (398 us per read frame) and loses the correctness objection;
(b) the supervisor must not lazily unmount on guest death - it converts loud `EIO`
into a silent empty workspace; (c) a stalled request stalls every other client of
the mount, because the pump is serial: with the pump `SIGSTOP`ped, `mountpoint(1)` on
the mountpoint blocked for the full 2 s timeout (`rc=124`), and so would any tool.

## 5. Harness defects found in this round (fix before the next measurement)

1. **`mmapwatch.c`'s returning `SIGBUS` handler** (above). Use `mmapwatch2.c`; if a
   handler is wanted, it must write a marker and `_exit`, never return.
2. **`relay-host.c`'s `opname()` table is shifted by one for every opcode from 14
   up** (`13 OPEN`, `14 READ`, `15 WRITE`, `16 STATFS`, `20 GETXATTR`), i.e. it uses a
   numbering the kernel does not: the kernel/fuse-backend-rs enum is `Open=14`,
   `Read=15`, `Write=16`, `StatFs=17`, `Fsync=20`, `Flush=25`, `Opendir=27`,
   `Readdirplus=44`. Verified here from `req_len`/`reply_len` shapes: opcode 14 is
   48 B (=`fuse_open_in`), 15 is 80 B (=`fuse_read_in`), 16 is 80 + data. **Any table
   keyed on the pump's printed names for those opcodes is mislabelled** - key on the
   numeric opcode and check the shapes.
3. **The pump loses its last `stats.tsv` rows on `SIGKILL`** (it flushes every 64
   requests and on clean exit): a frame count read after killing the pump can be 0
   when the frames did happen. Counts are trustworthy only for pumps that exited on
   `ENODEV` (both `-u -z` arms and ARM B).
4. The guest rootfs had no `dd`/`md5sum`/`sha256sum`/`expr`/`dirname`/`basename`/
   `uname`/`seq` symlinks; ARM A needs `dd`. Added to `rootfs/bin/` (same coreutils
   closure) - worth adding to `populate-rootfs.sh`'s loop.
5. One `fusermount3` mount shows up **twice** at the same mountpoint
   (`/proc/self/mountinfo` mount ids 734 and 737, same `0:63` superblock, different
   parents) and a single `-u -z` removes both. Note 62 §2 quotes one of the two
   lines; nothing else here depends on it, but a future arm that counts mount
   entries must expect 2.

## 6. Artifacts (sha256)

| artifact | path | sha256 |
|---|---|---|
| corrected probe, source / binary | `src/mmapwatch2.c` / `bin/mmapwatch2` | `3cbc6ab626b883a338f61ba34ee82c068195de7c5b49a72a777ffffe510de7d3` / `0f3a26242dfcdbe426b2db8bedb0690914dfca80754b4bb84588bd4028ab4e19` |
| `umount2()` errno probe | `src/umounttry.c` / `bin/umounttry` | `e84640180caa194f0131bf50fac9510907b8fcd20dea11c7860daef1e0b45a8a` / `5d717fcc7a6a13daa4c332b288009531cf07c20a50d57a6918f32026c76ce6f7` |
| ARM A drivers | `armA.sh` (round 1) / `armA2.sh` / `armA3.sh` | `0a072b0e691dd42e9a313723cb898c460bb08fe6530ad5b04193cb8d5c9276e1` / `e6d613da8b67d455ac5fdc7230072bb866b84d1ae08363d454e5a9ce5687135f` / `514c4011529f05bc1406477d5ca5b41ead1bf7077b5b7316770476da643aed2f` |
| ARM B driver | `armB.sh` | `2af8fdd064e4d1fbce20e85fbcc6fa775f1cea20f16d374b976af2f22a99296e` |
| vendored-crate patch | `vendor/bigwrites.patch` (17-line diff, 8 added lines) | `330636a008be832c91920d5cbe4dd080020f8ab26d25a1b22e194a0dfca6de51` |
| guest FUSE server, control | `guest/target/x86_64-unknown-linux-musl/release/guest-relay` (1 133 600 B) | `04c36fba0eec5c4c86e76991f9c5f24b8d41f5abaacfeb0065d4550c70235e15` |
| guest FUSE server, lifted | `guest-lifted/target/x86_64-unknown-linux-musl/release/guest-relay` (1 133 560 B) | `63472a85daf50bf9675c2d9eb55fde743ec073a5909596e15a2de502d4dc9bc3` |
| host pump / launcher / control client / mmap arm | `bin/relay-host` / `bin/launcher` / `bin/ctlc` / `bin/mmapwatch` | unchanged from note 62 |
| btrfs image (both ARMs) | `/tmp/fr/ws.raw` (regenerated: `truncate -s 256M`; `mkfs.btrfs -f -r /tmp/fr-seed/repo -d single -m single`) | `15134b476f64f892918db34d05c357d214ff3b5b5dbc4d571199f95dbbf615b5` |
| seed tree | `/tmp/fr-seed/repo`, HEAD `8899323240cfb67d7add6f0c957103c1cc10b7b3` | - |
| firmware | `/nix/store/n1nj9ajb91rj9nkzvpwa4g2ppgy23x35-libkrunfw-v5.6.2-cang.6/lib/libkrunfw.so.5.6.2` | `d7c00ad3b23a4b8237a5125579463895010635518ada8dddabfa7669c9da12ca` |

Note: the seed's HEAD is `8899323` rather than note 62's `36270cb` because it is
`git archive HEAD` of this checkout today; the tree is the same shape (764 tracked
files, one modified note, one untracked file).

## 7. Not measured

- **The pump's third death behaviour - silent, connection alive**: only `-EIO` and
  abort were available. A pump that simply stops reading `/dev/fuse` produces exactly
  the round-3 "blocked" state (measured, via `SIGSTOP`), and that state is unbounded
  - nothing in the kernel times a `mmap` fault out.
- **`FUSE_INTERRUPT`** (a signal arriving while a request is in flight) and the
  interaction of a fault-blocked reader with it.
- **Concurrency**: still one client, one connection, one request in flight. ARM B's
  1 MiB frames make the serial pump cheaper per byte, not parallel.
- **Reads at the new `max_write`**: `max_readahead` is still 131072 in both arms;
  only WRITE size was lifted. Fetching 1 MiB read frames needs `FUSE_MAX_PAGES` on
  the read path too (`max_pages` is now 256, so the kernel *may* already send larger
  READs; not measured here).
- **A live guest's `mmap` cost** - unchanged from note 62 §9.
- Nothing about `cang` itself: this is still the hand-written launcher, no supervisor,
  no seccomp/Landlock, no task-dir lifecycle.
