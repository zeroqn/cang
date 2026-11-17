---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Shipped: reads are serialised by default, and it holds up under load

`crates/cang-libkrun/src/linked.rs::block_device_new` now sets
`set_parallel_reads(false)`; `parallel_writes` is untouched, so writes keep the pool and their
copy-free path. What a read does with that flag off is pread straight into the guest's descriptor
(`Writer::write_from_at`), with no staging buffer, no per-request allocation and no copy.

## Acceptance under a load the guest survives

26 CPU spinners on 26 vCPUs plus 8 `dd oflag=direct` writers on the build disk, then the same arms
as notes 33/34 (cold `find /nix/store` over 3 897 files; a cold 128 MiB single-reader read), image
mincore-dropped per arm, both boots gated on no other VM worker:

| build | cold `find` (ms) | cold 128 MiB read |
|---|---|---|
| **serial (new default)** | **6 778 / 6 794 / 8 827** | **3 579 ms** wall, 755 ms in-reader (170 MB/s); **1 874 ms**, 446 ms (287 MB/s) |
| pool (old default) | 8 747 / 10 847 / 16 534 | 8 099 ms, 3 890 ms (33 MB/s); **19 560 ms**, 9 905 ms (13 MB/s) |

Serial is 1.3-2.5x faster for the walk and 5-20x for the bulk read **under load**, which is the
direction the idle measurements predicted and larger than expected. Mechanism, and it is not the
copy alone: the pool runs 8 workers plus a dispatcher, so under 26 spinners it has 34 runnable
threads on 26 vCPUs, and every read pays a queue handoff on top of the copy. The serial path has
one thread that both issues the pread and completes the request.

Both boots' idle-relative degradation is worth recording: the walk goes from ~450-500 ms idle to
6.8-8.8 s with the vCPUs saturated, i.e. **most of a loaded walk is guest CPU scheduling, not
storage** - the same lesson as note 32, where a build's own CPU load dominated the exec path.

## The one case still unmeasured

A backing device slower than this host's page cache. Every arm here answers in ~150 us per request
with the image warm on the host; on a cold or network-backed image, serialising preads could cost
more than the copy saves. Nothing in this map measures that, and the depth-7 arms of note 29 (a wash
inside a 1 128-2 354 MB/s spread) are the closest signal, so this stays a candidate for a follow-up
rather than a settled property.

## What would let reads be pooled again

Pool threads writing guest memory directly, gated on a proven non-overlap check per request - the
single-completion-thread rule and its regression tests
(`pool_wakes_dispatcher_and_drains_overlapping_reads_on_drop`,
`reads_stay_serialized_when_only_writes_are_parallel`) forbid it while guest read buffers can
overlap. Reusing the staging buffers was measured and does not help (note 34), so the copy is the
only thing left to remove.

## Evidence

`run-nixload3-serialnew-20261117T051739Z` and `run-nixload3-pool-*` (`ws/evidence/results.txt`,
`progress.txt` with the load and MemFree at arm time, `logs/proof.log` per-arm drop proof).
Binaries: serial default `/nix/store/qw0sgx0c…`, pool `/nix/store/m0ymp1sh…`. Probe
`probe-nixload3.sh`, runner `run-nixload3.sh`.
