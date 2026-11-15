---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The depth ceiling is the guest's buffered read path, not the block device

**Verdict.** With a real instrument (`readbench`, static, per-reader offsets, per-reader
latency, guest device counters) and per-reader workloads held constant, the guest's
buffered read path saturates around **200 MB/s** and its per-reader share collapses
with depth, while the same bytes through `O_DIRECT` - which skips the guest page
cache entirely - run at 1300-3000 MB/s. The transport is not the limit; the guest's
page-cache fill is. This holds on `tmpfs` too, where there is no block device at all.

One boot, `run-rb-20261115T044546Z`, 128 MiB per reader, host page-drop before
every arm (`cacheproof.py`, mincore-verified), 7 slices from two lower files, guest
and host counters per arm:

| arm | active readers | MB | wall ms | aggregate MB/s | per reader | guest CPU ms | guest device read_ms | ms of device time per MB | p95 latency |
|---|---|---|---|---|---|---|---|---|---|
| `d1` | 1 | 128 | 890 | 144 | 144 | 34 | 377 | 2.9 | 71 ms |
| `d2` | 2 | 256 | 2605 | 98 | 49 | 271 | 4073 | 15.9 | 118 ms |
| `d4` | 4 | 512 | 5140 | 100 | 25 | 2482 | 13519 | 26.4 | 266 ms |
| `d4b` | 4 | 512 | 5173 | 99 | 25 | 2637 | 12616 | 24.6 | 143 ms |
| `d8` | 7 | 896 | 14399 | 62 | 8.9 | 8509 | 58529 | 65.3 | 999 ms |
| `d1d` | 1 | 128 | **300** | **426** | 426 | 28 | 402 | 3.1 | 7.2 ms |
| `d8d` | 7 | 896 | **296** | **3029** | 433 | 150 | 1819 | 2.0 | 3.2 ms |
| `tmp1` | 1 | 128 | 612 | 209 | 209 | 610 | - | - | 12 ms |
| `tmp4` | 4 | 512 | 2478 | **207** | 52 | 5996 | - | - | 44 ms |

Three readings, each in a different place:

1. **`tmpfs` does not scale either.** 209 MB/s at one reader, 207 MB/s aggregate at
   four (52 MB/s each). So a shared, guest-side cost caps buffered reads at ~200 MB/s
   and gives each reader a quarter of the single-reader share, with no device involved.
2. **The block lower is slower still and degrades more** - 144 MB/s at one reader,
   62 at seven - and its per-MB device-time inflates 2.9 -> 65 ms/MB with depth. The
   device counters are the *guest's*, so this is the guest kernel's own block path
   under queue pressure, not host storage.
3. **`O_DIRECT` removes the ceiling**: 426 MB/s at one reader and 3029 MB/s aggregate
   at seven, with 2-3 ms of device time per MB and p95 latency of 3-7 ms. The path
   from guest buffer to host is fast; what costs 60 ms/MB is filling guest pages.

That closes the question this session chased. It also explains the earlier result
([note 09](09-direct-to-guest-reads.md)) that removing libkrun's staging copy helped
one reader (1.5x) and did nothing at depth eight: at depth one the copy *is* part of
the guest-visible cost, and at depth eight the guest's page fill dominates both
paths. It is not the host block device, and a vhost-user backend cannot change a
guest page-cache cost.

## What this does not establish

- **Where 1300-3000 MB/s came from.** `mincore` reported **0** resident pages of all
  three 12 GiB `nix-lower` images before and after every arm, and the state root's
  device (`sda`) moved 0-50 MB during arms that read up to 896 MB. Either the host
  serves the image from something `mincore` cannot see (a shared btrfs extent read at
  3 GB/s across 7 streams is one candidate) or the counters are being read wrong.
  Until that is resolved, no arm here can be called host-cold with certainty; the
  guest-side numbers (device service time, latency, CPU) stand on their own.
- **Buffered vs workload.** `O_DIRECT` measures transport, not the cold-read workload,
  which needs the pages in the cache. The 3 GB/s figure is an upper bound on what the
  transport could deliver, not a claim about `find`/`exec`-style reads.
- One boot, seven readers not eight (only 7 slices existed), 7% of a 12 GiB image.

## Instrument

`readbench.c` (built static with `pkgsMusl.gcc` in `SCRATCH`): N pthreads, each
preading explicit `(path, offset, length)` slices from a manifest, recording per-read
latency (p50/p95/max), total ms, `getrusage` CPU, and `/sys/block/<dev>/stat` deltas;
its own cache drop and a `want`/`go` handshake with the host (`--sync-out`/`--sync-in`,
because the guest cannot write into a host-owned directory). Host side: `cacheproof.py`
PROT_READ-maps the image and asks `mincore` (never faults), optionally after
`posix_fadvise(POSIX_FADV_DONTNEED)`, then releases the guest and records device
sector deltas. Runner `run-rb.sh`, control `control8.sh`, probe `probe-rb.sh`.

Four failed handshakes preceded this one and each is now fixed in the harness: a
global `sync()` before the handshake, a duplicate shell-level `want`/`go` exchange
that consumed the host's token before `readbench` could see it, a host `touch` into a
directory the guest had created (owner 165536, host `dev` cannot write), and a
`pkill -f control8.sh` that matched and killed the harness's own shell.
