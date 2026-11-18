---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Shipped: pooled, copy-free block reads are the default

`crates/cang-libkrun/src/linked.rs::block_device_new` sets `set_parallel_reads(true)` again, this
time with the fork's direct-to-guest pool (`2fad450a`) behind it: no staging buffer, no copy, and
admission by guest range so no two in-flight requests write the same bytes.

## The shipped binary, verified

Built from `main` with the pin at `2fad450a` (`/nix/store/bcq7ffm0…`); the diagnostic toggles are
*not* in this build, so `CANG_BLOCK_SERIAL_READS` is ignored and every arm ran the shipped default -
worth stating because the runner still passes that variable, which makes the labels look like a
serial arm when they are not.

| shape | shipped default (pool, direct-to-guest) |
|---|---|
| idle, cold `find /nix/store` (3 897 files) | **389 / 409 / 409 / 486 ms** |
| idle, cold 128 MiB read | **197.9 ms (647 MB/s), 80.5 ms (1 590 MB/s)** |
| 26 spinners + 8 direct writers, cold `find` | **4 939 / 5 016 / 5 113 ms** |
| under the same load, cold 128 MiB read | 3 147 ms (41 MB/s), 2 643 ms (48 MB/s) |

Against the arms in note 37: idle is equal or better than both candidates (serial 448-509 ms on the
same walk, pool-direct 390-573), and under load the walk holds at ~5.0 s against serial's
6.6-67 s. The load-read arms are the noisy ones - 692/721 ms measured in the note-37 boot for the
same code against 2 643/3 147 here, a 4x spread with contention deciding which reader gets
scheduled - so I quote them as "seconds, not tens of seconds" rather than as a point estimate.

## Why this is the default and what would change it

- Idle + warm backing: indistinguishable from serial, so the default costs nothing in the common
  case.
- Loaded guest: 1.3-13x better for the walk, and the pool is the configuration that keeps
  parallelism when vCPUs are contended.
- The open risk stays the one note 35 named: a backing slower than this host's warm page cache,
  where many parallel preads could queue behind each other. Nothing in this map measures it, and it
  is the arm that would justify revisiting either direction.

## Evidence

`run-nixload3-shipped-20261118T110118Z`, `run-nixread-shipped-20261118T*`
(`ws/evidence/results.txt`, `logs/proof.log`); binary `/nix/store/bcq7ffm0…`; cang commit for the
flip and the fork pin `2fb76b4`. 720 `cang` lib tests and 175 `krun-devices` tests pass.
