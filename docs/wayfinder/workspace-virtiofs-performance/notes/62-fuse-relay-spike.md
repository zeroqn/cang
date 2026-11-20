# 62 - FUSE relay spike: the guest's btrfs tree mounted on the host, live

**Measured 2026-11-20** on this host (28 cores, `kvm.halt_poll_ns=200000`), against
libkrun from a copy of `deps/libkrun` at **`v2.0.0-cang.22` (`2fad450a`)** built
here with `--features ffi,blk`, firmware `libkrunfw-v5.6.2-cang.6`, two vCPUs, 1 GiB
guest RAM, one VM at a time, no `cang` CLI involved. Harness:
`.scratch/fuse-relay/`. Raw evidence: `results/run8/` (the measured campaign -
`stats.tsv`, `phases.tsv`, per-arm outputs, console log), `results/run2/` (the same
campaign with a per-request guest log line, used as an A/B below), and
`results/run4|run5|run7/` (the failure arms, kept as counts: their `stats.tsv` runs
to 1.7 M rows).
Tickets: [22](../tickets/22-reverse-share-transport.md) (transport, resolved),
[23](../tickets/23-reverse-share-contract.md) (the contract this must satisfy).

## Verdict

**The shape works. A real mount exists and the guest serves it, a host `git status`
costs 5.3x native, and the failure arm is the one thing that must be decided before
v1 because it is not `EIO` for `mmap`.**

- **A real mount exists and it is served by the guest.** `fusermount3` mounts
  `cang-workspace` at a host path; every operation is answered by a
  `fuse-backend-rs` passthrough server in the guest over a btrfs image the guest
  mounted from virtio-blk. A nonce file the guest writes at boot reads back on the
  host, and bytes written on the host read back inside the guest.
- **Cost: ~236 us per FUSE frame overall** (median; 182-287 us for the cheap
  metadata ops, 398 us for reads carrying 4 KB median / 105 KB max) and **~3.5 k
  frames/s with one request in flight**. That is 1.3-2x the 143 us transport round
  trip of note 61: the guest server's own work plus the second `/dev/fuse` syscall
  pair is the rest.
- **`git status --porcelain` on this repo (764 tracked files), host-through-relay vs
  guest-native: 0.211 s vs 0.040 s warm (5.3x), 1.638 s vs 0.040 s cold (41x).**
  Identical stdout and identical HEAD from both sides. The cold run is a full
  content re-hash (4765 frames vs 810) because the first host-side `git status`
  distrusts the index's stat cache and rewrites the index through the relay.
