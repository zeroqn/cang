---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Re-testing the cold-read wall with a validated cache control: block lower vs virtio-fs, and the concurrency collapse

**Verdict.** With the host page cache *proved* cold rather than assumed cold, the
recorded "the wall is the guest kernel's buffered readahead path" does not
reproduce on this host. What does reproduce, in one boot each time:

- the block lower delivers **109-115 MB/s** where virtio-fs delivers
  **176-194 MB/s** for the same 610 MB, same guest cache state, host cache hot
  for both;
- eight concurrent readers make the block path **worse**, not better:
  **58-74 MB/s aggregate** against 80-115 MB/s for one reader;
- the guest is never CPU-saturated (5-22 % of 26 vCPUs), never reclaimes
  (`pgsteal` 0, 3.5 GiB free), and its own warm page-cache ceiling is
  **8.7 GB/s** - so neither guest CPU nor guest RAM is the limiter.

Nothing here changes cang's code, and it does **not** justify a vhost-user or
queue-count change (see §5).

## 1. The instrument, checked this time

The earlier campaigns could not drop the host page cache and said so. This
session dropped it with `posix_fadvise(POSIX_FADV_DONTNEED)` and then *verified*
the drop instead of assuming it: pread 128 MiB of the `.raw` cold, warm, after a
range drop, and after a whole-file drop.

| read | ms | MB/s |
|---|---|---|
| into cache | 21 | 5976 |
| cache hit | 16 | 8067 |
| after range `DONTNEED` | 75 | 1710 |
| after whole-file `DONTNEED` | 74 | 1724 |

So the eviction works and re-reads go to the backing device at ~1.7 GB/s. Two
corrections follow for reading the older notes: a `DONTNEED` on a *mapped* region
proves nothing (`invalidate_mapping_pages` skips mapped pages - the first attempt
here "measured" 14 GB/s and was discarded), and the state root's device is
**`sda`** (btrfs, `compress=zstd:1`, 93 % full, 88 307 extents), not `sdb`, which
is `/`. Sampling `sdb` reports zero traffic and looks like cache service.

## 2. Route comparison: block lower vs virtio-fs, identical bytes

One boot, 610 MB read once per arm, guest page cache dropped per arm, host cache
dropped per arm for both sources (`sda` moved 0-32 sectors per arm, i.e. both were
served from host RAM, which makes this a pure delivery-path comparison).

| arm | source | ms | MB/s |
|---|---|---|---|
| `blk-a` | `/run/cang/nix-lower` (virtio-blk) | 5882 | 109 |
| `vfs-a` | `/workspace` (virtio-fs) | 3296 | 194 |
| `blk-b` | block lower | 5546 | 115 |
| `vfs-b` | virtio-fs | 3630 | 176 |

The block lower is ~1.6x slower than the FUSE share for the same bytes in the
same boot. That is the opposite of the warm picture recorded in note 04 (block
921 MB/s against virtio-fs 583 MB/s), so coldness is doing something to this
device that it does not do to the share.

## 3. Concurrency collapses the block path

Same boot shape, 8-9 files > 50 MiB in the lower: one reader against eight
readers over distinct files, buffered and `O_DIRECT`, guest cache dropped per arm.

| arm | readers | MB | ms | MB/s (aggregate) | guest CPU % | iowait s | device-busy % of wall |
|---|---|---|---|---|---|---|---|
| `one-a` | 1 | 610 | 7210 | 85 | 7 | 0.5 | 22 |
| `one-b` | 1 | 610 | 5945 | 103 | 5 | 0.3 | 50 |
| `direct1` | 1, `iflag=direct` | 610 | 3734 | 171 | - | - | - |
| `eight-a` | 8 | 2078 | 35853 | 58 | 22 | 24.4 | 217 |
| `eight-b` | 8 | 2078 | 33916 | 61 | 22 | 30.6 | 332 |
| `direct8` | 8, `iflag=direct` | 2078 | 35413 | 59 | 22 | 30.6 | 332 |
| `eight-a` (repeat boot) | 8 | 2078 | 28132 | 74 | - | - | - |

Aggregate throughput *falls* by ~30 % when eight readers are added, `O_DIRECT`
changes nothing at either depth, and the device's accumulated service time rises
to 2-3x the wall (concurrent requests each waiting) with 24-30 s of guest iowait.
No reclaim, 3.5 GiB free, no swap: this is not memory pressure and not CPU.

## 4. The guest's own ceiling, and one retraction

`/tmp` in the guest is tmpfs (`2.0G`), and reading 610 MB of it:

| arm | ms | MB/s |
|---|---|---|
| first buffered read | 3875 | 165 |
| second (cache-warm) | 73 | **8745** |
| `bs=64k`, cache-warm | 88 | 7280 |
| `bs=4k`, cache-warm | 224 | 2860 |

Writing 610 MB of zeros to that tmpfs took **17.9 s (34 MB/s)**, while `dd` from
`/dev/zero` to `/dev/null` runs 610 MB in 81 ms (7.9 GB/s). So this guest's
page-allocation path is far slower than its copy path, and a *first-touch* read
can look like a 300 MB/s "ceiling" that is really page-allocation cost. An earlier
draft of this note called 313 MB/s "the guest's ceiling"; that is **retracted** -
the warm ceiling is 8.7 GB/s, so the 85-194 MB/s of §2-§3 is not the guest's copy
path and the block device genuinely loses time somewhere in delivery.

