---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Three depth-curve attempts, none of them valid - and the per-arm cache check that would have caught each

**Verdict: the depth-8 collapse is still unmeasured.** Three probe designs in one
session all failed, each for a different reason, and the retraction matters more than
the numbers: any conclusion about "the block path collapses with concurrent readers"
drawn from these runs would be unsupported.

## 1. What each attempt got wrong

| attempt | design | why it is invalid |
|---|---|---|
| `run-depth-20261115T022347Z` | 1/8 readers, `dd` per reader | per-reader byte totals summed `tail -1` of several `dd` outputs; arms read *different* file sets |
| `run-depth2-20261115T040334Z` | 1/2/4/8 readers over distinct files, plus tmpfs control | tmpfs payload was 1967 MB inside a 4 GiB guest, so the control arm fought the page cache: tmp8 90 MB/s against tmp4 857 MB/s |
| `run-depth3-20261115T040958Z` | same eight files every arm, disjoint reader subsets, 400 MB tmpfs | the reader subshell backgrounded **every** `dd`, so all arms ran eight concurrent streams regardless of `n`; `d1a` claimed "1 reader" over 510 MB of a 2077 MB set |

## 2. The measurement that should be printed on every arm

`run-depth3` also shows the host page cache was never dropped during a VM run: the
state root's device moved 4-18 % of each arm's bytes.

| arm | readers | wall ms | MB | aggregate MB/s | device MB | device share |
|---|---|---|---|---|---|---|
| `d1a` | 1 | 25924 | 510 | 20 | 93 | 18 % |
| `d2` | 2 | 11420 | 1120 | 98 | 72 | 6 % |
| `d4` | 4 | 7581 | 1502 | 198 | 78 | 5 % |
| `d8a` | 8 | 11621 | 2078 | 179 | 131 | 6 % |
| `d8b` | 8 | 4178 | 2078 | 497 | 85 | 4 % |
| `d1b` | 1 | 13386 | 610 | 46 | 91 | 15 % |
| `tmp4` | 4 | 2458 | 200 | 81 | 0 | 0 % |
| `tmp8` | 8 | 608 | 400 | 658 | 0 | 0 % |

So these arms were **cache-served**, not device-served, and the spread from 20 to
497 MB/s at the same reader count is guest memory/page-cache state, not storage. The
earlier `posix_fadvise(POSIX_FADV_DONTNEED)` drop that *was* verified (8067 -> 1710 MB/s
on a fresh fd) does not take effect against a file a live VM has open and is reading:
pages are re-cached as fast as they are dropped. Any future arm must therefore print
its own `device share` and label itself cache-hot or cold; a "cold" claim without that
column is unsupported.

## 3. What survives from this session

Only the single-file, single-reader comparison, which was repeated and instrument-attributed
([note 09](09-direct-to-guest-reads.md)): pooled staging 5.1-11.1 s, device-thread direct
3.9-4.2 s for 610 MB, with the staging copy at 34-50 % of the arm and the completion
trace falling to zero proving the direct path ran.

## 4. The instrument the depth question actually needs

Not another `dd` shell probe. One guest program that opens N files, reads them at
explicit offsets into one buffer, and reports its own per-phase timings; and a host
control that both drops and *then proves* the drop per arm (device share in the arm
record), so cache-hot and cold arms are never pooled in one table. Until that exists,
the depth-8 question stays open and no fork or backend change should be justified by it.

## Evidence

`/home/dev/disp/blkread/`: `run-depth-20261115T022347Z`, `run-depth2-20261115T040334Z`,
`run-depth3-20261115T040958Z`, probes `probe-attr2.sh`, `probe-depth2.sh`,
`probe-depth3.sh`, runner/control `run-depth3.sh` + `control7.sh`.
