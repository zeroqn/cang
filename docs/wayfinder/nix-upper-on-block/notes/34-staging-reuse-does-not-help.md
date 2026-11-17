---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Staging-buffer reuse does not move the pool's cost - the copy is the cost

Note 33 recommended "keep the pool's parallelism, drop the copy" and pointed at a cheaper
intermediate: the pool allocates a **zeroed buffer per request** (`allocate_read_buffer` ->
`try_reserve_exact` + `resize(len, 0)`) before copying it into guest memory, so recycling those
buffers should remove the allocation and its first-touch page faults without touching the
concurrency contract. Built it (a free list of buffers by capacity, up to 4 per pool worker, buffer
returned by the completion path) and measured it: **no improvement.**

Six boots, same workload as note 33 (`find /nix/store` = 3 897 files; a cold 128 MiB single-reader
read), image mincore-dropped and proved before every arm, gate refusing to start beside any other
VM worker:

| build | cold `find` (ms) | cold 128 MiB read |
|---|---|---|
| pool, per-request allocation | 522, 523, 632, 1 156 | 183 ms / 995 MB/s, 287 ms / 589 MB/s |
| pool, recycled buffers | 561, 654, 994, 589 | 479 ms / 292 MB/s, 283 ms / 560 MB/s |
| **serial (no pool, no copy)** | **419, 523, 458, 449** | **107 ms / 1 898 MB/s, 122 ms / 1 672 MB/s** |
| pool (old), repeat | 707, 1 024, 1 019, 670 | 495 ms / 353 MB/s, 209 ms / 870 MB/s |
| pool (reuse), repeat | 729, 498, 1 034, 615 | 370 ms / 455 MB/s, 442 ms / 357 MB/s |
| pool (old), third | 490, 820, 528, 569 | 345 ms / 455 MB/s, 400 ms / 370 MB/s |

The two pool variants are indistinguishable, and both are slower than serial on the read arm in
five of six boots. So the allocation was never the pool's cost: **the copy into guest memory is**,
and only removing the copy (serial, or a proven-non-overlap direct path) changes the number. That
is consistent with the fork's own design note - writes are already copy-free (`write_from_guest`
reads straight from the guest's buffers) while reads must stage.

Retired: the recycle patch, kept only as `blkread/fork-recycle-staging-buffers.patch`; the fork is
back at `9ff213f1` with a clean tree. It passed `cargo test -p krun-devices` (173 tests, including
the overlapping-read and serialized-read tests) and clippy, so it is a usable starting point if a
direct-to-guest path ever needs a fallback staging buffer.

## What this leaves for the "turn the pool off" decision

Note 33's evidence is now the only measured win: serial is 1.7-2.8x faster for bulk `/nix` reads
and at least as good for the metadata walk, at the price of serialised reads. With the copy
identified as the cost and the allocation ruled out, the choice is:

1. ship `parallel_reads = false` (cheap, measured);
2. or prove non-overlap per request and read straight into guest memory from pool threads (the
   real fix, forbidden today by the single-completion-thread rule and its regression tests);
3. or read into guest memory from a *single* completion thread while the pool preads into recycled
   buffers - which is exactly today's design, now shown not to be where the time goes.

## Evidence

`run-nixread-{pool,poolreuse,serial,poolold2,poolnew2,poolold3}-*` (`ws/evidence/results.txt`,
`logs/proof.log` per-arm drop proof, `logs/env.txt`). Probe `probe-nixread.sh`, runner
`run-nixread.sh`, control `control11.sh`. Fork patch: `fork-recycle-staging-buffers.patch`.
