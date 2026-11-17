---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The exec stall is in the spawn/handshake, not in the command - and only memory-allocating
# loads produce it

Split the exec round trip into its parts by having the guest command print its own clock:

```
host t0 --[send]--> guest S --[run]--> guest E --[reply]--> host t1
total = t1 - t0          run = E - S          other = total - run
```

`other` is clock-offset-free (the guest's clock runs ~350 ms ahead of the host's, measured, so
absolute S-t0 is useless but the difference is not).

| window | n | total p50 | total p95 | total max | command's own run p50 |
|---|---|---|---|---|---|
| idle | 52 | **58 ms** | 67 ms | 74 ms | 8 ms |
| 26 CPU burners (spin) | 183 | 62 ms | 100 ms | 147 ms | - |
| 26 fork+exec loops of `/bin/true` | 907 | 82 ms | 109 ms | 156 ms | - |
| 13 `dd oflag=direct` + 13 tmpfs churners | 9 | **1 979 ms** | 14.2 s | 14.2 s | 278 ms |
| **26 concurrent `rustc -O`** | 32 | **4 015 ms** | **54.3 s** | **97.7 s** | 88 ms |

Three readings:

1. **CPU saturation and a fork/exec storm are cheap**: 62 and 82 ms p50, both under 160 ms max.
   So "vsock goes to seconds because the CPUs are busy" is wrong, and so is "because of process
   churn".
2. **Loads that allocate memory and dirty page cache are what break it**: 2-4 s p50, tens of
   seconds at p95. The tmpfs churner arm (`dd` 200 MB into /dev/shm repeatedly) and 26 rustc
   processes share exactly that property and nothing else with the compile.
3. **The executed command is not slow**: it runs in 8-278 ms inside the guest while the round trip
   takes seconds, so the time is before the command starts or after it ends - process creation,
   the protocol handshake, or the transport copy.

## What this does and does not say about PTY and waypipe

They ride the same virtio-vsock, so a transport-level stall would hit them too - but they do **not
fork a process per message**, which is the half of `other` this experiment cannot separate out.
The measurement therefore does not establish that attach PTY or waypipe degrade like `cang exec`;
it establishes that `cang exec` does, under memory-allocating load.

Attempting to measure the PTY directly failed: driving `cang attach` from a scripted pty produced
2 091 samples of 100-160 us marked `timeout` - the client exits immediately (the pty read raises
EIO), so the attach path refuses a non-interactive client and needs a real terminal. That is a
harness limitation, not a result, and it is why this note reports no PTY number.

## Separating the remaining two halves

`other` = (spawn: send + fork/exec + handshake) + (return: reply + host wakeup). To split it, the
guest agent should log its own accept/parse/fork/exec timestamps (it is `cang-guest-init`, pids
visible in `setup.txt`), or the host should timestamp a one-way ping that needs no process at all
(the attach protocol's frame, not exec). Both are next steps; neither is done.

## Also: is the IO pool generally slower than serial?

Two measurements answer it differently, by workload shape:

- **concurrent reads** (note 29, `/nix` through the composed overlay, 128 MiB slices): serial beat
  the pool at every depth - 2 087 vs 1 824 MB/s at 1 reader, 2 681 vs 2 261 at 2, 2 541 vs 2 005
  at 4, 1 975 vs 1 882 at 7 (a wash inside a 1 128-2 354 spread).
- **a real compile** (note 31, `-p sandsea-collector --jobs 26`): indistinguishable - the three
  warm arms were 246 / 236 / 266 s across serial, pool+direct and serial+direct, with the
  first-of-round artefact (553 / 681 s) dominating everything else.

So: serial is faster for read-heavy work, not measurably faster for a compile. The pool's cost is
a bounce-buffer copy per request, which is a proportion of a read phase that a compile barely has.

## Evidence

`run-vsock7-20261117T012455Z` (`logs/exec-split.txt`: per-sample host t0/t1 and guest S/E,
`workspace/evidence/setup.txt` with the rustc version and source size). Earlier windows:
`run-vsock4-*` (spin/spawn), `run-vsock5-*` (spawn/io, and the failed PTY attempt),
`run-vsock6-*` (block+memory mix). Probes `probe-vsock{4,5,6,7}.sh`, runners `run-vsock{4,5,6,7}.sh`,
`pty_rtt.py`.
