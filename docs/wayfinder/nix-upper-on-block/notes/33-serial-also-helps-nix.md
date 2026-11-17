---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Turning the block pool off also helps `/nix`: bulk reads 1.7-2.8x faster, metadata no worse

The question was "will serial improve `/nix` access, so should I switch it off generally?" Measured
directly, same binary (`/nix/store/m0ymp1sh…` built with the diagnostic toggle), two boots, one mode
each, `--nix=lower:blk --nix=upper:blk`, 8 GiB guest, and the host image mincore-dropped and proved
before every arm:

| mode | cold `find /nix/store` (941-read walk) | cold 128 MiB single-reader read |
|---|---|---|
| pool (default) | 522, 523, 632, 1 156 ms | 128.6 ms (995 MB/s), 217.2 ms (589 MB/s) |
| **serial (no pool)** | **419, 523, 458 ms** | **67.4 ms (1 898 MB/s), 76.5 ms (1 672 MB/s)** |

So: **yes, and not marginally.** The bulk read is 1.9-2.8x faster with the pool off, and the
metadata walk is at least as good (serial's median 458-523 ms against the pool's 523-632, with the
pool producing one 1 156 ms outlier). Both arms read the same 3 897 files and the same 128 MiB.

This agrees with note 29's depth series (serial ahead at 1/2/4 readers, a wash at 7) and does not
conflict with note 31 (a compile is indifferent) - a compile is CPU-bound with a small sequential
read phase, while both arms here are read-bound.

## Recommendation

For read-heavy or metadata-heavy work, `parallel_reads = false` is the better default on this
hardware: it removes a bounce-buffer copy per request, and nothing here shows a depth at which the
pool pays it back. I would still not flip it on these measurements alone, for two reasons:

- every arm ran on a guest with idle vCPUs, and note 31's compile arms - the one loaded workload
  measured - were indistinguishable, so the loaded read picture is untested;
- the cleanest fix is a fork change rather than a global switch: keep the pool's parallelism but
  read straight into guest memory (the direct-to-guest path, note 09), which would remove the copy
  without losing overlap. Serial is the cheap proxy for that, not the ideal endpoint.

## Also: PTY and waypipe are on the same transport

They are implemented over virtio-vsock, so the degradation measured in note 32 applies to them in
principle, and the shape of that measurement supports it: under memory churn the exec round trip
stalled *before the guest command ever produced output* (empty stdout, empty stderr, 11 s p50,
two calls past 120 s), which is a property of the path, not of `fork`. The remaining unknown is how
much of the stall is process creation versus transport; a fork-free frame ping on the attach
protocol would separate them.

## Evidence

`run-nixread-pool-20261117T023525Z`, `run-nixread-serial-20261117T023923Z` (per-arm
`ws/evidence/results.txt`, `logs/proof.log` with the per-arm mincore drop proof, `logs/env.txt`).
Probe `probe-nixread.sh`, runner `run-nixread.sh`, control `control11.sh`, toggle patch
`blkread/blk-serial-direct-toggles.patch`. Both boots passed a gate that refuses to start while any
other VM worker is alive (the note-32 lesson).
