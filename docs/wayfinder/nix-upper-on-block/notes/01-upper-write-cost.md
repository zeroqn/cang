---
label: wayfinder:note
ticket: 01-upper-write-cost
---

# What a nix-store-shaped write costs on the virtio-fs `/nix` upper vs the guest-local block disk

**Verdict.**

- **The ticket's threshold - `>= 20 %` off arm A on the store-write workload - is
  NOT cleared.** The only uncontended A/B data (two pairs, page cache cold,
  alternating lead) puts arm B **10.9 % faster** on the write (median 99 105 ms vs
  111 177 ms; paired -4.0 % and -17.6 %), which is under the 20 % the ticket asks
  for and inside the run-to-run spread of the three contended sets (+24.7 %,
  -6.5 %, +14.5 % - sign-unstable, [appendix](#discarded-appendix-three-contended-sets-not-comparable)).
  So: no evidence of a 20 % write win, and the write is not what a move buys.
- **What a move does buy is metadata and deletion, by an order of magnitude, and
  that now rests on the clean pair**: walking the written store 7.7x, `du -s`
  15.4x, deleting it 13.3x faster on the block disk, exactly as in the contended
  sets (6.1-8.7x / 15.0-17.1x / 13.2-17.2x). That is the map's own hypothesis -
  "the metadata cost is where a FUSE upper is expected to lose" - confirmed; the
  write half of it is not.
- **Shape caveat, stated up front**: the workload is **bandwidth-shaped at the
  path level** (12.06 MB per path against the harmony upper's ~1.2 MB), so the
  write number describes a store write dominated by a few large toolchains. The
  requested metadata-shaped arm could **not** be built from this image's store
  (685 paths total, and any *closed* ~3 GB subset of it collapses back to ~684
  paths) - see [Shape](#shape-of-the-workload-what-the-headline-number-actually-measures)
  and [The metadata pair that could not be a metadata arm](#the-metadata-pair-that-could-not-be-a-metadata-arm).

## Arms (as observed, not as designed)

Both arms write a **Nix chroot store** - `nix copy --to local?root=<dest>`, same
command, same source closure, same `nix` - through **kernel overlayfs**, over the
**same read-only lower** (`/run/cang/nix-lower`, the image's `/nix` on
virtio-blk), same options. The only variable is the filesystem under the upper.
Quoted from the guest's own `/proc/mounts`:

| arm | overlay the guest writes through | upper's filesystem |
|---|---|---|
| **A** (today) | `overlay /nix overlay ...,lowerdir=/run/cang/nix-lower,upperdir=/run/cang/nix-overlay/upper,workdir=/run/cang/nix-overlay/work-guest,redirect_dir=nofollow,userxattr` | `/run/cang/nix-overlay` is `virtiofs` (`stat -f`: `fuse`, magic `65735546`) - the host directory share |
| **B** (candidate) | `overlay /run/cang/upper-cost-cand ...,lowerdir=/run/cang/nix-lower,upperdir=<build disk>/upper-cost-<tok>/upper,workdir=.../work,redirect_dir=nofollow,uuid=on,userxattr` | btrfs on virtio-blk (`/dev/vdb` -> `/home/dev/.cache/cang-build`, magic `9123683e`) - `cang-build.raw` |

Arm A's destination is `/nix/.upper-cost-<tok>-A`, so its bytes travel the same
overlay upper the real `/nix/store` upper uses. It is a **proxy**, not the real
`/nix/store`: importing paths already registered there is a no-op, so the workload
is the store's own content re-written into a fresh store root. The overlay is on
**both** sides, so this is the deployment comparison, not "overlay vs no overlay".

## Pinned inputs

- repo `/home/dev/cang/cang`, branch `main`; HEAD `098d27d` for run 3 (`crates/`
  unchanged since `cf1b480`: `git diff --name-only cf1b480..098d27d | grep -c
  '^crates/'` is **0**).
- host CLI: `nix build .#cang` ->
  `/nix/store/gpvi3r00l0jx3lay3nyrw4wy405cq7k0-cang-0.20.1/bin/cang`, sha256
  `15d893984adb3dea...`, launched by absolute store path
  (`logs/proc-sample.txt`, 2010 samples).
- guest-init: `nix build .#cang-musl` ->
  `.../njc92xh7a1i5447gk3d5a95f44cb4mzs-cang-static-x86_64-unknown-linux-musl-0.20.1/bin/cang-guest-init`,
  passed as `--guest-init`. **Stated limitation:** the loaded image's own
  guest-init is 0.17.2, which predates block mode and the build disk (measured:
  `FATAL build-disk-not-mounted`, and the host's "predates block mode" warning),
  hence the override - the arms measure filesystem behaviour, not HEAD's
  guest-init.
- guest image: `localhost/cang:latest`, id
  `aeceb9750c143e2e562f88892224f901f18d63c8b7c7b38bf92232693dd39f18`, digest
  `sha256:99e4729ad92b0e38...`, 8.36 GB - **reused, not rebuilt** (`.#container`
  unrealised, `/nix/store` at 6.9 GiB free). Both arms boot the same image.
- guest: `7.3.1-hardened1`, 26 vCPUs, 8 149 992 kB RAM, nix 2.34.9 (daemon),
  `--mem 8 --alloc hardened --seccomp=off --landlock=off --root`, default
  `--nix-lower=auto` (block lower engaged: `/dev/vdc on /run/cang/nix-lower`).
- host: state root on btrfs, `compress=zstd:1,ssd,discard=async,space_cache=v2` -
  both arms' bytes end up compressed by the host btrfs underneath.

## Workload

Frozen before timing and identical in every arm:
`RESULT store kb=8223056 dirs=685` - the image's whole store, **685 valid paths /
8.26 GB**, manifest `nix path-info --all | sort` (the DB's own valid path list,
not a `find`), digest
`ecdf3c26555fec02532002d464bf7e357417880b2189a2caad05ea94f68e43e3`, copied
offline (`--offline --no-check-sigs`) from the read-only lower. Re-read set: the
40 largest files between 8 and 64 MB, `RESULT readset files=40
bytes=1402503244` (1.31 GiB), chosen once and mapped into each destination, every
arm hash-verified against the source.

## Shape of the workload (what the headline number actually measures)

Measured on the host from the image's own root filesystem
(`state/cang/microvm/images/btrfs-snapshots/sha256-99e4729a.../rootfs/nix/store` -
the very tree the guest's read-only lower is built from), over exactly the 685
paths of the frozen manifest:

| | **primary manifest** (the headline) | the attempted metadata set | harmony upper (the target) |
|---|---|---|---|
| store paths | 685 | 684 | 2 501 |
| total | 8.26 GB (`du` 8 223 056 KiB) | 7.98 GB | 3.0 GB |
| **bytes per path (mean)** | **12.06 MB** | **11.67 MB** | **~1.2 MB** |
| per-path median / p90 / max | 533 kB / 13.6 MB / 852 MB | - | - |
| files | 49 115 | 49 066 | - |
| files < 4 KiB | 27 215 (55.4 %) | - | - |
| files 4-64 KiB | 17 489 (35.6 %) | - | - |
| files 64 KiB-1 MiB | 3 702 (7.5 %) | - | - |
| files > 1 MiB | 709 (1.4 %) | - | - |
| median file / p90 / p99 / max | 3 217 / 54 443 / 1 498 112 / 639 150 592 B | - | - |

- **By bytes the headline is bandwidth-shaped.** 259 of the 685 paths are over
  1 MiB and carry **8.14 GB of the 8.26 GB (98.6 %)**; the other 426 paths carry
  118 MB between them. At 12.1 MB per path it is **10x fatter than the harmony
  upper**, so the write number answers "what does a store write dominated by a few
  large toolchains cost", not "what does a realistic upper cost".
- **By file count it is metadata-heavy anyway**: 49 115 files at a 3 217 B median,
  91 % under 64 KiB - ~49 000 creations, renames and `setxattr`s give a FUSE upper
  plenty of per-node round trips even here, which is why the metadata walk and the
  delete separate the arms so sharply while the write does not.

## The metadata pair that could not be a metadata arm

The extra pair ran **after** the primary set with the same harness: the smallest
paths of the store that fit in 3 GiB, closure-expanded, cold cache, alternating
lead (`meta-1-A, meta-1-B, meta-2-B, meta-2-A`).

**It is not a metadata-shaped workload, and it cannot be one with this image.**
670 roots chosen by size expand through `nix path-info -r` to **684 paths /
7.98 GB** - store references pull the large toolchains straight back in - i.e. the
same bandwidth shape as the primary set (11.67 MB per path). A genuinely
metadata-shaped set needs ~2 500 paths, and this image's store has **685**; no
closed ~3 GB subset of it can be metadata-shaped. So, as instructed, this is not
reported as a metadata arm. It is still the only **uncontended** A/B data taken,
so it is the primary evidence below - on the bandwidth shape.

## The clean pair (admissible): 4 arms, no foreign VM

Window 14:33-14:48 (+0800); the contamination sampler logged **nothing** after
14:31:39, so all four arms ran alone on the host.

| # | arm | write_ms | flush | find all files (49 066) | du_ms | read_ms (3.85 GB) | rm_ms |
|---|---|---|---|---|---|---|---|
| 1 | A | 109 850 | 33 | 14 134 | 38 976 | 20 527 | 55 089 |
| 2 | B | 105 483 | 154 | 1 704 | 2 481 | 23 200 | 3 977 |
| 3 | B | 92 728 | 183 | 1 923 | 2 535 | 24 362 | 4 354 |
| 4 | A | 112 504 | 24 | 13 681 | 38 065 | 21 768 | 55 279 |
| | **median A** | **111 177** | 28 | 13 907 | 38 520 | 21 147 | 55 184 |
| | **median B** | **99 105** | 168 | 1 813 | 2 508 | 23 781 | 4 165 |

- write: **B 10.9 % faster** (paired: rep 1 -4.0 %, rep 2 -17.6 %) - **below the
  20 % threshold**, and with two repetitions per arm this is a low-power estimate,
  not a decisive one.
- metadata walk **7.7x**, `du` **15.4x**, delete **13.3x** in B's favour.
- cold re-read of the 40 largest files (3.85 GB - a *different*, larger read set
  than the primary's 1.31 GiB, so this figure is not comparable with the appendix)
  is **12.5 % slower** on B here.
- `flush_ms` (a `sync` immediately after the copy) is **25 ms on A against 168 ms
  on B** - the FUSE upper hands the commit to the host and returns.

All four rows: `rc=0`, 684 paths, identical bytes, `read_sha_match=yes`.

## Discarded appendix: three contended sets (not comparable)

None of these satisfies the ticket's "no concurrent runs" condition. They are
kept as evidence that contention matters: same harness, same manifest, three
co-tenant windows, write ratios of unstable sign.

| set | window (+0800) | contamination | median A write | median B write | B vs A |
|---|---|---|---|---|---|
| run 1 | 13:19-13:37 | 140/140 samples `FOREIGN_VMS=2` | 105 249 ms | 131 297 ms | **+24.7 %** |
| run 2 | 13:44-14:03 | clean gate, `FOREIGN_VMS=2` from 13:45:57 (128/128) | 103 727 ms | 96 970 ms | **-6.5 %** |
| run 3 (primary set) | 14:11-14:32 | `FOREIGN_VMS=2` 14:10:55-14:31:39 (240/241 samples) | 136 246 ms | 155 953 ms | **+14.5 %** |

Per-repetition values (all six arms of run 3, all void):

| # | arm | write_ms | flush | find (metadata walk) | du_ms | read_ms | rm_ms |
|---|---|---|---|---|---|---|---|
| 1 | A | 161 770 | 126 | 14 273 | 38 977 | 12 071 | 52 563 |
| 2 | B | 172 914 | 204 | 3 821 | 2 286 | 6 440 | 3 823 |
| 3 | B | 155 953 | 131 | 1 641 | 3 058 | 12 782 | 3 437 |
| 4 | A | 136 246 | 22 | 13 014 | 38 675 | 7 846 | 58 984 |
| 5 | A | 133 398 | 24 | 15 530 | 43 403 | 6 782 | 55 346 |
| 6 | B | 81 854 | 159 | 1 542 | 1 946 | 4 178 | 3 491 |

Run 1 and run 2's per-repetition rows are in the previous revision of this note's
history (`logs/driver-run1.out`, `logs/driver-run2.out`); run 3's row 6 is the
instructive one: arm B's write is 81.9 s there against 156-173 s in its own two
preceding arms, because the co-tenant left the host at 14:31:39 during that arm -
which is precisely why a contended number is worse than no number.

## Contamination

The gate is the one `cang-vm-process-identity-and-exclusivity-gate` prescribes -
the VM worker's argv minus this session's state dir; `pgrep -x cang` reads 5 on an
idle host and is not a VM count. The sibling campaign is
`/nix/store/07hym9hyrrwj5ivdh4pbdl7186l4gsfv-cang-0.20.1/bin/cang internal
libkrun-vm-worker-enter /home/dev/disp/nix-develop-blk/run1/state/...`.

- Run 3's gate cleared at 14:10:42 (`preflight comm_cang=0 foreign_workers=0`) and
  the campaign returned **5 s after the boot started** (14:10:55), which voided
  the whole primary set. It left at 14:31:39, which is what makes the extra pair
  admissible.
- Evidence gap, stated: run 1's contamination log was truncated by run 2's start
  (now fixed - `run.sh` writes `contamination-$RUNID.log`); run 1's counts are the
  live readings from that run, and its driver log survives.

Reproduce the gate (read-only):

```sh
SELF=/home/dev/disp/nix-upper-cost/state
pgrep -af 'libkrun-vm-worker-enter' | grep -v "$SELF"   # must be empty before a scored boot
pgrep -x cang | wc -l                                    # 5 on an idle host: not a VM count
```

## Interpretation and caveats

1. **A's write time is the guest's view, not the host's work.** Arm A's writes are
   served by the FUSE server on the host, which commits them to the host btrfs and
   returns, so the guest sees 24-33 ms of `sync`; arm B's own btrfs journals,
   checksums and allocates, which is real in-guest work (154-183 ms of `sync`
   after the same write). Arm A's deferred cost plausibly surfaces in the steps
   that follow - its metadata walk and `du` are 7-15x slower even where its writes
   were faster - but that is an inference from the timings, not a measured host
   commit time.
2. **The write estimate is low-power**: two uncontended repetitions per arm, and
   the contended sets show arm B's write ranging 81.9-172.9 s. If the exact number
   matters, more clean repetitions are the cheap next step, not a bigger workload.
3. **`overlayfs: upper fs does not support tmpfile`** is logged at boot for arm A's
   overlay (the virtio-fs upper); the btrfs upper supports it.
4. Both arms' bytes land on the same host btrfs (the upper directory for A, the
   `cang-build.raw` file for B), so host compression and any co-tenant I/O hit
   both.
5. Nothing was built or loaded for this ticket: no `podman`, no image build, no
   nix store growth beyond the two flake outputs (`.#cang`, `.#cang-musl`) already
   present; nothing outside `/home/dev/disp/nix-upper-cost/` was written or
   deleted.

## Verdict

- **`>= 20 %` off arm A on the store-write workload: NOT cleared.** The clean
  estimate is **10.9 %** in B's favour (2 pairs, -4.0 % and -17.6 %), and the
  three contended sets straddle zero (+24.7 %, -6.5 %, +14.5 %). A store write is
  not what moving the upper to the block disk buys.
- **Metadata and garbage collection are an order of magnitude cheaper on the block
  disk**, in the clean pair (walk 7.7x, `du` 15.4x, delete 13.3x) and in every
  contended arm of all three earlier sets. A substitution-plus-GC cycle is
  therefore cheaper on arm B even when its write half loses: run 1 measured
  155.6 s against 134.7 s (13.4 %), and the clean pair's write+delete is 166.4 s
  (A) against 103.3 s (B).
- For the map: the metadata/GC/ENOSPC/one-disk argument stands on its own and is
  now uncontended; a write-throughput argument for the move does not exist, and if
  the write needs to be pinned down it needs clean repetitions, not a bigger
  workload.
- **Not measured, and not measurable with this image: a metadata-*shaped* upper**
  (2 500 paths / 3 GB). The workload here is 685-684 paths at 12 MB each; the
  harmony upper at 1.2 MB per path remains the shape nobody has driven through
  this harness.

## Raw evidence

Out dir `/home/dev/disp/nix-upper-cost/` (not committed):

| path | what it is |
|---|---|
| `logs/driver.out` | run 3: pins, `preflight comm_cang=0 foreign_workers=0`, `gate clear`, `boot done rc=0`, `rows=6 ok=6`, `meta_rows=4 meta_ok=4`, `sentinel=present` |
| `logs/driver-run1.out`, `logs/driver-run2.out` | the first two sets' driver logs |
| `results.txt`, `results-run1.txt`, `results-run2.txt` | the `RESULT` rows of each set (run 3 carries the four `RESULT stage=meta` rows) |
| `logs/contamination-20261109T060833Z.log` | run 3: 241 samples, 240 at `FOREIGN_VMS=2`, 14:10:55-14:31:39 (+0800); `logs/contamination.log` symlinks it |
| `logs/contamination-run2.log` | run 2: 128 samples, all `FOREIGN_VMS=2` (run 1's was truncated - see above) |
| `logs/proc-sample.txt` | launched-binary identity while alive |
| `logs/vmm/ws-<task>.helper.stderr.log` | VMM logs: the overlayfs messages and the `drop_caches` lines; **no opcode histogram** in this build, so FUSE request counts are not quotable |
| `mounts.txt`, `trace.txt` | the guest's mount table and its `RESULT` mirror |
| `ws/evidence/` | per-arm scratch: `r<rep>-<arm>.*`, `meta-<rep>-<arm>.*`, `manifest.txt`, `manifest.sha256`, `meta-manifest.txt`, `meta-roots.txt`, `read-src.txt`, `nix-prep.log` |
| `run.sh`, `ws/smoke/run-guest-arms.sh` | the harness: host runner (gate, sampler, VMM-log copier, PTY boot) and the in-guest arms |

Run 3: `boot start` 06:10:50Z, `runner end rc=0 rows=6 ok=6 meta_rows=4 meta_ok=4`
06:49:21Z; `df` 75 G free afterwards; no surviving VM worker of this session.

