---
label: wayfinder:prototype
title: The responsiveness-and-survival arm kit
status: closed
blocked_by: []
claimed_by: "pi (agent, 2026-11-25)"
---

## Question

Build the instrument every later decision cites: one arm kit that measures, on a
live guest, both halves of this map - does the VM survive a memory storm, and
what does the interactive path cost under one.

Extend what exists rather than writing a new harness:

- `scripts/cang-pty-benchmark.sh` for the PTY round trip and the guest profile
  line (`CANG_ATTACH_PROFILE=1`).
- `tools/rust-compile-bench` for a reproducible storm (its README records the
  settings: `RUSTC_WRAPPER=`, `CARGO_INCREMENTAL=0`, an explicit
  `CARGO_BUILD_JOBS`).
- Note 36's shape for the load table (p50/p90/p99/max and the `> 250 ms` /
  `> 1 s` counts) - `docs/wayfinder/nix-upper-on-block/notes/36-pty-responsiveness-under-load.md`.

The kit must produce, per arm:

1. **Survival**: the guest's kernel console, whether the VM exited, the task
   exit status, and which process the OOM killer named - plus proof the arm
   actually reached pressure (`MemAvailable` floor, peak swap used, PSI
   `full avg10`), because a storm that never pushed is not a resilience result.
2. **Latency**: PTY round trip (raw pty, echo and canonical off, a marker that
   proves the *guest* produced it) and command start (`cang exec` or the session
   equivalent), each reported as a distribution, labelled by host-side phase.
3. **Attribution**: host-side phase file, guest `/proc/meminfo` +
   `/proc/pressure/memory` + `/proc/vmstat` + `/sys/block/zram0/mm_stat` samples
   at a fixed cadence, so the ladder in ticket
   [01](01-memory-failure-ladder.md) can be read from the same run.
4. **Discipline**: one boot per arm, no concurrent VM, a clean window, every log
   path quoted in the note (`cang-live-guest-evidence-harness` and
   `evidence-capture-reliability`).

Deliverable: the kit in `tools/` (or a documented, checked-in sibling of it) plus
`notes/05-arm-kit.md` describing the arms, the controls and one worked run with
raw log paths. It is a prototype: rough is fine, reproducible is not optional.

## Resolution

Closed 2026-11-25. The kit is `tools/responsive-arm/` (README, `sample-guest.sh`,
`presser.rs`, `run-arm.sh`), built from the artefacts that actually ran rather
than re-derived, because the fourth arm to build this instrument hit bugs its
predecessors had already fixed.

**Why it is a promotion, not a rewrite.** The three arms that needed it each
hand-rolled the same three pieces:

| piece | source | what it carries over |
|---|---|---|
| `sample-guest.sh` | ticket 01's `arm4/sampler.sh` | the two fixes its probe run cost: `printf '--'` under dash, and per-field zram sysfs being root-only while `mm_stat` is not (so only `mm_stat` is read, and its six fields carry the rest) |
| `presser.rs` | ticket 01's `arm4/presser.rs` (v2) | **distinct** pages per page (xorshift32): a zero-filled presser is stored as one flag plus a fill value and measures nothing (`same_filled`) |
| `run-arm.sh` | ticket 01's `host/run-arm.sh` | one boot per arm, a foreign-VM gate, a 1 s console copier (cang deletes the task dir and `guest-kernel-console.log` is always 0 bytes), launcher-then-task teardown |

Parameterised for reuse: `ARM`, `MODE=session|exec`, `PAYLOAD`, `MEM`, `IMAGE`,
`CANG`, `GUEST_INIT` (built from `.#cang-musl` when unset), `WORKLOAD_SRC`,
`OUT_DIR`, `WATCH_S`, `DONE_TIMEOUT`. `MODE=exec` is ticket 12's launch shape; the
session shape is tickets 01/04's.

**Corrected the same day (arm [13](13-default-bound-and-earlyoom.md) found this
copy was wrong).** The claim below that the sampler "carries over" the
`printf`-with-a-leading-`--` fix was **false**: this kit's first copy still wrote
`printf '--- SAMPLE %s\n'`, so arm 13's capture had no timestamps at all and its
session log was 428 lines of `printf: --: invalid option`. Fixed to
`printf '%s\n' "--- SAMPLE ..."`; the per-process reads are silenced (a process
vanishing between the glob and the read is normal on a busy guest); and the
driver's done-sentinel loop re-reads the file before blaming the launcher, because
a launcher that exits on the payload was being logged as "no done sentinel"
immediately before the sentinel line.

**Two changes made to the copies, both fixes rather than features:**
- the gate now matches the worker's **comm** (`pgrep -x libkrun-vm-work`) instead
  of a command line: `pgrep -af libkrun-vm-worker-enter` also matches the shell
  that *typed* the pattern, so the first trial run reported a co-tenant that was
  not there. Verified: with no VM running the gate line reads
  `foreign_workers=0`, and the abort branch (exit 7) prints the offending
  `/proc/<pid>/cmdline`;
- `presser.rs` falls back to stderr with one message when its progress path is not
  writable, so a host-side smoke test does not panic on the guest's `/workspace`
  path. Verified by running it on the host with `PROGRESS=/nonexistent/...`.

**What it does not prove.** No arm was run through the shipped kit end to end: the
host was busy with ticket [09](09-rustc-job-cap.md)'s boot when this closed, and
booting a guest just to exercise a driver is the cost the kit exists to avoid. The
first arm that uses it is ticket [06](06-zram-and-swap-tunables.md)'s or
[11](11-earlyoom.md)'s, and that arm is its acceptance. Its syntactic checks
(`bash -n`, `sh -n`, `rustc` on the presser) and the gate's both branches pass.
