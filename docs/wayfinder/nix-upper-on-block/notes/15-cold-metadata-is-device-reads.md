---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# (a) The cold metadata penalty is 941 small serialized device reads, not CPU

Same `find <lower>/store -maxdepth 3 -type f` every arm (3897 files), block lower,
clean host, all arms in one boot, guest device counters per arm.

| arm | drop | perf | files | wall ms | device reads in arm |
|---|---|---|---|---|---|
| `mc2` | guest + host | no | 3897 | **370** | 941 |
| `ms1` (`-printf '%s\n'`, forces a stat each) | guest + host | no | 3897 | **341** | 986 |
| `mc1` | guest + host | 999 Hz | 3897 | 2308 | 941 |
| `mc3` | guest + host | 999 Hz | 3897 | 1225 | 941 |
| `mw1` / `mw2` | none | 999 Hz | 3897 | 1164 / 1211 | 0 |

## 1. What the numbers say

- **A cold sweep costs ~316-370 ms and moves ~482 KB from the device.** That is 941
  read I/Os, i.e. **~336-390 µs per device read** - latency-bound, one small read at a
  time, not bandwidth-bound. Warm is ~54 ms (previous boot) with **0** device reads.
- Forcing a `stat` of every file (`ms1`) costs no more than the plain listing
  (341 vs 370 ms): inode reads are already part of the same metadata blocks.
- **`perf` at 999 Hz costs 900-1900 ms on this arm** (mc1 2308, mc3 1225 vs 341-370
  without). Any metadata profile taken at that rate measures the profiler. The two
  999 Hz cold arms disagree by 1.9x for this reason alone, and warm+perf (1164-1211 ms)
  is slower than cold without perf - a reminder that an instrument's overhead must be
  measured as one of the arms, which `mc2`/`ms1` do here.
- The on-CPU profile contains no dominant kernel cost: `vm_notify` 10 %, the allocator
  4-5 %, `find`'s own `rpl_fts_read`/`consider_visiting`/`rpl_fcntl` in the warm arm,
  and only `find_extent_buffer`/`btrfs_search_slot`/`read_block_for_search` at 2 % each.
  The 316 ms is therefore **blocked time on device reads**, invisible to a sampling
  profiler - which is why the counter comparison above, not the profile, is the result.

## 2. Where a lever could act

The cost is the *number* of serialized small reads and their latency, so the candidates
are all about batching or removing them, and each is cheap to test with this harness:

1. **Read-ahead on the block device**: `/sys/block/vdc/queue/read_ahead_kb` is 128 KB
   by default; store directories are read roughly sequentially, so raising it (e.g.
   4096) is a one-line arm and needs no build.
2. **Request size**: `max_sectors_kb` interacts with the same batching; earlier work in
   this effort swept it for data reads (no 512 KiB advantage at depth), but metadata
   was never in that matrix.
3. **Fewer metadata blocks**: an alternative lower image layout (erofs/squashfs with a
   single metadata pass, or a pre-warmed directory index) removes reads instead of
   batching them - a bigger change, and the only one that could remove the penalty
   rather than amortise it.
4. **Not a lever**: guest CPU. The cold arm spends 8-9 ms of CPU in a 341-370 ms arm, so
   removing kernel/user work cannot recover this cost.

## 3. (b) DAX, answered from data instead of another boot

`/nix/store/4kjxfzpg…` contains no DAX surface at all (`dax=`, `dax_never`, `DAX`,
`CANG_NIX_DAX`, `cache=none` all absent from the binary; `--help` has no dax key), so
there is no DAX arm to run on this build.

And the arithmetic argues against building one for `/nix`:
- In block mode the guest CPU is 8-9 ms of a 46-51 ms 128 MiB read and the device
  service time (64-70 ms, overlapped) is the floor. DAX removes guest page fill and the
  copy, i.e. at most the 8-9 ms - **≤18 %**, and DAX does not exist for block devices
  anyway.
- DAX is a virtiofs feature, and virtiofs is currently **2.1-3.0x slower** than block
  on the same cold bytes (note 14: 889-1258 MB/s against 2491-2777). For DAX to matter
  it would have to make that path beat 46 ms per 128 MiB, from a starting point of
  102-144 ms whose CPU is only 40-43 ms.

So DAX is not the lever for this cost; the metadata read count is.

## Evidence

`run-meta-blk-20261115T130052Z` (`workspace/evidence/meta.txt` per arm with device
counters before/after, `perf-*.txt`, `progress.txt`). Probe `probe-meta.sh`, runner
`run-meta.sh`, control `control10.sh` (host image drop + mincore proof per arm).
