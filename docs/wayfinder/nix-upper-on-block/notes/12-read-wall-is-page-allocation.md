---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Where the buffered-read wall actually is: page allocation, and a pre-touch halves it

Two boots on `/nix/store/4kjxfzpg…` (cang HEAD + trace hunk, `--nix=lower:blk`,
6 GiB, 128 MiB per reader, 4 slices of the two largest lower files).

## 1. The host is not the limit, and its page cache never holds the image

| arm | readers | MB | wall ms | MB/s | guest device read_ms | device MB | host sda moved | host `mincore` after arm |
|---|---|---|---|---|---|---|---|---|
| `c1` | 1 | 128 | 786–1053 | 122–163 | 210–286 | 130 | 0–1 MB | 0 of 3026176 pages |
| `c4` | 4 | 512 | 3697 | 138 | 9705 | 536 | 0 MB | 0 pages |
| `p4` | 4 | 512 | 5835 | 88 | 8913 | 547 | 0 MB | 0 pages |

The guest's own device counters say the backing served 130 MB in 210–286 ms
(~450–620 MB/s) in every single-reader arm, while the host's state device moved
under 1 MB and `mincore` on the image stayed at **0 resident pages before and after
every arm**, including arms that read 512 MB. So: the image is served without
populating the host page cache at the path we can see, and the host is *not* where
the time goes — 210 ms of a 786–1053 ms arm.

## 2. Cached re-reads are CPU-bound, and a cached pass still costs 148–580 ms

| arm | readers | drop | MB | wall ms | MB/s | guest device reads | CPU ms |
|---|---|---|---|---|---|---|---|
| `c1` | 1 | guest + host | 128 | 1053 | 122 | 53 | 24 |
| `w1` | 1 | none | 128 | 580 | 221 | **0** | 576 |
| `w2` | 1 | none | 128 | 148 | 867 | **0** | 144 |

`w1`/`w2` did **zero** device reads, so these are pure page-cache-to-buffer copies:
221 MB/s and 867 MB/s, entirely CPU (`cpu_ms` 576 and 144 of 580 and 148 ms).
That is the floor any backing must beat, and it is a *copy* cost, not a device cost.

## 3. Pre-touching guest RAM roughly halves the cold arm

Same bytes, same drop, the only change being that ~3 GiB of guest RAM was written and
freed first so every page the guest later hands to its page cache had already been
faulted on the host:

| arm | when | MB | wall ms | MB/s | guest device read_ms | device MB |
|---|---|---|---|---|---|---|
| `c1` | before pretouch | 128 | 1053 | 122 | 286 | 130 |
| `p1` | after pretouch | 128 | 690 | 186 | 219 | 130 |
| `p1b` | after pretouch | 128 | 494 | 259 | 220 | 130 |

Device service is unchanged (219–286 ms); the *rest* of the arm falls from 767 ms to
471 ms to 274 ms. So a large part of the cold cost is first-touch host work for the
pages that become guest page cache, and pre-faulting guest memory addresses it -
which is what cang's held `CANG_GUEST_RAM_POPULATE` prototype does.

## 4. What this means for the block-vs-backend question

- The block device hop is 210–290 ms of a 786–1053 ms single-reader cold arm and its
  service is ~500 MB/s. A vhost-user-blk backend cannot remove guest page allocation
  or the cache-to-buffer copy, and at depth its extra parallelism does not help either
  (note 11: buffered depth does not scale, tmpfs included).
- The only mechanism that removes the measured cost by construction is **DAX** -
  mapping host pages straight into the guest so the guest neither allocates nor copies
  them. That is already charted in `workspace-virtiofs-performance` (notes 58-60,
  tickets 22-24), and it is the "passthrough" that fits this measurement; FUSE-level
  passthrough does not, because the copy and the page fill are in the guest either way.
- `O_DIRECT` arms (note 11: 426 MB/s single, 3029 MB/s at seven readers) measure the
  transport ceiling only. They are not a candidate fix: the guest application's
  `open()` flags are not something cang can impose on `nix build`, `exec`, or a
  workload's own `read()` calls.

## Evidence

`run-alloc-20261115T051416Z` (host sampling of every block device plus
`/proc/meminfo` and `/proc/vmstat` every 100 ms during each arm, `logs/samples.tsv`),
`run-touch-20261115T051803Z` (cold / warm / warm, 3 GiB pretouch, cold again;
`workspace/evidence/gvm.txt` has the guest's own `pgalloc_normal`/`pgfault` deltas per
arm - 55k pages allocated in `c1` against 21k in `w1`). Instrument `readbench.c`
(`--direct`, `--no-drop`), `cacheproof.py`, `control9.sh`, `probe-alloc.sh`,
`run-touch.sh`.