- **That 5x is inside the map's price envelope** (note 61 re-priced a host `git
  status` at 0.2-0.45 s) and an editor save / pi read stays around a millisecond, so
  stat-and-read-shaped host tools - pi's file tools, `git`, `grep`, an editor save -
  are served at a workable cost. **Builders stay in the guest**, as the map says.
- **Failure: `EIO` for every syscall the contract names, and a livelock for `mmap`.**
  After `kill -9` of the guest, `ls`/`cat`/`stat`/`mkdir`/`rm`/`dd` all return
  `EIO` and the stale mount stays - ticket 23 decision 4(a), exactly. But a page
  fault on an **already-mapped** file does not fail: with the pump answering `-EIO`
  a faulting reader burned 100% CPU on both sides and gave **1 676 753 `-EIO`
  replies in ~15 s** without ever raising `SIGBUS`; with the pump instead closing
  `/dev/fuse` (connection aborted) the reader spun at ~1 core inside the kernel and
  still never got `SIGBUS`. `git` mmaps its index and `rust-analyzer`/`rustc` mmap
  everything, so this is a v1 blocker for those tools - and at 398 us per read frame
  they are not viable through this mount anyway.
- **Writes are capped at 4 KiB per frame** because `fuse-backend-rs` 0.12's INIT
  reply advertises neither `FUSE_BIG_WRITES` nor `FUSE_MAX_PAGES`, so a 4 MiB write
  is 998 frames and runs at **12.8 MB/s**. That is a guest-side INIT flag, not a
  transport change, and the first thing to price.

## 1. What was built, and where

All of it untracked, under `/home/dev/cang/cang/.scratch/fuse-relay/` (copied from
`.scratch/vsock-spike/`; that spike's `libkrun-src/` was reused and rebuilt):

| file | what it is |
|---|---|
| `src/launcher.c` | the vsock-spike launcher extended: one **virtio-blk** device for the btrfs image, a second vsock unix port for a guest control shell, guest binary `guest-relay` |
| `src/relay-host.c` | the **host pump**: libfuse's `fuse_mount` handshake (`socketpair` + `_FUSE_COMMFD=3` + `exec fusermount3` + `recvmsg(SCM_RIGHTS)`), then a serial relay of FUSE frames between `/dev/fuse` and the vsock unix socket; per-frame TSV stats; two death modes |
| `guest/src/main.rs` | the **guest half** (static musl Rust): mount devtmpfs/proc, mount `/dev/vda` (btrfs) at `/mnt/ws`, write a boot nonce, listen on `AF_VSOCK:50430` and serve each accepted connection with `fuse_backend_rs::passthrough::PassthroughFs` behind `Server::handle_message`; plus a control shell on `AF_VSOCK:50431` for driving guest commands |
| `src/ctlc.c` | host client for the guest control shell (write a line, stream the reply to the `###DONE` sentinel) |
| `src/mmapwatch.c` | the `mmap` failure arm: map a file, fault **only page 0**, hold, then fault every page and report `SIGBUS`/success per page |
| `src/nowmono.c` | `CLOCK_MONOTONIC` nanoseconds, so phase boundaries and the pump's per-frame timestamps share one clock |
| `populate-rootfs.sh` | builds the guest rootfs: static `guest-relay`, `/bin` symlinks, and real copies of the `bash`/`coreutils`/`git` closures (this host cannot hard-link `/nix/store` - cross-subvolume `EXDEV`) |
| `campaign.sh`, `failarm.sh`, `vm.sh` | the campaign, the failure arm, and one boot |
| `results/run8/`, `results/run2/`, `results/run{4,5,7}/` | raw evidence |

Artifacts actually measured:

| artifact | path | sha256 |
|---|---|---|
| libkrun (this build, `ffi,blk`) | `.scratch/fuse-relay/libkrun-src/target/release/libkrun.so` | `f6f95dd475d755c82f3d33c69f4d7746ecd25fc216890cec152d153d4423117d` |
| libkrun init blob | same dir, `libkrun_init.so` | `9b9e978876caae854f1d7b85a61c55018dd0421e279d213ff00feda7fbb7fbe8` |
| firmware | `/nix/store/n1nj9ajb91rj9nkzvpwa4g2ppgy23x35-libkrunfw-v5.6.2-cang.6/lib/libkrunfw.so.5.6.2` | `d7c00ad3b23a4b8237a5125579463895010635518ada8dddabfa7669c9da12ca` |
| guest FUSE server (static musl) | `.scratch/fuse-relay/guest/target/x86_64-unknown-linux-musl/release/guest-relay` (copied to `rootfs/bin/` by `populate-rootfs.sh`; the rootfs itself is not kept) | `04c36fba0eec5c4c86e76991f9c5f24b8d41f5abaacfeb0065d4550c70235e15` |
| host pump | `.scratch/fuse-relay/bin/relay-host` | `1b1b1b57871bbef4f9797a6b67706ea746eb459915b53a986a9be9ce8afe6278` |
| launcher | `.scratch/fuse-relay/bin/launcher` | `a212f20836ec6280c4a83d085ba04eeef1f5d674cfe6ef8934d392069842d26c` |
| control client | `.scratch/fuse-relay/bin/ctlc` | `e446fcde1308ace6d736dd5f1ea9eae77f85b8dd1966fd3e56194c742045e159` |
| mmap arm | `.scratch/fuse-relay/bin/mmapwatch` | `0b9e56c4a71479eac20a400bcdd868d3452eefc826079f5196d807b54feca8e3` |
| btrfs image of the measured campaign | `/tmp/fr/ws.raw` at run8 (regenerate: `truncate -s 256M` then `mkfs.btrfs -f -r /tmp/fr-seed/repo -d single -m single`) | `4343baa068341326ee9d0382d1bf373ea546fad183ab310dc5fd8547e05938f4` |
| guest FUSE library | `fuse-backend-rs 0.12.1` (crates.io, Apache-2.0 AND BSD-3-Clause), `default-features = false, features = ["fusedev"]` | `guest/Cargo.lock` |

