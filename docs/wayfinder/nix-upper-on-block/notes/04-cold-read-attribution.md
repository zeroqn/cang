---
label: wayfinder:note
ticket: 01-upper-write-cost
---

# Cold data reads on the `/nix` block lower: what the 12.5 % re-read cost actually is

**Verdict.**

- **Cold data reads are not a cost of the block lower's read path.** Two live
  guests (block lower, then `--nix=lower:virtiofs`), same image, same 20-file
  manifest (2.56 GB, sha256-identical in both arms), same 4 GiB guest: every cold
  pass lands at **70-165 MB/s on both arms** (block 92 / 113 / 89 / 91 / 87 / 93
  MB/s across five `dd` passes, virtio-fs 76 / 93 / 89 / 82 / 97 / 87). They are
  not two paths, they are one wall — the host's storage.
- **Every guest-side lever is a no-op at that wall.** `read_ahead_kb` 8192 →
  32768 → 65536 (2.4 → 2.6 → 2.9 MiB per request): 91 / 87 MB/s. `max_sectors_kb`
  4096 → 2048 → 512: 91 / 107 / 154 MB/s, ordering unstable across repeats.
  4 and 8 parallel `dd` streams: block 91 MB/s and 74-246 MB/s (sign unstable),
  virtio-fs **71 and 70 MB/s, and 45 MB/s for 8 streams** — parallel readers
  *hurt* the FUSE arm. Nothing in the guest moved a cold read by more than noise.
- **When the host already holds the bytes, the block lower is the faster path**:
  the same manifest read with `iflag=direct` at the end of a boot takes
  **2.78 s (921 MB/s) on the block arm against 4.39 s (583 MB/s) on virtio-fs**
  (run 1), 2.92 vs 3.18 s in run 2. The guest stack is not what makes a cold read
  slow; it has 6-10x headroom over the wall.
- **What the lower does buy is metadata, by 6-9x, and that reproduces every
  boot**: `find /nix/store -name '*.nix'` **1.53 s / 3.67 s** on the block arm
  against **14.9 s / 14.1 s** over virtio-fs, `du -s /nix/store` 1.52 / 3.89 s
  against 11.3 / 11.3 s, with `du` agreeing to 7 MB on 8.2 GB. The cold-read
  "cost" carried forward from ticket 01/07 is inside the noise of that trade.
- **The recorded 12.5 % (and 8932 vs 5894 ms) is a host-page-cache artifact of the
  smoke, not a stack cost.** The smoke's cold-read arm (`find … | head -20`
  inside the timer, "first 20" paths rather than the largest, a *different* list
  per arm, guest `drop_caches` only) cannot attribute anything: in these runs the
  same arm varied 22.7-37.3 s (block) and 16.0-56.8 s (virtio-fs) between
  adjacent passes with no change of code or flags.

**Mechanism.** Host page cache and guest page cache are the same RAM. The lower
image is 7.90 GiB apparent with **276 MiB exclusive and 7.62 GiB reflink-shared**
with the container image store (`btrfs filesystem du`), i.e. it costs no host
space but it is a *second inode view* of the same extents, so the container
store's warm pages do not serve a guest read of it. On a 17 GiB host with a 4 GiB
guest, a pass that fills the guest's own page cache evicts the image's host pages
— which is why consecutive passes at fixed flags came out 22.7 s, then 18.1 s,
then 27.2 s, and why the two arms coincide only when cold and separate (block
ahead) only when the host still holds the bytes.

**What would move a cold read, if anything.** Only two things, neither in the
guest read path:

1. **Fewer bytes off the host storage.** The image's blocks are already
   `zstd:1` on the host (`compress=zstd:1` on the filesystem), inheriting the
   store's own encoding through `--reflink`. A higher level or a read-only
   compressed filesystem (erofs/squashfs) buys at most ~20-30 %, and costs the
   reflink sharing and the build time.
2. **Host cache residency** — worth ~6x (90 → 900 MB/s) — but it is RAM that the
   4 GiB guest is simultaneously filling, and the image is 7.9 GiB. Not something
   a launch flag can fix.

So: do not spend guest-side work on this. The block lower is ahead on metadata by
6-9x, level on cold data, and ahead on warm data; what the corpus should stop
carrying is the 12.5 % "cold re-read regression", which is not a property of the
block lower.

**Evidence.** `/home/dev/disp/blkread/` — `run-20261109T164024Z/{block,virtiofs}/workspace/evidence`
(run 1, 13 arms per boot), `run2-20261109T165325Z/…` (run 2, interleaved
base/p8/max_sectors arms with absolute per-phase timestamps), `probe.sh` /
`probe2.sh` (guest probes), `run.sh` / `run2.sh` (booters: `--mem 4 --alloc
hardened --seccomp=off --landlock=off --root`, `cang 0.21.0`,
`localhost/cang:latest`), `parse.py`. Every boot: 0 `csum failed`, 0 `I/O error`
on all arms.
