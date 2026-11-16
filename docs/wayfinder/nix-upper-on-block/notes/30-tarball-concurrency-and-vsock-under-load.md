---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The sandsea tarball is 1.7 MB: 2-4 concurrent extractions cost the same as one; and `cang exec`
# over vsock doubles under full guest CPU load

Two questions in one boot (`run-vsock-20261116T155633Z`), guest 6 GiB / 26 vCPUs,
`--nix=lower:blk --nix=upper:blk`, gates passed.

## 1. Concurrency on `sandsea-src-20261105-1537.tar.gz`

The file is **1 770 735 bytes - 1.69 MiB**, not a large payload, and it lives in the workspace
(virtiofs), sha256 `64d7d6d6…` matching the host copy. So this measures latency and CPU, not
throughput.

| arm | jobs | wall | CPU ticks (user/sys) |
|---|---|---|---|
| `x1` | 1 extract | 83 ms | 7 / 23 |
| `x2` | 2 extracts | 81 ms | 13 / 21 |
| `x4` | 4 extracts | 89 ms | 15 / 34 |
| `x1b` | 1 extract (repeat) | 63 ms | 4 / 6 |

Each extract materialises **404 entries**. Doubling and quadrupling the job count costs
**nothing in wall time** (81-89 ms against 63-83 ms) while user CPU scales with the jobs: the
work is decompress-and-write CPU spread over 26 vCPUs, and the 1.7 MB read is half a millisecond
of virtiofs traffic.

Raw concurrent reads of the same file (`readbench`, whole file per reader): 3.1 ms at 1 reader,
2.7 at 2, 2.0 at 4 -> 550 / 627 / **845 MB/s**, first-read p50 1 970 / 1 690 / 1 324 us. Small
files are latency-bound and get *cheaper* with concurrency here (more parallel requests in
flight, no queue to fill).

**So for this tarball there is nothing to tune**: it is 1.7 MB, and reading it 4x concurrently
is 2 ms. Any build cost around it is extraction CPU and the compile, not I/O.

## 2. Does vsock degrade under concurrent compilation? Yes, ~2x

Host-side round trip of `cang exec <task> /bin/true` (the exec protocol over virtio-vsock, so it
includes the guest-side spawn) measured 40 times in each window, in one guest:

| window | p50 | mean | p95 | max |
|---|---|---|---|---|
| idle | **23.9 ms** | 23.9 ms | 28.4 ms | 28.9 ms |
| 26 CPU burners on 26 vCPUs | **52.6 ms** | 54.4 ms | 79.1 ms | 99.4 ms |

**p50 2.2x, p95 2.8x, max 3.4x.** The load arm used spin loops rather than rustc (no toolchain
in this boot's command path), so this is "fully saturated guest CPUs", which is the same
scheduling pressure `-j26` compilation creates: the guest agent's own thread has to wait to be
scheduled, and that wait dominates a 24 ms round trip.

Consequences worth keeping in mind:

- Interactive latency (`cang exec`, attach, the exec protocol any tool drives) is **not**
  independent of build load: expect ~50 ms instead of ~24 ms while a full-width build runs.
- A 24 ms idle round trip is itself large for an IPC hop - it is dominated by process spawn on
  both ends plus the protocol handshake, not by the vsock transfer, so nothing here says vsock
  bandwidth is the constraint.
- The measurement is one boot, one guest, 26 vCPUs: an over-subscribed real `cargo build -j26`
  may be worse, never better.

## 3. On "disable the pool but keep 2 readers"

From note 29's medians, serial (no IO pool) beat the pool at **every** measured depth: 2 087 vs
1 824 at 1 reader, 2 681 vs 2 261 at 2, 2 541 vs 2 005 at 4, 1 975 vs 1 882 at 7 - with the
depth-7 arms spread 1 128-2 354, i.e. a wash inside noise at high depth. So "serial with a couple
of concurrent readers" is the configuration the data supports, and host O_DIRECT should **not** be
added (it loses at depth 7 and btrfs only partly honours it on compressed extents, note 29).

Two caveats before anyone flips the default: the pool's cost is a per-request bounce-buffer copy,
which a fork change can remove *without* serialising reads (that is the untried option), and every
arm above is a guest with 26 idle vCPUs - under real compile load the pool may behave differently,
which is exactly the measurement this note does not have.

## Evidence

`run-vsock-20261116T155633Z`: `workspace/evidence/{setup.txt,phases.txt,arms.jsonl}` (per-arm CPU
ticks, extract file counts, per-reader latency), `logs/exec-rtt.txt` (80 host round trips, window
labelled), `logs/env.txt` (task handle, gate values, binary md5, tarball sha). Probe
`probe-vsock.sh`, runner `run-vsock.sh`.
