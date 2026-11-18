---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Copy-free pool reads exist now, and under load they beat serial

Note 34 retired the staging-buffer recycling idea and concluded that "the copy into guest memory
is the pool's cost, and only removing the copy changes the number". This is that change, measured.

## What changed (fork `2fad450a`)

`virtio-blk`'s pool worker no longer stages. It preads straight into the guest's descriptor
buffers with the same `Writer::write_from_at` the serial path uses, and the completion path writes
only the status byte and reports `data_len`; the bounce buffer, its 64 MiB permit budget and
`allocate_read_buffer` are gone.

The hard part is the reason the old design staged at all: a pool thread must not write guest memory
that another in-flight request writes. The dispatcher now keeps the guest ranges of every in-flight
read (`inflight_read_ranges`) and admits a read only when its ranges are disjoint, draining
completions until they are (`ranges_overlap` + `wait_until_read_ranges_are_free`). A guest that
posts genuinely overlapping buffers is therefore served in submission order rather than torn, and
falls back to the device thread if the pool ever stops making progress.

Two bugs of my own the new tests caught, both worth naming because they would have been silent in
production:

- `Writer::split_at` returns the *remainder*, so my first version wrote the data into the
  one-byte status writer and left the real data region untouched - and reported success, because
  the write "consumed" one byte. Only `pooled_reads_write_straight_into_guest_memory` (asserting
  the guest bytes equal the image) exposed it.
- The dispatcher's two inline "pool is full" fast paths consumed a completion without retiring its
  ranges, so after the first completion every later read looked like an overlap and silently fell
  back to serial. `overlapping_pooled_reads_are_admitted_one_at_a_time` caught it (a stale entry,
  not a correctness failure - the fallback was safe but would have made the pool useless).

## Measured: warm host, no load (uninformative)

`find /nix/store` 3 897 files, cold `128 MiB` read, host image dropped per arm:

| mode | find (ms) | read |
|---|---|---|
| serial | 509 / 473 / 450 / 448 | 926 ms, 182 ms (162 / 863 MB/s) |
| pool direct-to-guest | 966 / 573 / 390 / 478 | 697 ms, 250 ms (195 / 620 MB/s) |

Inside this host's noise for both shapes, as note 29's depth series predicted for a fast backing.

## Measured: 26 spinners + 8 direct writers (decisive)

| mode | cold `find` under load (ms) | cold 128 MiB read under load |
|---|---|---|
| serial | **66 979**, 11 956, 6 641 | 2 810 ms (46 MB/s), 3 608 ms (36 MB/s) |
| **pool direct-to-guest** | **5 869, 5 739, 5 603** | **692 ms (185 MB/s), 721 ms (178 MB/s)** |

So with the copy gone the pool is the better choice under CPU saturation: the walk is 4-5x faster
and stable (5 603-5 869 ms against serial's 6 641-66 979), the read 4-5x faster, and it keeps the
parallelism that a loaded guest actually needs. The serial path's 66 979 ms first arm is the same
starvation note 35 described from the other side: one thread doing both pread and completion has
nothing to overlap with when its vCPU share is contended.

## What this means

- The shipped default is still serial reads (`f63b80b`). On an idle guest with a warm backing the
  two are indistinguishable; under load the pool is now clearly better.
- Re-flipping to pooled reads is therefore defensible *with this change*, but it is a default for
  every workload, and the untested case remains a slow backing (note 35's open item) and the
  depth-8 arms where the earlier staging pool was a wash.
- Both configurations now avoid the copy that note 34 identified; the difference is only whether
  preads run in parallel.

## Evidence

Fork commit `2fad450a` (175 `krun-devices` tests, clippy clean); cang builds
`/nix/store/bb63miak…` (this change + the diagnostic toggles `CANG_BLOCK_SERIAL_READS`,
`CANG_BLOCK_DIRECT_IO`). Runs: `run-nixread-{ser,pooldirect}-20261118T*` (idle),
`run-nixload3-{ser,pooldirect}-20261118T*` (load), each with `ws/evidence/results.txt` and
`logs/proof.log`. Control `control11.sh`, probes `probe-nixread.sh` / `probe-nixload3.sh`.