## 5. What this does and does not settle

- **Not a backend question.** A vhost-user device cannot help: the fork already
  widened this device to four virtqueues with `VIRTIO_BLK_F_MQ` and saw no change
  (note 07, reverted), and any vhost-user device also flips all guest RAM to
  memfd, changing the RAM-backing axis that `MADV_POPULATE_WRITE` already refuted.
  Declining the prototype is arithmetic: with the device path accounting for
  ~0.24 s of a 4-8 s arm in the traced campaign, the ceiling on a transport swap
  is a few percent, inside a 25-37 % within-boot spread.
- **Not settled: where the block device loses the time.** The per-request trace
  that produced note 07's attribution lives in a locally built cang
  (`CANG_BLK_TRACE`); the prebuilt 0.21.0 used for these arms has no such trace,
  which is why this boot can only say "delivery, and it degrades with depth".
  The decisive next arm is the same 1-reader/8-reader pair on a HEAD build with
  the trace enabled, splitting pread, staging copy, and completion/interrupt.
- **Host caveat.** This box is itself a VM (`/dev/sda` is a `QEMU HARDDISK`, host
  kernel `7.3.3-xanmod1`), so absolute MB/s are not comparable across campaigns;
  only within-boot comparisons here are used.

## Evidence

`/home/dev/disp/blkread/` - `run-attr-20261115T021745Z` (§2 style single-reader
arms with CPU/PSI/vmstat), `run-depth-20261115T022347Z` (§3),
`run-iso-20261115T022948Z` (§2), `run-stack-20261115T023429Z` (§3 repeat),
`run-ceil-20261115T024136Z` (§4). Probes `probe-attr.sh`, `probe-attr2.sh`,
`probe-iso.sh`, `probe-stack.sh`, `probe-ceil.sh`; controls `control4.sh` (working)
and `control5.sh`/`control6.sh`; analyzer `analyze_attr.py`. Instruments:
`posix_fadvise` drop validated by re-read timing, `sda` `diskstats` per arm,
guess-free image resolution from the live worker's open fds.

## 6. Per-request attribution (added same session, HEAD build with `CANG_BLK_TRACE`)

The prebuilt has no per-request trace, so this ran on a HEAD build with the
campaign's own instrumentation applied to a pristine `deps/libkrun` HEAD
(`v2.0.0-cang.21`, `9ff213f1`) as one commit, overridden with
`nix build .#cang --override-input libkrun-src git+file://…/fork-trace
--no-write-lock-file` (out path `zdsl6iqg…-cang-0.21.0`; no pin moved). Each
completion logs `len`, `pread_us` and `copy_us`; `logs/host.log` brackets every
arm with the trace's line count so lines can be sliced per arm.

| arm | readers | traced bytes | wall ms | pread s | copy s | pread % | copy % | copy MB/s | device busy ms |
|---|---|---|---|---|---|---|---|---|---|
| `one-a` | 1 | 626 MB | 5721 | 0.15 | 1.97 | 3 % | 34 % | 310 | 2280 |
| `one-b` | 1 | 610 MB | 9331 | 0.22 | 4.68 | 2 % | 50 % | 130 | 5063 |
| `eight-a` | 8 | 2106 MB | 44716 | 1.95 | 37.4 | 4 % | 84 % | 56 | 266917 |
| `eight-b` | 8 | 2078 MB | 34450 | 0.68 | 26.5 | 2 % | 77 % | 78 | 150475 |

`pread` is 2-4 % of every arm - the host side of the read is free, which is what
"host cache hot" predicts. **The staging copy into guest memory is 34-50 % of a
single-reader arm and 77-84 % of an eight-reader arm, and its throughput falls
from 130-310 MB/s to 56-78 MB/s as readers are added.** Request lengths are
mostly 4 MiB and 16 KiB (502 and 406 of the 1080 completions in `eight-a`).

That is what the concurrency collapse in §3 is made of, and it re-orders the
levers. A device that preads **straight into the guest's descriptor buffers**
does not do this copy at all: it replaces [pread -> host staging buffer] followed
by [staging buffer -> guest memory] with one pread into guest memory. The
in-fork version of the same idea is direct-to-guest reads, which the worker's own
correctness rule currently forbids for possibly-overlapping buffers (one
completion thread must write them, `cang-block-read-completion-serialization`).
So the honest recommendation is: **direct-to-guest reads with a proven
non-overlap test first, vhost-user-blk only as the fallback** - both buy the same
copy, and the fork is the cheaper place to prove it.

## 7. What this changes about the vhost-user question

The earlier "decline" was arithmetic on the campaign's attribution, which put the
cost guest-side. This trace puts 77-84 % of an eight-reader arm in the host's
**staging copy into guest memory**, which is exactly the work a backend that reads
into guest buffers removes. So the question is no longer "can a backend help" but
"which is cheaper: a fork-side direct-to-guest read with a non-overlap proof, or a
whole vhost-user backend". The fork side is one code path and keeps cang's single
`FsDevice`/`BlockDevice` topology, so it is where to measure first; a vhost-user
prototype would need a backend on this host (none built: only `qemu-ga` is
realised) plus the memfd-RAM flip, and would confound the RAM-backing axis.
