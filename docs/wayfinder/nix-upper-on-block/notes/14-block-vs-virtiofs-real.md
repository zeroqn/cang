---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Real workload, clean host: the block lower beats virtiofs 2-3x on cold mmap reads

**Verdict.** With the host unpressed, the same bytes read through mmap page faults (the
shape `exec`, `ld.so` and `nix build` actually use), the block lower is **2.1-3.0x
faster at one reader and 1.7-3.9x at six**. The two modes returned identical content
(identical checksums), so this is the same work done twice.

| mode | arm | readers | MB | wall ms | MB/s | checksum | device reads |
|---|---|---|---|---|---|---|---|
| `blk` | `mm1a` | 1 | 128 | 51 | 2491 | 4091916 | 52 (132 MB) |
| `blk` | `mm1b` | 1 | 128 | 46 | 2777 | 4091916 | 56 |
| `blk` | `mm1c` | 1 | 128 | 48 | 2682 | 4091916 | 57 |
| `blk` | `mm8a` | 6 | 768 | 216 | 3547 | 19593189 | 290 (793 MB) |
| `blk` | `mm8b` | 6 | 768 | 190 | 4045 | 19593189 | 294 |
| `virtiofs` | `mm1a` | 1 | 128 | 144 | 889 | 4091916 | n/a |
| `virtiofs` | `mm1b` | 1 | 128 | 106 | 1213 | 4091916 | n/a |
| `virtiofs` | `mm1c` | 1 | 128 | 102 | 1258 | 4091916 | n/a |
| `virtiofs` | `mm8a` | 6 | 768 | 758 | 1013 | 19593189 | n/a |
| `virtiofs` | `mm8b` | 6 | 768 | 367 | 2093 | 19593189 | n/a |

Metadata sweep (`find <lower>/store -maxdepth 3 -type f`, 3897 files) on the block
lower: **309 ms cold, 54 ms warm** - a 5.7x cold penalty for metadata alone, which no
read-path change addresses.

## Host conditions, recorded per boot

`run-real-blk-20261115T124745Z`: `MemFree` 12.0 GB, `AnonPages` 1.5 GB (gate: refuses
below 4 GiB `MemFree` or with any live `cang --mem`). Both boots ran on
`/nix/store/4kjxfzpg…`, 6 GiB guest, same 6 slices, `control10.sh` dropping every
`nix-lower` image per arm and proving each drop with `mincore`.

## Three instrument defects this comparison exposed

1. **The mmap arm measured nothing at first.** `acc += m[o]` with `acc` never read is
   dead code: gcc deleted the whole touch loop, so a 128 MB arm finished in 2.4 ms at
   "53 GB/s" and did 0 device reads. Fixed by accumulating into a `volatile` and
   emitting `sum` in the JSON - which then doubles as a content check across arms.
2. **The device counters were missing** (`--dev` not passed), so early arms looked
   cold-invalid when they were simply unattributed. Fixed; a cold arm now states its
   own device reads and a zero would mark it invalid.
3. **The metadata sweep fell back to the wrong root**: in virtiofs mode
   `/run/cang/nix-lower` does not exist, so `find $LOW/store` returned 0 files. Fixed
   to use the resolved root; the virtiofs metadata arm in this run is invalid and is
   *not* reported as data.

## What this settles and what it does not

- Settles the block-vs-virtiofs question for cold reads on a clean host: keep the block
  lower for `/nix`. This is the decision the effort actually needed.
- Does not measure a build or an exec sweep end to end, and does not measure
  concurrency beyond six readers; the metadata cold penalty (309 ms) is unaddressed by
  any read-path lever and is a separate question.

## Evidence

`run-real-blk-20261115T124745Z`, `run-real-virtiofs-20261115T125058Z`
(`logs/env.txt` for gates and hashes, `logs/samples.tsv` for host conditions,
`workspace/evidence/arms.jsonl` for per-reader detail, `phases.txt` for the metadata
sweep). Instruments `readbench.c --mmap --dev` (checksum-bearing), `control10.sh`,
`probe-real.sh`, `run-real.sh`, `seq-real.sh`.
