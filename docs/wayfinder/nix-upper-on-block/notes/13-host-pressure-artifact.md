---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Correction: the ~200 MB/s "wall" was host memory pressure, not the /nix lower

**Verdict.** Notes [11](11-guest-read-path-ceiling.md) and
[12](12-read-wall-is-page-allocation.md) describe a host that was short of memory, not
a property of cang's block lower. On the same binaries, same guest kernel, same
slices, with the host at ~11 GB free instead of ~2 GB, a cold buffered read of 128 MiB
takes **38-70 ms (1.8-3.4 GB/s)** and a four-reader 512 MiB arm **148 ms
(3.45 GB/s)** - at device speed, not at 122-163 MB/s.

## 1. The evidence that the host differed

The per-arm host sampler had `meminfo` in every line; comparing the two boots:

| boot | host `MemFree` | `MemAvailable` | `AnonPages` | 128 MiB cold arm |
|---|---|---|---|---|
| `run-alloc-…T051416Z` | 1.6 GB | 7.5 GB | 8.4 GB | 786-1053 ms |
| `run-touch-…T051803Z` | 2.7 GB | 8.1 GB | 7.8 GB | 1053 / 690 / 494 ms |
| `run-perf2-…T120544Z` | **10.9 GB** | 15.2 GB | **2.1 GB** | **70 / 38 / 56 ms** |

The morning boots ran while ~8 GB of anonymous pages (several microVMs, including
runs I had killed with SIGTERM and which reported `rc=143`) plus the day's nix builds
occupied the host. Every arm that "measured the guest page-cache fill" was in fact
measuring host allocation and reclaim work on behalf of a guest that was itself
pushing the host toward its limit.

## 2. What the arms look like on a clean host

`run-perf2-20261115T120544Z`, 6 GiB guest, 128 MiB per reader, all three `nix-lower`
images mincore-dropped before each cold arm (proof recorded per file):

| arm | readers | MB | wall ms | agg MB/s | guest device read_ms | ms device/MB | p95 |
|---|---|---|---|---|---|---|---|
| `cold-cpu` | 1 | 128 | 70 (perf overhead) | 1840 | 55 | 0.4 | 1.6 ms |
| `warm-cpu` | 1 | 128 | 20 | 6514 | 0 | 0 | 0.2 ms |
| `cold` | 1 | 128 | 38 | 3368 | 46 | 0.4 | 1.4 ms |
| `cold4` | 4 | 512 | 148 | 3450 | 671 | 1.3 | 7.1 ms |
| `pre-cpu` | 1 | 128 | 56 | 2279 | 59 | 0.5 | 1.9 ms |

Guest device time is now the *same order as the wall* (46-70 ms of 38-70 ms, with
request overlap), the pre-touch made no difference, and four readers scale instead of
collapsing. The host disk was measured independently at 1.1 GB/s buffered single
stream, and the guest reaches 3.45 GB/s aggregate across four streams - so on a clean
host the lower behaves like the fast device it is.

## 3. What the profile says anyway (symbols on)

`kptr_restrict=0`, `perf_event_paranoid=1` (root, guest kernel) - `perf record -F 1999`
per arm, `perf report --sort dso,sym`:

| arm | top symbols |
|---|---|
| `cold-cpu` | `_copy_to_iter` 20.8 %, `memcmp` 6.6 % (btrfs checksum verify), `btrfs_do_readpage` 2.6 %, `try_to_migrate_one` 2.4 %, `__page_cache_release` 2.1 % |
| `cold4` | `_copy_to_iter` 45.1 %, spinlocks ~8.5 %, `btrfs_do_readpage` 3.0 %, `btrfs_free_extent_map` 2.9 %, `__rmqueue_pcplist` 1.3 % |
| `pre-cpu` | `_copy_to_iter` 29.4 %, `vm_notify` 5.4 %, `btrfs_do_readpage` 2.4 % |
| `warm-cpu` | `_copy_to_iter` 13.6 %, perf's own `amd_pmu_v2_*` ~16 %, `__clock_nanosleep` 2.5 %, `fuse_permission`/`make_vfsuid` (the `/workspace` manifest and output file) |

So even at device speed the CPU is dominated by copying page-cache pages into the
caller's buffer, with btrfs checksum verification and page allocation behind it. That
matters twice: it is the one item DAX would remove, **and** it is an artefact of the
instrument - `nix build`/`exec` mmap files and fault pages, they do not `pread` them
into a bounce buffer, so this profile is not the real workload's profile.

## 4. Rules this session earns

- **Every arm records host `MemFree`/`AnonPages`, and an arm taken under host memory
  pressure is invalid**, not merely noisy. Three probe designs and two conclusions
  died to this in one day; the sampler had the data in `samples.tsv` all along and I
  only read it when the numbers refused to reconcile.
- **Kill leftover microVMs before measuring**, and check `pgrep -f 'cang --mem'` is
  empty; `rc=143` runs still hold ~6 GiB each for as long as the worker lingers.
- The synthetic instrument (`pread` into a user buffer) is a transport probe; the
  workload question needs a workload instrument (mmap/exec/find/build), or the
  numbers will be right about something nobody runs.

## Evidence

`run-perf2-20261115T120544Z` (`logs/samples.tsv` for host conditions, `proof.log` for
per-image drop proof, `workspace/evidence/perf-*.txt` for profiles),
`run-alloc-20261115T051416Z` and `run-touch-20261115T051803Z` for the pressured boots.
Instruments: `readbench.c` (`--direct`, `--no-drop`, `--dev`), `cacheproof.py`,
`control10.sh` (drops every image and proves each), `probe-perf2.sh`, `run-perf2.sh`.
