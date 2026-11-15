---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# An uncompressed btrfs lower does not speed up the metadata walk (and why note 17 did not transfer)

Built the lower uncompressed, end to end through cang, and measured it in the guest.
**No gain.** The change was reverted.

## 1. What was built and how

The state root's mount (`compress=zstd:1`) is what compresses the image, so the fix has to
be made at image creation. Two mechanisms were needed, and the second one is the one that
matters:

- pin the image file to `compression=none` before `mkfs.btrfs` writes into it (advisory:
  `btrfs property set -f <image> compression none` in the Buildah storage namespace), and
- **drop `--reflink`**. A reflinked image clones the *snapshot's* extents, so it inherits
  the snapshot's compression no matter what the image's own property says. Verified on a
  small tree: `--shrink` without reflink plus the property gives **0 encoded extents**;
  with `--reflink` it gives 256 encoded extents; without the property, 315.

With both, cang built `ec5b74e0….raw` at **303 extents, 0 `encoded`**, 12.4 GB apparent,
7.9 GB on disk (sparse) - a genuinely uncompressed lower.

## 2. The guest sees no improvement

Same probe, same 3 897-file cold `find`, five arms per image (guest `drop_caches` before
every cold arm):

The controlled pair is two boots with the **same binary** (md5 `880f5d7c…`), the first
reusing the compressed image and the second rebuilding it uncompressed:

| boot | image the launch attached | cold arms (ms) | mean | warm (ms) |
|---|---|---|---|---|
| `run-find-…T144322Z` | compressed reused (`ec5b74e0…`, 88 170 extents, 62 961 encoded) | 420, 461, 474, 500 | 464 | 88, 88 |
| `run-find-…T144803Z` | uncompressed rebuilt (303 extents, 0 encoded) | 411, 437, 438, 455 | 435 | 75, 81 |

So the uncompressed lower is **~6 % faster on the mean, inside this host's run-to-run
spread** (the same compressed image has measured 362-500 ms across the session), and warm
arms are unchanged at 75-88 ms. Earlier, less controlled pairs from other boots
(362-447 compressed against 411-455 uncompressed) pointed the other way; the honest
statement across all of them is **no reliable difference**. The host side agrees, on the
two artifacts directly, cold before each run, 16 KiB random reads over the first 2 GiB:

| image | mean µs | p50 µs |
|---|---|---|
| compressed | 388 / 257 | 321 / 248 |
| uncompressed | 510 / 288 | 334 / 193 |

So the compression of the *image* is not what the 316-400 ms metadata walk is made of.

## 3. Why note 17 measured 2.0-2.8x and this measures none

Note [17](17-per-read-cost-is-fs-compression.md) compared *fresh 2 GiB files of binary data*
on the same btrfs: compressed p50 240/215 µs against uncompressed 116/78 µs. This note
compares the actual image, and the difference is gone. The likely reasons, none of which
that experiment controlled for:

- the differences tracked **extent fragmentation** as much as decompression (5 802 extents
  in the compressed file against 2 in the uncompressed one) - and the image has 88 170
  extents compressed against 303 uncompressed with no latency change at all, which rules
  fragmentation out as the explanation too;
- the image is 12.4 GB sparse with large hole regions, so a random read over its first
  2 GiB mostly lands on holes or shared extents, while note 17's files were fully
  allocated;
- note 17's files were written by `dd` from the image, i.e. content that compresses badly
  but still had to be decompressed on the way out.

**The honest reading: the per-read compression penalty is real for freshly written,
fully-allocated data and does not reproduce on the lower image.** Note 17's number should
not be quoted for the image.

## 4. What does survive from the erofs comparison

Note [18](18-erofs-lower-metadata.md)'s erofs-plain result (240-250 ms against btrfs
362-447) was measured with a *loop-mounted file on the guest root*, i.e. an extra layer,
and it beat the btrfs lower anyway. If that advantage is real it is about the **metadata
layout** (denser directory/index stream, fewer read requests), not about compression -
which is consistent with this note finding no compression effect while erofs-plain wins
and erofs-lz4 does not.

## 5. Cost of the change that was reverted

Dropping `--reflink` makes the build copy the tree: the image stops sharing extents with
the snapshot it was built from and the build reads and writes ~12 GB (the image went from
mostly-shared to ~8 GB of its own on this disk). With no measured benefit, shipping that
is a pure loss, so `mkfs_args` keeps `--reflink` and the space preflight keeps pricing a
shared build at the floor. Both edits were reverted; the measurements above are the
deliverable.

## Evidence

`run-find-20261115T144322Z` (compressed, reused) and `run-find-20261115T144803Z` (the
uncompressed rebuild) - the controlled pair, both on binary `880f5d7c…`; also
`run-find-20261115T142654Z`/`T143511Z` for the earlier less controlled pairs,
`randlat` A/B against `compressed-live.raw` and the uncompressed `ec5b74e0….raw` (both
kept in `<state>/cang/microvm/images/btrfs-snapshots/nix-lower/`), `mkerofs3.sh`-era
artifacts for note 18. Probe `probe-find.sh`, runner `run-find.sh`, control `control11.sh`.
