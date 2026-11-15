---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Removing the staging copy: direct-to-guest reads on the device thread, and why it is not enough

**Verdict.** Serving guest reads on the block worker thread - straight into guest
memory, no bounce buffer - is a real win at depth one and no win at depth eight.
Measured in two boots of one binary, engagement proved by the trace going silent:

| mode | arms | 1 reader | 8 readers |
|---|---|---|---|
| pooled staging (default) | `one-a` … `eight-b` | 5140 / 7003 / 11110 / 7448 ms | 22344 / 36045 / 50799 / 47125 ms |
| device-thread direct | `one-a` / `one-b`, `eight-a` / `eight-b` | **4177 / 3876 ms** | 37749 / 46920 ms |

One reader: 610 MB at 146-157 MB/s against the pooled path's 55-124 MB/s - inside
the 1.5x ceiling that §6's copy accounting predicted (copy was 34-50 % of the arm),
and outside the session's 20-30 % spread at the low end. Eight readers: **no
improvement**, 37.7-46.9 s against 22.3-50.8 s pooled.

## 1. Engagement, proved by the instrument rather than by the environment

Two attempts had already failed before the third worked, and both failures are
worth recording because either could fool a "did it take effect" check:

1. Setting `parallel_reads: false` in the fork's `BlockDevice::new` changed
   nothing, because **cang forces the flag on**: `crates/cang-libkrun/src/linked.rs`
   calls `device.set_parallel_reads(true)` with a comment explaining that a guest's
   parallel block I/O otherwise stalls on one device thread. The constructor default
   is dead code in cang's path.
2. Making cang's setter read `CANG_BLOCK_SERIAL_READS` also changed nothing, because
   the run script no longer *passed* the variable - the env-presence check then
   showed `CANG_BLK_TRACE` set and `CANG_BLOCK_SERIAL_READS` absent, which is what
   caught it.

The check that actually discriminates: the fork's read-completion trace exists only
on the pooled path, so `trace_lines` deltas of **0** across every arm prove the
direct path ran. In the two failed attempts the same deltas were 425-431 per
single-reader arm.

## 2. Why depth eight still collapses

With reads on the device thread, guest concurrency has nothing to overlap them
with: the device accumulates 235-241 s of read service time across requests that
are now issued one at a time (`read_ms` 241513 / 235466 against a 37.7-46.9 s wall),
guest iowait rises to 41-68 s, and CPU to 17-22 %. At depth one the same queue
costs 1.2-1.4 s of a 3.9-4.2 s arm.

So the collapse needs **both** properties at once - no staging copy *and* parallel
preads - and this prototype has only the first. The two known ways to get both:

- pool workers that pread into guest memory directly, which needs the non-overlap
  proof the completion-serialization rule requires (overlapping destinations must
  stay on one thread);
- a vhost-user-blk backend, which preads into the guest's descriptors outside the
  VMM by construction.

That reopens the vhost-user question that §7 said the copy's cost made worth asking,
and narrows it: the backend does not need to be faster per request, it needs to keep
the preads parallel while writing guest memory without a bounce buffer.

## 3. Evidence and reproducibility

`run-serial-parallel-20261115T034728Z` (pooled), `run-serial-serial-20261115T035645Z`
(direct; `trace_lines` 0 per arm, `CANG_BLOCK_SERIAL_READS=1`), both on
`/nix/store/4kjxfzpgb23bwh281h4w80jqmzhnmx72-cang-0.21.0` - cang HEAD `838dc14`
with a one-line `linked.rs` toggle, over `--override-input libkrun-src` at pristine
fork HEAD `9ff213f1` plus the `CANG_BLK_TRACE` hunk. The cang edit was reverted
after the build; the artefact is the record. Probes `probe-trace.sh`, control
`control7.sh`, runner `run-serial.sh` (`MODE`, `BUILD`), analyzer `analyze_trace.py`.
Correctness: every arm read exactly its manifest bytes (639150592 / 2178521499) with
no `dd` error, no `csum failed`, and no guest-visible I/O error across four arms of
each mode.
