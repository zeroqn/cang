---
label: wayfinder:note
ticket: 01-upper-write-cost
---

# What a nix-store-shaped write costs on the virtio-fs `/nix` upper vs the guest-local block disk

**Status: in progress** - the harness is written and pinned; the arm set is
running. Numbers land below the line "## Numbers" when the guest finishes.

## Question (from the ticket)

Every other ticket on the map hangs on one number: what it costs today to write
a Nix store into the **virtio-fs** `/nix` upper, against the same writes landing
on the **guest-local btrfs disk** the workspace already carries. No cang code is
changed for this; the decision to build `upper:blk` is made on this number, with
a recommended threshold of **>= 20 %** off arm A on the store-write workload.

## Arms

Both arms write through **kernel overlayfs with the same lower and the same
options**, and differ only in the filesystem under the upper - which is the
deployment comparison, not "overlay vs no overlay":

| arm | overlay lower | overlay upper (`upperdir`) | transport |
|---|---|---|---|
| **A** (today) | `/run/cang/nix-lower` (ro btrfs image, virtio-blk) | `/run/cang/nix-overlay/upper` = host directory over **virtio-fs** | virtio-fs |
| **B** (candidate) | `/run/cang/nix-lower` (same) | `<build disk>/upper-cost-<tok>/upper` on the **guest-local btrfs** (`cang-build.raw`, virtio-blk) | virtio-blk |

- Arm A's destination is a Nix **chroot store** rooted at
  `/nix/.upper-cost-<tok>-A`, so its bytes travel the overlay upper the real
  `/nix/store` upper uses. It is *not* the real `/nix/store`: importing paths
  that are already registered there is a no-op, so the proxy is the store's own
  content re-written into a fresh store root. Same nix code path in both arms
  (`nix copy --to local?root=...`), so the only variable is the filesystem.
- Arm B's destination is the identical chroot store through the candidate
  overlay (`/run/cang/upper-cost-cand/.upper-cost-<tok>-B`), mounted in the guest
  with `lowerdir=/run/cang/nix-lower,upperdir=<build disk>/upper,workdir=...,userxattr`
  - the recipe ticket 02 recorded. If that mount fails the guest falls back to a
  plain btrfs directory on the build disk and says so in `RESULT armB_dest`.

## Pinned inputs

- repo `/home/dev/cang/cang`, branch `main`, `HEAD` at run start: recorded in
  `logs/driver.out` (`head=` line). The host CLI is the tree's own
  `nix build .#cang` output, launched by absolute store path; its sha256 is in
  `logs/driver.out`.
- guest image: `localhost/cang:latest`, id `aeceb9750c14...` - the image already
  loaded in this host's containers storage. **Not rebuilt**: `.#container` is not
  realised and `/nix/store` had 6.9 GiB free, so building and loading a fresh
  8.4 GB image would not fit. Both arms boot that one image, so the comparison is
  unaffected; the limitation is that the measurement does not carry a
  fresh-from-HEAD image id.
- guest kernel: recorded per run (`RESULT boot kernel=...`).
- host: btrfs state root with `compress=zstd:1` (recorded in `logs/driver.out`).

## Workload (one manifest, frozen before timing)

The image's own store, measured once (`RESULT store`, `RESULT workload`): the
largest single closure under 3.5 GiB, or the whole store when the store is
smaller than 6 GiB and the largest closure is under 1.5 GiB. The frozen manifest
is `manifest.txt` (sorted store paths) + `manifest.sha256`; the write set is that
closure, copied by `nix copy --to local?root=<dest>` (offline, no substituters).

## What is recorded per arm, per repetition

Six arms in one guest, alternating order **A1 B1 B2 A2 A3 B3**, guest page cache
dropped before every timed step:

- `write_ms` - `nix copy` wall time for the frozen manifest,
- `flush_ms` - a following `sync`,
- `find_nix_ms` - cold metadata walk (`find <dest>/nix/store -name '*.nix'`),
- `du_ms` - cold `du -s -k` over the written store,
- `read_ms` - cold re-read (sha256) of the **same 40 files** (largest in the
  manifest, chosen once from the source), with `read_sha_match=yes` meaning the
  hashes equal the source's,
- `clean_ms` / `rm_ms` - deleting the previous/this copy (outside the timed write).

## Raw evidence

`/home/dev/disp/nix-upper-cost/` (not in the repo; 21 GB-class logs):

- `logs/driver.out` - runner log, gate counts, host/image/kernel pins
- `logs/contamination.log` - foreign-VM samples during the run (gate fingerprint:
  `libkrun-vm-worker-enter` argv minus this session's state dir)
- `logs/cang-arms.console` - the guest console of the run
- `logs/proc-sample.txt` - launched-binary identity while alive
- `logs/vmm/<task>.helper.stderr.log` - polled VMM log (virtio-fs opcode
  histogram; proves arm A's requests actually crossed virtio-fs)
- `results.txt`, `trace.txt`, `mounts.txt` - copied out of the guest
- per-arm scratch in the guest's `/workspace/evidence` (`r<rep>-<arm>.*`),
  copied to `logs/guest-evidence/`

## Numbers

(to follow)

## Verdict

(to follow)