## 2. Which route was taken, and what fought back

- **Guest half: `fuse-backend-rs`, not hand-rolled.** `Server::<PassthroughFs>::new`
  + `handle_message(Reader, Writer, None, hook)` covers the whole op set the
  contract lists. The transport seam is `Reader::from_fuse_buffer(FuseBuf::new(&mut
  req))` plus a `FuseDevWriter` over the reply buffer; there is no connection trait
  to implement, so the vsock socket is driven by the loop around those two.
- **Host half: hand-written pump, no libfuse servicing loop.** `fuse_mount` is
  reproduced exactly - the helper's env convention is `_FUSE_COMMFD`, and the
  handshake worked on the first attempt.
- **No extra framing.** One FUSE message is one contiguous write on the vsock
  stream, sized by its own `in_header.len`; the reply likewise by `out_header.len`.
- Three protocol/implementation facts cost most of the debugging time and belong in
  any re-implementation:
  1. **`FORGET`/`BATCH_FORGET` carry no reply.** A pump that waits for one eats the
     *next* request's reply and every frame after it is mismatched - which looks
     exactly like a broken guest server (garbage opcodes, a "reply" of 144 bytes for
     a forget). The pump now relays those two opcodes and moves on, and the guest
     never answers them, not even on error.
  2. **`FuseDevWriter::commit()` writes to its file descriptor.** `do_readdir` - and
     therefore every `READDIRPLUS` - composes the reply in a split writer and then
     *commits*, i.e. issues the device write; with `fd = -1` that is `EBADF` for
     every directory listing, and since `handle_message` returns that as an encoding
     error the whole stream shifts. The writer is given an open `/dev/null`
     instead: the bytes are already aliased into the reply buffer, and the commit
     write is discarded.
  3. **`FORGET` has no reply, so its "latency" is pump-side only (4.9 us)** - do not
     average it into the per-op table without saying so.
