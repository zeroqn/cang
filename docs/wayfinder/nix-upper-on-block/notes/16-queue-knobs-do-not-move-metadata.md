---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The queue knobs do not touch cold metadata; host service time per read does

One boot, eight cold `find` arms over the same 3897-file tree on the block lower, no
profiler (perf cost 900-1900 ms on this arm, note [15](15-cold-metadata-is-device-reads.md)),
sweeping `read_ahead_kb` and `max_sectors_kb` and recording the guest's own device
counters before and after every arm.

## 1. Every arm did exactly the same I/O

**941 read I/Os and 30 112 sectors (14.7 MB) in every single arm**, whatever the knobs
were set to (read-ahead 128 / 1024 / 2048 / 4096 KB; `max_sectors_kb` 512 / 1024 / 4096).
Readahead neither merges nor removes these reads: they are already issued as btrfs
metadata blocks, above the block layer's readahead.

| arm | read_ahead_kb | max_sectors_kb | wall ms | device read_ms | non-device ms | µs per device read |
|---|---|---|---|---|---|---|
| `base1` | 128 | 4096 | 517 | 260 | 257 | 276 |
| `ra1024` | 1024 | 4096 | 555 | 297 | 258 | 315 |
| `ra4096` | 4096 | 4096 | 845 | 466 | 379 | 495 |
| `base2` | 128 | 4096 | 438 | 221 | 217 | 234 |
| `ms512` | 1024 | 512 | 888 | 498 | 390 | 529 |
| `ms1024` | 2048 | 1024 | 375 | 188 | 187 | 199 |
| `ra1024b` | 1024 | 1024 | 404 | 201 | 203 | 213 |
| `base3` | 128 | 1024 | 378 | 186 | 192 | 197 |

## 2. What actually moved: host service time, not the knobs

Device service time for identical work spans **186-498 ms (2.7x)** and the wall tracks it
(`wall ≈ device read_ms + 187-390 ms`). It shows no monotone dependence on either knob
and no trend across arm order: the two arms with the *same* settings (`base1` 517 / 260,
`base2` 438 / 221) differ by 18 %, while `ra4096` (128 → 4096 KB read-ahead) is 845 ms
and comes between them. The differences are host-side variance in serving 941 small
reads from the `.raw` image - which lives on a btrfs filesystem that was **93 % full**
for this entire session.

So the honest reading: **these knobs are not levers for cold metadata**. Per read the
host charges 197-529 µs, and the guest's own non-device work is a stable 187-390 ms
(3897 directory entries and 941 completions).

## 3. Where that leaves the question

Two things follow, both on the image/host side rather than in the guest:

- **Reduce the number of metadata blocks** (941 for 3897 files, i.e. one block per ~4
  files): an image layout with a denser directory/index stream - erofs or squashfs over
  the same tree - removes reads instead of trying to batch them.
- **Make each read cheaper**: 197-529 µs per read is slow for a local device, and the
  2.7x spread within one boot suggests extent locality or host load matters. Defragmenting
  or rebuilding the `.raw` on a filesystem with free space would test that directly, and
  until it is tested no guest-side number here is stable to better than ~2x.

## 4. A defect in this run's own labels

`base1`/`base2`/`base3` were intended to be stock baselines but the probe set
`read_ahead_kb=128` for them, while the image's stock value is **8192** (recorded in
`setup.txt`). Because the I/O count and volume are invariant across 128-4096 KB, this
does not invalidate the invariance result - but the "baseline" label is wrong, and a
re-run should take the stock value as its baseline arm.

## Evidence

`run-meta2-blk-20261115T130857Z` (`workspace/evidence/meta.txt` for the eight arms with
device counters, `setup.txt` for the stock queue defaults and knob writability - the
queue is writable as root in the guest, `progress.txt` per arm). Probe `probe-meta2.sh`,
runner `run-meta2.sh`, control `control10.sh`.
