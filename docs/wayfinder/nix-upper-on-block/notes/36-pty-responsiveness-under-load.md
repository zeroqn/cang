---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# PTY responsiveness: the tail was guest CPU starvation, and nice -20 removes it

Asked to make the terminal stay responsive while the guest compiles, with "over 10 s is a
disaster". Measured it first, then fixed the cause the measurement named.

## 1. What the terminal actually does under load (before the fix)

A driver on a raw pty (echo and canonical off, so a marker coming back proves the *guest*
produced it) types `printf '<marker>'` and times the round trip; samples are labelled by a
host-side phase file. Six boots, one workload each, gates refusing to run beside any other VM:

| load | n | p50 | p90 | p99 | max | > 250 ms | > 1 s |
|---|---|---|---|---|---|---|---|
| idle | 6 783 | 0.9 ms | 1.2 | 2.9 | 6.7 ms | 0 | 0 |
| 8 rustc loops | 42 796 | 1.4 | 3.6 | 9.1 | 403 ms | 1 | 0 |
| 13 rustc loops | 17 508 | 3.1 | - | 24.1 | **1 353 ms** | 12 | 2 |
| output flood only | 81 563 | 0.9 | - | 2.7 | 30 ms | 0 | 0 |
| 13 rustc + flood | 10 718 | 4.3 | 10.0 | 34.7 | **9 084 ms** | 19 | 4 |

Three things this separates, each of which changes what to fix:

- **The flood of output costs nothing** (81 563 samples, max 30 ms): not backpressure, not
  queueing behind a build's printing.
- **Guest memory is not the cause**: in the 13-rustc boot `MemAvailable` stayed between 6 564 and
  7 418 MB, and the guest console has no RCU stall or OOM line.
- **The tail tracks compiler concurrency** (8 rustc -> 403 ms, 13 -> 1.3 s, 13 + flood -> 9 s), and
  a time-to-first-byte breakdown puts it before any output (worst sample: 1 918 ms to the first
  byte with 52 bytes seen), i.e. the session thread was not scheduled, not waiting on I/O.

## 2. The fix: prioritise the two terminal threads

`crates/cang-guest-init/src/guest_init/runtime/session.rs` now raises the nice level of the two
threads that carry a keystroke - `session-pty-pump` (pty -> vsock frame) and `session-pty-input`
(vsock frame -> pty) - to `-20` at the top of `serve_attached_client`. Best effort: a failure is
logged through the module's existing `managed_debug` and the session runs at normal priority.

Verified from inside the guest rather than inferred: the probe records every `cang-guest-init`
thread's `nice` from `/proc/<tid>/stat` during the load window, and the run shows **`-20` for the
two session threads** (`{'‑20': 41, '0': 93}` - the other guest-init threads untouched).

## 3. After the fix, same loads

| load | n | p50 | p90 | p99 | max | > 250 ms | > 1 s |
|---|---|---|---|---|---|---|---|
| idle | 7 592 | 0.8 ms | 1.0 | 1.3 | 4.0 ms | 0 | 0 |
| 13 rustc loops | 37 929 | **1.7** | 3.7 | **7.5** | **59 ms** | **0** | **0** |
| 13 rustc + flood | 28 415 | 2.4 | 4.9 | 11.1 | 170 ms | **0** | **0** |

Against the same workloads before the fix: p99 24.1 -> 7.5 ms, max 1 353 -> 59 ms (and 9 084 ->
170 ms with the flood). **No sample over 250 ms anywhere**, so the "10 s is a disaster" case is
gone by two orders of magnitude.

## 4. What this does not fix

`cang exec` is a different path and stays expensive under load (note 32): 397 ms p50 / 13.2 s p95
under a rustc storm, because it creates a process and walks a handshake per call. The terminal is
now responsive; asking a *new* command to start is still slow while the guest compiles.

## Evidence

`run-pty{3,4,5,7,8,9,10,11}-*` (`logs/pty-rtt.txt` per sample, `ws/evidence/{phase,mem.txt,nice.txt}`),
drivers `pty_rtt{3,4,5}.py`, runners `run-pty{3..11}.sh`. Binary after the fix:
`/nix/store/2zasxagd…`; before: `/nix/store/qw0sgx0c…`. `cargo test -p cang-guest-init` 392 passed.