- **What is served, from what tree.** The workspace image is a **btrfs image
  attached as virtio-blk** (`krun_block_device_new(..., KRUN_DISK_FORMAT_RAW)`),
  created with `mkfs.btrfs -r` from a real git checkout (the cang tracked tree, repo
  HEAD `d2acc16b`, committed in the seed as `36270cb`; 764 files, one modified +
  four untracked files so `git status` has something to report). The guest mounts
  `/dev/vda` itself
  (`guest: MOUNTS /dev/vda /mnt/ws btrfs rw,relatime,discard=async,noacl,space_cache=v2,subvolid=5,subvol=/`)
  and serves `PassthroughFs::new(Config { root_dir: "/mnt/ws", ..Default::default() })`
  - **cache policy `Auto`, 5 s attr/entry timeouts, `writeback = false`,
  `xattr = false`**, which is what ticket 23 decision 2(a) wants ("kernel cache
  timeouts on").
- **Mount options: `-o fsname=cang-workspace`, nothing else.** No
  `default_permissions` (the kernel would then check mode bits against the host uid;
  image files are uid 1000, guest-created files are root), no `auto_unmount` (the
  contract wants the stale mount to stay), read-write (v1's read-only rule is
  product policy, not a spike requirement).

The kernel and the guest server negotiated this (raw `fuse_init_out` in
`results/run8/stats.tsv`'s INIT row and the pump log):

| field | value |
|---|---|
| major/minor | 7.33 |
| flags | `0x6000` = `FUSE_DO_READDIRPLUS \| FUSE_READDIRPLUS_AUTO` only - **no `FUSE_BIG_WRITES`, no `FUSE_MAX_PAGES`, no `FUSE_ASYNC_READ`, no `FUSE_WRITEBACK_CACHE`** |
| flags2 | `0x2` = `FUSE_HAS_INODE_DAX` (advertised; no DAX window exists in this setup) |
| max_readahead | 131072 |
| max_background / congestion | 65535 / 49149 |
| **max_write** | **4096** |
| max_pages / map_alignment | 0 / 0 |
| time_gran | 1 ns |

`/proc/mounts`: `cang-workspace /tmp/fr/mnt fuse rw,nosuid,nodev,relatime,user_id=1000,group_id=993`
- i.e. an unprivileged mount via the setuid `fusermount3`, no root, no userns.

## 3. Exact commands

```bash
# 1. libkrun + init blob: a copy of the fork, built HERE with the block device enabled
cd /home/dev/cang/cang/deps/libkrun
tar -c --exclude=./.git --exclude=./target . | tar -x -C ../../.scratch/fuse-relay/libkrun-src
nix develop /home/dev/cang/cang --command bash -c 'cd /home/dev/cang/cang/.scratch/fuse-relay/libkrun-src \
  && export CARGO_TARGET_DIR=$PWD/target \
  && cargo build --release -p libkrun --features ffi,blk \
  && cargo build --release -p libkrun_init'
# note 61 built the same tree WITHOUT `blk`, so krun_block_device_* is absent from
# that libkrun.so (the launcher then dies at symbol lookup: the feature is not default)

# 2. the guest FUSE server, static musl (cargo + the nix musl rustc/gcc)
export PATH=/nix/store/0592hadxldzhhd3ppc6nskslzd39fwlz-gcc-wrapper-15.3.0/bin:/nix/store/ay6cv1bpy48djry0gvadrgiz3xzsv4yn-rust-stable-with-components-2026-10-15/bin:$PATH
export RUSTC=/nix/store/3k9jxj3ypnr43878x8bpnnk8k8my4f1n-x86_64-unknown-linux-musl-rustc-wrapper-1.95.0/bin/rustc
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=/nix/store/vc8fyjxviq0cg1iciw5b45l8anhh68rz-x86_64-unknown-linux-musl-gcc-wrapper-15.3.0/bin/x86_64-unknown-linux-musl-gcc
cd .scratch/fuse-relay/guest && cargo build --release --target x86_64-unknown-linux-musl

# 3. host programs (gcc from the devshell)
gcc -O2 -o bin/relay-host src/relay-host.c
gcc -O2 -o bin/ctlc src/ctlc.c
gcc -O2 -o bin/mmapwatch src/mmapwatch.c
gcc -O2 -o bin/nowmono src/nowmono.c
gcc -O2 -Ilibkrun-src/include -Llibkrun-src/target/release -lkrun -lkrun_init \
    -Wl,-rpath,$PWD/libkrun-src/target/release -o bin/launcher src/launcher.c
# lib/libkrun.so.2 and lib/libkrun_init.so must point at THIS build, not the spike's

# 4. rootfs + image (regenerable)
git -C /home/dev/cang/cang archive HEAD | tar -x -C /tmp/fr-seed/repo
(cd /tmp/fr-seed/repo && git init -q . && git add -A && git commit -qm seed)
echo "relay spike" >> /tmp/fr-seed/repo/docs/wayfinder/workspace-virtiofs-performance/notes/61-reverse-share-transport-spike.md
echo "untracked file created by the seed" > /tmp/fr-seed/repo/UNTRACKED.md
.scratch/fuse-relay/populate-rootfs.sh
truncate -s 256M /tmp/fr/ws.raw
mkfs.btrfs -f -r /tmp/fr-seed/repo -d single -m single /tmp/fr/ws.raw

# 5. one boot, then the mount, then the campaign
.scratch/fuse-relay/vm.sh /tmp/fr/run8 /tmp/fr/ws.raw 2 &     # up when its log says GUEST_READY
RELAY_STATS=/tmp/fr/run8/stats.tsv .scratch/fuse-relay/bin/relay-host \
    /tmp/fr/run8/vsock.sock /tmp/fr/mnt 'fsname=cang-workspace' &
.scratch/fuse-relay/campaign.sh /tmp/fr/run8 3
.scratch/fuse-relay/failarm.sh /tmp/fr/run8                   # kills the guest; run last
```

Pump knobs: `RELAY_ON_DEATH=eio` (default, ticket 23 decision 4) or `abort` (closes
`/dev/fuse` on transport EOF); `RELAY_STATS=<tsv>`; `RELAY_VERBOSE=1`.

## 4. It is the guest's tree (correctness arms)

From `results/run8/`, one boot, one mount:

| arm | command | observed |
|---|---|---|
| host `ls -la` | `ls -la /tmp/fr/mnt` | the guest tree with the image's metadata; the guest-created `GUEST_NONCE` appears as `-rw-r--r-- 1 root root 33` (root: the guest wrote it as root) |
| **guest-only file, read by the host** | `cat /tmp/fr/mnt/GUEST_NONCE` | `12c3cadcf0307c5f 487 guest-relay`, and this boot's guest console line was `GUEST_NONCE 12c3cadcf0307c5f` (checked before the reply was written) |
| **guest write, host read** | guest: `echo "written by the guest at $(date +%s%N)" > /mnt/ws/GUEST_WRITE.txt` | host `cat`: `written by the guest at 1795183786110250014`; host `ls -l`: `-rw-rw-rw- 1 root root 44` |
| **host write, guest read** | host: `echo "written by the host over the relay at $(date +%s%N)" > /tmp/fr/mnt/HOST_WRITE.txt` | guest `cat /mnt/ws/HOST_WRITE.txt`: `written by the host over the relay at 1795183786643104530`; guest `ls -l`: `-rw-r--r-- 1 1000 993 58` |
| statfs | `stat -f /tmp/fr/mnt` | `type=fuse bsize=4096 blocks=65536 free=60559` (the btrfs image's geometry) |

The guest is driven by a control shell over a second vsock unix port
(`AF_VSOCK:50431` -> `ctl.sock`), so "run this in the guest and show me the output"
is a real `/bin/sh -c` in the guest, not a host guess.

## 5. `git status --porcelain` - host-through-relay vs guest-native

Same checkout, same command, alternating inside one boot, after the cold first run.
Host timing is bash's `time` on the real command; guest timing is `time` inside the
guest's own shell.

| sequence | host through the relay | guest native (`/mnt/ws`) |
|---|---|---|
| cold first run (index re-hash, 4765 frames) | **1.638 s** | - |
| warm: 5 alternating reps | 0.462 / 0.208 / 0.204 / 0.216 / 0.211 s (median **0.211 s**) | 0.056 / 0.053 / 0.040 / 0.037 / 0.033 s (median **0.040 s**) |
| campaign reps (same boot, earlier; `time`) | 0.237 / 0.217 s | 0.241 / 0.058 / 0.103 s |
| **ratio, warm median** | **5.3x native** (4.8x on means: 0.210 vs 0.0438 s) | - |
| ratio, cold | 41x native | - |

`git status --porcelain` output was byte-identical on both sides (the same ` M` plus
four `??` lines, verified by md5 across all reps) and both sides reported `rev-parse
HEAD` = `36270cbf7962678f0828a01eebf68ab07f68e850` and `ls-files | wc -l` = 764.

Where the two runs differ in *work*, not just speed (stats bucketed by `phases.tsv`):

| phase | frames | wall (phase boundaries) | frames/s | dominant opcodes |
|---|---|---|---|---|
| `host_git_1` (cold) | 4765 | 1.645 s | 2897 | LOOKUP 1089, FLUSH 786, RELEASE 786, OPEN 785, READ 785 (READDIRPLUS 165) |
| `host_git_2` | 810 | 0.242 s | 3342 | READDIRPLUS 165, GETATTR 140, OPENDIR 121, RELEASEDIR 121, LOOKUP 93, READDIR 73 |
| `host_git_3` | 810 | 0.223 s | 3634 | same |
| `host_ls_la` | 7 | 0.020 s | - | READDIRPLUS 2, GETATTR 1, LISTXATTR 1, GETXATTR 1, OPENDIR 1, RELEASEDIR 1 |
| `host_cat_nonce` (2 `cat`s + a 3.7 KB file) | 13 | 0.027 s | - | OPEN/READ/FLUSH/RELEASE x3, GETATTR 1 |
| `guest_write_host_read` | 5 | - | - | LOOKUP, OPEN, READ, FLUSH, RELEASE |
| `host_write_guest_read` | 6 | - | - | LOOKUP, CREATE, WRITE, FLUSH, RELEASE, FLUSH |

So the **cold** host run is the whole tree being content-read (one
open/read/flush/release per file, 785 files) because the first host-side `git`
distrusts the index's stat cache and rewrites the index through the relay; the warm
run is directory scans only (READDIRPLUS/GETATTR/OPENDIR). **810 frames buy the
whole warm `git status`** - the 5.3x is per-frame latency, not frame count.

## 6. Op histogram and per-frame latency

**A guest-side `println!` per request costs ~90 us per frame** - the same campaign
run with one log line per FUSE request in the guest (`results/run2/`) gave 330 us
medians and 0.310 s warm `git status`; with the line removed (`results/run8/`) the
medians are 182-287 us and warm `git status` 0.211 s. All numbers below are the
clean run.

All relayed frames of run8 (11 264 frames: 10 384 with `error = 0`, 871 `ENOENT`, 2
`ENOSYS`, and 7 no-reply FORGETs; the latency table covers the 10 391 reply-bearing
or no-reply frames):

| op | frames | median | p95 | p99 |
|---|---|---|---|---|
| LOOKUP | 1775 | 257.0 us | 469.0 us | 794.1 us |
| READDIRPLUS | 1314 | 287.2 us | 413.1 us | 503.1 us |
| OPENDIR | 964 | 203.1 us | 320.3 us | 375.2 us |
| RELEASEDIR | 961 | 181.6 us | 273.5 us | 332.6 us |
| FLUSH | 953 | 194.7 us | 298.0 us | 413.7 us |
| RELEASE | 952 | 209.3 us | 315.2 us | 466.6 us |
| OPEN | 944 | 240.4 us | 356.8 us | 442.6 us |
| READ | 944 | 397.5 us | 1030.7 us | 1730.1 us |
| GETATTR | 881 | 186.4 us | 288.6 us | 380.9 us |
| READDIR | 579 | 235.9 us | 318.0 us | 370.7 us |
| ACCESS | 73 | 192.9 us | 337.5 us | 996.4 us |
| WRITE | 27 | 245.7 us | 299.3 us | 345.2 us |
| CREATE | 9 | 395.5 us | 2604.4 us | 3388.9 us |
| UNLINK | 6 | 278.8 us | 309.5 us | 314.4 us |
| FORGET | 7 | 4.9 us | - | - (no reply: pump-side only) |
| RENAME | 1 | 1241.2 us | - | - |
| INIT | 1 | 1725.7 us | - | - |
| **all frames (error = 0 + no-reply)** | **10 391** | **236.5 us** | **530.8 us** | **875.4 us** |

A flat ~190-290 us floor per op, whatever the op is; `READ` is the only op that
carries a payload cost (398 us median, p95 1.0 ms).

Payload shapes (measured, not assumed):

- **READ**: 944 frames carrying 1 B to **105 130 B**, median **3 985 B**, p95
  35 523 B, 463 of 944 larger than 4 KiB - the kernel does read ahead in large
  frames even though `FUSE_MAX_PAGES` was not negotiated.
- **WRITE**: capped at **4096 B of data per frame** (`max_write=4096`, no
  `FUSE_BIG_WRITES`). `dd if=/dev/zero of=/tmp/fr/mnt/BIG2.bin bs=1M count=4
  conv=fsync` = **4 MiB in 0.328 s = 12.8 MB/s**, 998 WRITE frames at ~250 us each;
  the guest's own `ls -l`/`wc -c` on its mount showed 4 194 304 bytes.

## 7. Failure arms - what the host actually sees

**(a) The contract's arm: the pump answers `-EIO` (`RELAY_ON_DEATH=eio`).**
`kill -9` of the guest launcher while the mount is up (`results/run2/failarm.txt`,
509 `-5` frames in that run's stats):

| host command | result |
|---|---|
| `mountpoint /tmp/fr/mnt` | `Input/output error` |
| `ls -la /tmp/fr/mnt` (root, dentries cached) | `cannot access '/tmp/fr/mnt': Input/output error` |
| `ls -la /tmp/fr/mnt/crates/cang/src/runtime` (never listed) | `Input/output error` |
| `cat /tmp/fr/mnt/crates/cang/src/main.rs` (never read) | `Input/output error` |
| `cat /tmp/fr/mnt/GUEST_NONCE` (read before) | `Input/output error` |
| `stat /tmp/fr/mnt/docs/wayfinder` | `cannot statx ...: Input/output error` |
| `dd` a new file / `mkdir` / `rm` | `Input/output error` |

The mount **stays** (`/proc/mounts` still lists `cang-workspace`), nothing is queued
or replayed, and the pump prints one named line
(`relay-host: write to guest failed: Broken pipe -> eio`). Exactly ticket 23
decision 4(a).

**(b) The arm that does not match the contract: an existing `mmap` livelocks.**
`mmapwatch /tmp/fr/mnt/COLD2.bin 8` (5 530 000 B, 1351 pages, only page 0 faulted
before the kill), then `kill -9`:

| death mode | what the faulting reader did | what the kernel/pump did |
|---|---|---|
| `eio` | never returned from the first unread page; **no `SIGBUS`**, no error | the pump served **1 676 753 `-EIO` replies in ~15 s** (~110 k/s); the reader was killed |
| `abort` (close `/dev/fuse`) | still spinning in the fault 10+ s later, `State: R`, **~1 core** (297 ticks of CPU in 3 s), **no `SIGBUS`** | `waiting=0`: after the abort the kernel refuses the request itself |

Host *syscalls* in abort mode return **`ECONNABORTED`** ("Software caused connection
abort") for the first one and **`ENOTCONN`** ("Transport endpoint is not connected")
afterwards - a different contract from (a), and not the `EIO` ticket 23 decided.
`fusermount3 -u -z` unmounts the stale mount cleanly in both modes; that is the only
escape hatch this spike demonstrated, and it contradicts "the stale mount stays", so
it needs bob's decision rather than a code tweak.

**(c) A durability observation, not a failure arm.** A file the guest created and
never synced (`COLD.bin`, 5.5 MB, VM then `kill -9`ed) came back as **0 bytes** on
the next boot; the same file created and `sync`ed in the guest came back at its full
5 530 000 bytes. Killing the VM loses unsynced guest page cache the way killing any
kernel does - the contract's "writes are not queued" should be read as including the
guest's own last seconds.

## 8. What this prices, against notes 58/61 and ticket 23

| quantity | measured here | compare |
|---|---|---|
| transport round trip (echo, note 61) | (inherited) 143 us median | - |
| **one FUSE frame, cheap op** | **182-287 us median**, p99 333-794 us | 1.3-2.0x the echo round trip |
| one FUSE frame, median READ (4 KB) | 398 us | 2.8x |
| one FUSE frame, all ops | 236.5 us median, p99 875 us | - |
| **aggregate, one request in flight** | **2.9-3.6 k frames/s** (810 frames / 0.22-0.24 s) | note 61's serial ceiling was 6.6 k echo ops/s |
| host `git status` (764 files, warm) | **0.211 s** (810 frames) | note 58 re-priced 0.2-0.45 s; note 61 0.7-1.5 s at 8-stream latency |
| host `git status` (cold, index rewrite) | 1.638 s (4765 frames) | - |
| guest-native `git status` (same tree) | 0.040 s | ticket 23 recorded 52 ms native |
| `cat` of a 3.7 KB file | ~10 ms for the arm incl. 3 frames | sub-millisecond per op |
| 4 MiB write | 12.8 MB/s (998 x 4 KiB frames) | capped by `max_write=4096` |
| mount/INIT | 1.7 ms | - |

**Is this a workable product shape at these costs?** For the host tools the map
names - pi's file tools, `git`, `grep`, an editor save - yes: 5.3x native `git
status`, ~0.2-0.4 ms per metadata op, ~12.8 MB/s writes, no root, no userns, the
guest's own btrfs tree. For `mmap`-heavy tools - `rust-analyzer`, `rustc`, anything
that faults many pages - no, and not only because of the livelock: at 398 us per read
frame a fault-heavy workload pays a frame per page. Two fixable knobs would move the
numbers, both on the guest/INIT side, neither in the transport:

1. **`FUSE_BIG_WRITES` + `FUSE_MAX_PAGES`** in the INIT reply (the `max_write` field
   is already in `fuse_init_out`) - writes are 4 KiB serial today.
2. **A concurrent pump.** 3.5 k frames/s is one request in flight; note 61 measured
   15.5 k ops/s aggregate across 8 streams, so a windowed pump (or a guest server
   thread per stream) is where a further multiple could come from. Not measured here.

## 9. What I did NOT measure

- **No `cang` CLI, no cang image, no seccomp/Landlock.** A hand-written launcher
  against the fork's ABI-2 C API; the pump ran as the plain user via the setuid
  `fusermount3`; the guest server ran as guest root, unconfined. Nothing here
  exercises cang's supervisor, its vsock port registry (50 430 chosen by note 58),
  render-server-style policy, or task-dir lifecycle.
- **No concurrency of any kind**: one client, one connection, one request in flight,
  no second stream, no parallel guest service.
- **No real editor save, no `cargo build` through the mount, no `rust-analyzer`
  mmap traffic**, no xattr-dependent tool (`xattr = false` in the guest config), no
  `fsync`-heavy workload, no rename2/whiteout semantics, no hardlink/symlink
  exercise beyond what `git` did.
- **Read-only mount** (ticket 23 decision 3(c)) not exercised; the spike mount is
  read-write.
- **`mmap` success path**: I did not price faults against a *live* guest (only that a
  dead one livelocks); the read numbers above come from the `read()` path.
- **`FUSE_INTERRUPT`** (signal delivered to a host tool with a frame in flight) is
  untested; with a serial pump one hung op would block every other op.
- **One host, one firmware, one geometry** (2 vCPUs, 1 GiB). Note 61 showed vCPU
  count moves concurrency costs 3x, so these numbers are for this geometry only.
- **The image's longer-term behaviour**: the campaign image is a static
  `mkfs.btrfs -r` seed; growth, subvolumes, and reflink/extent layout were not
  exercised.
- **Nothing about the workspace-server VM** (ticket 29), the invalidation watcher
  (ticket 30), or `inotify` (unfixable by the protocol; unchanged).

## 10. Things worth naming rather than smoothing

- **The first host operation after a boot pays for the whole tree.** The 1.638 s
  cold `git status` is not latency drift: 785 files are opened and read because the
  index's stat cache was written by a git that was not looking through this mount.
  Any product that lets both sides touch the index re-hashes the tree on every
  stat-cache invalidation.
- **Guest-side logging is not free**: one `println!` per request costs ~90 us per
  frame here (330 us vs 236 us medians, 0.310 s vs 0.211 s warm `git status`). Any
  future measurement of this relay must say whether the measured path logged.
- **A killed guest loses unsynced page cache** (section 7c).
- **`FUSE_HAS_INODE_DAX` is advertised (`flags2=0x2`) with no DAX window** - the
  flag is not evidence of anything; the /nix DAX work must not read it as such.
- **Reconnect works**: unmounting and restarting the pump re-connects to the same
  guest (`guest: accepted a connection` again), so a pump crash does not need a VM
  restart. The guest builds a fresh `PassthroughFs` per connection, so its inode
  cache resets - invisible to the host thanks to the 5 s timeouts.
- **`deferring proxy removal` WARNs and one `unexpected dgram pkt: 3` at boot** are
  libkrun log noise repeated from note 61, not failures: every arm above completed
  and the client verified every reply.
