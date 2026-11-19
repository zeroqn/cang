---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# `cang exec` under load: 397 ms -> 132 ms p50, 13 s -> 2.4 s p95

Same class of fix as the terminal (note 36), applied to the exec path: the two threads that carry
an exec request now run at nice -20, so a guest whose vCPUs are saturated by a build still accepts
and answers promptly. The command itself keeps nice 0.

## What changed (`crates/cang-guest-init`)

- New `runtime/priority.rs`: `INTERACTIVE_NICE = -20` and `prioritise_current_thread(name, nice)`,
  best effort, logging through `CANG_GUEST_DEBUG`. The session's local copy of that helper is gone -
  both paths share this one.
- `exec.rs::start` raises the **listener** thread (it only accepts and hands off, never forks) and
  each **client** thread (handshake, pipe pumping, exit frame) to that level.
- `exec.rs`'s `pre_exec` now resets the child to nice 0 explicitly. Without it the child would
  inherit the client thread's -20, which would let `cang exec cargo build` outrank the guest's own
  work - a semantic change nobody asked for.

## Measured (rustc storm = 26 concurrent `rustc -O`, guest-side run/guest timestamps)

| window | before (note 32) | after |
|---|---|---|
| idle | p50 62 ms, p95 83 ms, max 92 ms | p50 44 ms, p90 64 ms, max 1.6 s (one outlier early in the boot) |
| 26 rustc | p50 **397 ms**, p95 **13.2 s**, max **66 s** | p50 **132 ms**, p90 220 ms, p95 **2.4 s**, max **4.4 s** |
| the command's own run | 35 ms | 17 ms |

So p50 3x better, p95 5.5x, worst case 15x, with the executed command unchanged - the delta is
entirely plumbing (accept, fork, frames), which is what the priority targets.

## Activation verified, not assumed

A dedicated boot read every `cang-guest-init` thread's nice from `/proc/<tid>/stat` at rest and again
under a 13-`rustc` storm:

```
== cang-guest-init threads at rest        == under a 13-rustc storm
750 750 nice=-20                          750 750 nice=-20
750 786 nice=-20                          750 786 nice=-20
750 788 nice=-20                          750 788 nice=-20
784 784 nice=0                            784 784 nice=0
785 785 nice=0                            785 785 nice=0
```

The exec server's threads (pid 750) hold -20 throughout; the unrelated guest-init helpers (784, 785)
stay at 0, and the child resets itself, so the blast radius is the plumbing only.

## Caveats

- One boot per condition; the before/after boots are hours apart and hosted on a machine that has
  shown 20-30 % spread. The p95/max improvements are far outside that; the p50 improvement (3x) is
  large but should be treated as "2-3x" rather than a point value.
- The idle max of 1.6 s in the after-boot is a single early sample (likely the first exec while the
  guest was still finishing boot); the p90 of 64 ms is the representative number.
- This does not make a *command* start faster under load - only the handshake around it. A guest
  that is memory-starved still wedges (note 32's retracted churn arms).

## Disk pressure (reported, not hidden)

The arm could not run at first: `/home/dev/disp` hit **100 %** and cang failed with
`No space left on device` writing its launch config. I freed what was mine - my erofs images
(`blkread/erofs2`, 12 GB, hashes recorded in note 18), `rbtest` payloads, and the 24 GB of 2026-11-09
run payloads (`run-20261109T164024Z/block` + `virtiofs`), keeping its logs in `blkread/logs-20261109/`
and hoisting its `storage.conf` to `blkread/storage.conf` with every runner repointed. That took it
from 100 % to **98 % (4.2 GB free)**, which is still tight: `gpu-smoke` (64 GB),
`release-v0.22.0-smoke` (94 GB), `smoke-fw735` (127 GB), `sigbus` (161 GB), `storage` (220 GB) and
`cang` (359 GB) are other sessions' or the state root's, and I have not touched them.

## Evidence

`run-vsock7-20261119T105844Z` (after: `logs/exec-split.txt`, 52 idle + 79 rustc samples),
`run-execnice-s0d0-20261119T113302Z` (`ws/evidence/nice.txt`), build
`/nix/store/n743p6hi…-cang-0.22.0`; 392 guest-init tests and clippy clean.
