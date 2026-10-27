# cang /workspace: does FLUSH removal and the cacheable negative entry pay?

Campaign run 2026-10-27 03:49–04:18 UTC. Binary `cang 0.14.0` from
`/home/dev/cang/cang-vfio/result-inst2/bin/cang`. Harness
`tools/rust-compile-bench/run.sh --arms guest-virtiofs|host --runs 1 --workload
synthetic`; workload tree unchanged (2401 crates / 19204 files / 151 MB); image
`ghcr.io/zeroqn/cang:latest` already loaded; every arm under `timeout 2400` with
its own log under `campaign/`. One VM at a time; no host-global pkill, no podman
store command, no git command.

## Verdict

**`CANG_FS_NO_FLUSH=1` — mechanically exact, timing gain marginal, no
correctness loss. Ship it.**
FLUSH 211,877 → **2** at the 1 M anchor (−100.00 %; the two are the flushes
already in flight before the kernel learns `no_flush` from the first `-ENOSYS`).
Build 171.4 s → 153.5 s (−17.9 s, −10.5 %). The two flush arms (159.5 / 147.5 s)
straddle the two base arms (179.5 / 163.4 s), and the base arms are themselves
16.1 s apart, so treat the −17.9 s as *at the edge of* this rig's noise for a
single pair — the honest statement is "removes 21 % of the requests and the
build got faster by about 10 %, which is outside anything the setting itself can
be blamed for but not by a wide margin". No correctness loss: the smoke's
create/append/read/symlink/unlink all survived with the switch on, and the host
saw the expected file content.

**`CANG_FS_NEGATIVE_ENTRY=1` — mechanically decisive, biggest single gain, and a
real (bounded, documented-shape) correctness cost. Worth shipping only with the
staleness semantics written down (see the coherence section).**
No `neg` arm reaches the 1 M-request milestone at all — its last milestone is
300 k where the base arms cross 1 M. Total requests (extrapolated, §5) fall
1.71 M → 0.89 M (−48 %); at the common 300 k anchor LOOKUP is 178 k → 88 k
(−50.6 % of the count, share 59.5 % → 29.4 %), and the estimated absolute
LOOKUP volume falls ~925 k → ~262 k (−72 %) because 91.5 % of base LOOKUPs were
misses that the kernel now caches. Build 171.4 s → 135.8 s (**−35.6 s, −20.8 %**),
2.2× the same-setting spread and consistent across both arms (133.7 / 137.9 s).
Cost: a file the **host** creates stays invisible to the guest for the remainder
of the 5 s negative-entry lifetime (2042 ms measured; worst case the full 5 s).
Measured timeline in §4; the control arm saw it at the next probe (≤ 0.2 s).

**Both together — additive, and that is the interesting result.** −55.1 s
(−32.2 %) against a sum of the two individual deltas of −53.5 s (within 1.6 s,
3 % of the total). Removing FLUSH does **not** make the negative entry pay less,
and vice versa: the two remove disjoint request classes and the build time is
close to linear in total requests (§5, ~53 µs per request of wall time).

**entry_timeout 60 on top — do not ship.** `neg@60` −40.4 s vs base (4.8 s
better than `neg@5`) and `both@60` −58.9 s (3.8 s better than `both@5`), i.e.
−3.5 % more, *inside* the noise, and it stretches the host→guest staleness of a
created name to a full 60 s. The request evidence explains why it is small: at
the 300 k anchor `neg@60` LOOKUP 74,963 vs `neg@5` 88,194 (−15 %) but GETATTR
21,408 vs 12,444 (+72 %) — the longer negative cache just moves metadata work
from LOOKUP into GETATTR revalidation (same shape the 5 s→60 s positive-entry
arm showed: LOOKUP −2.3 %, GETATTR +39.8 %, no wall-clock change).

## 1. Smoke gate (both switches at once) — PASS

Workspace `/home/dev/cang-bench/smoke`, payload `smoke.sh`, launched
`CANG_FS_NO_FLUSH=1 CANG_FS_NEGATIVE_ENTRY=1 CANG_FS_STATS=1 cang --image
ghcr.io/zeroqn/cang:latest -- sh /workspace/smoke.sh` in the background under
`timeout 600`. Marker `smoke-result.txt` appeared 12 s after launch, `RC=0`.

Guest output (`evidence/levers/smoke.log`, mtime 11:46:20 +0800; helper snapshot mtime 11:46:19):

    hello
    11 f.txt
    STAT_ENOENT_OK
    STAT_ENOENT_AGAIN
    x
    hello
    more
    6 bulk/f1199.txt
    RC=0

* create / read / append / `wc -c` / dir / file-in-dir / symlink / `rm -rf` all
  correct with `-ENOSYS` on FLUSH and with `nodeid==0` lookup replies;
* `stat nonesuch-abc` returned ENOENT **to the guest** both times, so the kernel
  accepts the negative-entry reply as a miss (it does not surface nodeid 0 or an
  error to userspace);
* host side: `/home/dev/cang-bench/smoke/f.txt` = `hello\nmore\n` (11 bytes,
  `od -c` checked), i.e. the writes really landed — nothing was lost by dropping
  FLUSH;
* `wc -c f.txt` prints 11, not the 10 the brief expected: "hello\n"+"more\n" is
  11 bytes; the script's expectation was off by one, not the share.

Histogram check on the captured helper log
(`evidence/levers/helper-all/tasks-smoke-441512-….log`, source mtime 11:46):

    fs: opcode histogram [milestone]: 5000 requests served
    fs:   LOOKUP                       2152   43.04%
    fs:   READ                         1190   23.80%
    fs:   READLINK                     1001   20.02%
    fs:   GETATTR                       205    4.10%
    ...
    fs:   OPEN                            3    0.06%
    fs:   FLUSH                           2    0.04%
    fs:   RELEASE                         1    0.02%
    fs:   OPENDIR                         1    0.02%
    fs:   lookup 2151 positive, 0 negative, 0 error (0.00% of lookups found nothing; 43.02% of requests)

FLUSH is **2**, not literally 0: the kernel has already queued a flush or two
when the first `-ENOSYS` arrives, then sets `no_flush` and never sends another
(it stays at 2 through the 20 k milestone). OPEN 3 / RELEASE 1 / OPENDIR 1 are
the same one-time `-ENOSYS` probes for zero-message open, identical in every
campaign arm — that optimisation is untouched by either switch.

## 2. Coherence cost of the cacheable negative (one arm each way)

Payload stats `/workspace/late-file` every ~0.2 s for 30 s and logs
`LT_MS=<ms since start> EPOCH_MS=<wall> SEEN|ENOENT`; a host-side watcher sleeps
5 s then `touch`es the file inside the workspace dir and logs its own
`HOST_TOUCH_MS`. Files: `evidence/levers/coherence/`.

`CANG_FS_NEGATIVE_ENTRY=1` (timeline-neg.txt, mtime 11:47:48):

    START_MS=1793072837547            (guest wall clock)
    HOST_TOUCH_MS=1793072840570       (host wall clock, +3023 ms after guest start)
    LT_MS=14    EPOCH_MS=1793072837561 ENOENT
    ... 23 samples ENOENT ...
    LT_MS=5065  EPOCH_MS=1793072842612 SEEN      <- 2042 ms after the host created it
    ... 114 samples SEEN to LT_MS=29817
    END_MS=1793072867596

Control, switch off (timeline-ctl.txt, mtime 11:48:21):

    START_MS=1793072870268
    HOST_TOUCH_MS=1793072873218       (+2950 ms after guest start)
    LT_MS=27    EPOCH_MS=1793072870295 ENOENT
    ... 12 samples ENOENT ...
    LT_MS=2859  EPOCH_MS=1793072873127 SEEN      <- 91 ms *before* the host's
                                                  recorded touch (guest clock ~0.1 s
                                                  behind the host's): i.e. at the
                                                  next probe, ≤0.2 s
    ... 116 samples SEEN to LT_MS=29964

So: control = immediate (bounded by the 0.2 s probe interval), negative-entry arm
= invisible for the rest of the negative dentry's lifetime. The dentry was cached
at guest-relative 14 ms and expired at 5065 ms — exactly `entry_timeout` = 5 s
from the *first* lookup, so the worst case is the full timeout and this run
happened to lose 2.0 s of it. With `CANG_FS_ENTRY_TIMEOUT=60` the same loss is
up to 60 s (why I would not ship that variant). The switch's own counter
confirms the mechanism from the other side: the `neg` arm's helper log shows
`lookup 2331 positive, 0 negative` at 5 k requests where the control shows `563
positive, 2536 negative`.

Counter caveat (matters for reading the tables): with the switch on, a missing
name is answered `Ok(negative_entry)` and `server.rs:339-351` counts a LOOKUP as
"negative" only when it returns an `-ENOENT` error, so the split line reads
`N positive, 0 negative, 0 error` **by construction**. Under `neg`, ignore the
split and read the LOOKUP count itself.

## 3. Per-run table (one line per run.sh invocation, in run order)

`build` = BENCH_MS from the arm's own result file; `outer` = run.sh wall clock;
`fs_worker` = mean CPU % of the share's worker thread; requests are from the
campaign's own helper-log snapshots (§6).

| # | label | switches | build s | outer s | est. total req | LOOKUP@300k | FLUSH@300k | GETATTR@300k | READ@300k | fs_worker% | highest milestone |
|---|-------|----------|--------:|--------:|---------------:|------------:|-----------:|-------------:|----------:|-----------:|-------------------|
| 1 | host1 | – | 112.951 | 123.609 | – | – | – | – | – | – | – |
| 2 | base1 | – | 179.453 | 186.936 | 1.75 M | 178,436 (59.5 %) | 55,403 (18.5 %) | 6,627 | 12,815 | 20.1 | 1 M |
| 3 | flush1 | `NO_FLUSH` | 159.536 | 166.594 | 1.34 M | 215,282 (71.8 %) | **2** | 8,829 | 14,834 | 21.6 | 1 M |
| 4 | neg1 | `NEGATIVE_ENTRY` | 133.688 | 140.252 | 0.93 M | 88,194 (29.4 %) | 98,002 (32.7 %) | 12,444 | 18,412 | 18.8 | 300 k |
| 5 | both1 | both | 114.760 | 121.988 | 0.69 M | 110,509 (36.8 %) | **2** | 21,671 | 26,772 | 18.7 | 300 k |
| 6 | both2 | both | 117.849 | 124.243 | 0.65 M | 110,338 (36.8 %) | **2** | 21,747 | 26,723 | 17.4 | 300 k |
| 7 | neg2 | `NEGATIVE_ENTRY` | 137.881 | 144.200 | 0.84 M | 87,310 (29.1 %) | 98,224 (32.7 %) | 12,546 | 18,376 | 17.6 | 300 k |
| 8 | flush2 | `NO_FLUSH` | 147.460 | 154.327 | 1.41 M | 214,992 (71.7 %) | **2** | 8,843 | 14,890 | 21.8 | 1 M |
| 9 | base2 | – | 163.397 | 170.556 | 1.67 M | 177,550 (59.2 %) | 55,819 (18.6 %) | 6,613 | 12,848 | 21.7 | 1 M |
| 10 | neg60 | `NEG`+`ENTRY_TIMEOUT=60` | 130.993 | 138.167 | 0.96 M | 74,963 (25.0 %) | 99,995 (33.3 %) | 21,408 | 18,559 | 18.9 | 300 k |
| 11 | both60 | both + `ENTRY_TIMEOUT=60` | 112.483 | 120.000 | 0.67 M | 95,298 (31.8 %) | **2** | 33,108 | 27,058 | 18.8 | 300 k |
| 12 | host2 | – | 117.389 | 128.368 | – | – | – | – | – | – | – |

Request counts at the 1 M milestone (the only arms that reach it):

| arm | LOOKUP | FLUSH | GETXATTR | WRITE | READ | GETATTR | READLINK | lookup positive/negative |
|-----|-------:|------:|---------:|------:|-----:|--------:|---------:|--------------------------|
| base1 | 540,997 (54.10 %) | 211,877 (21.19 %) | 40,525 | 42,291 | 32,674 | 29,280 | 23,368 | 45,851 pos, 495,146 neg (91.52 % miss) |
| base2 | 540,535 (54.06 %) | 212,194 (21.22 %) | 40,585 | 42,360 | 32,715 | 29,257 | 23,399 | 44,703 pos, 495,837 neg (91.52 % miss) |
| flush1 | 688,493 (68.85 %) | **2** | 49,569 | 50,334 | 36,461 | 38,782 | 30,866 | 49,997 pos, 638,495 neg |
| flush2 | 688,057 (68.81 %) | **2** | 49,607 | 50,390 | 36,504 | 38,776 | 30,914 | 48,620 pos, 639,440 neg |

The baseline reproduces the ticket's numbers exactly: LOOKUP 54.1 % of requests,
FLUSH 21.2 %, WRITE 4.2 %, GETXATTR 4.1 %, READ 3.3 %, READLINK 2.3 %, and 91.5 %
of lookups negative.

## 4. Time deltas

Base mean = (179.453 + 163.397)/2 = **171.4 s**. Host anchors 112.951 → 117.389 s
= **+3.9 %** end to end across the 29-minute campaign (within the ~5 % yardstick,
slightly against the later arms).

| setting | build s (mean) | Δ vs base | % | same-setting spread |
|---------|---------------:|----------:|--:|--------------------:|
| base | 171.4 | – | – | **16.1 s (8.9 %)** |
| flush | 153.5 | −17.9 | −10.5 % | 12.1 s (7.9 %) |
| neg | 135.8 | −35.6 | −20.8 % | 4.2 s (3.1 %) |
| both | 116.3 | −55.1 | −32.2 % | 3.1 s (2.7 %) |
| neg@60 | 131.0 | −40.4 | −23.6 % | 1 arm |
| both@60 | 112.5 | −58.9 | −34.4 % | 1 arm |

Inside/outside noise: the rig's own spread for an identical setting is 16.1 s
here (9 %), not the ~4 % a previous campaign showed — that is the number to
quote, and by it `flush`'s −17.9 s is only just outside, `neg`'s −35.6 s and
`both`'s −55.1 s are well outside. Interleaving is what makes this readable: the
guest times trace a clean V (179.5, 159.5, 133.7, 114.8 | 117.8, 137.9, 147.5,
163.4), i.e. the two ends of the campaign (both `base`) agree to 16 s while the
middle (`both`) is 55 s lower, so the ordering is not a monotonic drift story.

Additivity: −17.9 (flush) + −35.6 (neg) = −53.5 s vs −55.1 s measured for both —
additive within 1.6 s. Request estimate agrees: 1.71 M → 1.37 M (flush removes
0.34 M) → 0.89 M (neg removes 0.82 M) → 0.67 M (both removes 1.04 M, i.e. the
sum of the two minus 0.12 M).

The win is far bigger than the 4 % the rig showed before, for `neg` and `both`;
for `flush` alone it is about the size of the noise band.

## 5. Pricing one request (why the two add)

Requests per second in the last measured interval, and the extrapolation used in
`est. total req` (last milestone + last-interval rate × remaining build time;
the request **rate** is a direct observation, only the multiplication is an
estimate, ±10 %):

| arm | rate over last interval | remaining build | est. total |
|-----|-------------------------|-----------------|-----------:|
| base1 | 11.59 k/s | 65.1 s | 1.75 M |
| base2 | 11.13 k/s | 60.3 s | 1.67 M |
| flush1 | 9.06 k/s | 37.0 s | 1.34 M |
| flush2 | 10.71 k/s | 38.1 s | 1.41 M |
| neg1 | 7.65 k/s | 82.0 s | 0.93 M |
| neg2 | 6.56 k/s | 83.1 s | 0.84 M |
| both1 | 6.56 k/s | 58.9 s | 0.69 M |
| both2 | 6.28 k/s | 55.4 s | 0.65 M |
| neg60 | 8.06 k/s | 81.4 s | 0.96 M |
| both60 | 6.63 k/s | 55.1 s | 0.67 M |

A two-point fit on (1.71 M, 171.4 s) and (0.67 M, 116.3 s) gives **~53 µs of build
wall time per FUSE request** plus ~81 s of guest CPU that no share change touches.
It predicts the two arms it was not fitted on: flush 153.4 s vs 153.5 s observed;
neg 127.7 s vs 135.8 s observed (6 % high). That is why removing two disjoint
classes adds: each request removed is worth about the same, so the two deltas
stack. The rate falls as requests fall (11.6 → 6.3 k/s) because the guest's own
compile CPU becomes the limiter — the share is no longer the bottleneck in the
`both` arms.

## 6. Verbatim histogram lines and where they came from

Snapshots are the last copy of each run's `helper.stderr.log` before cang deleted
the task dir, taken by a stamp-guarded watcher into
`/home/dev/cang-bench/evidence/levers/helper2/`; the `.stamp` file records
`<source mtime> <source size> <copied-at>` so freshness is checkable without
trusting the copy's own mtime. Arm↔task mapping is by the log's first timestamp
against the run start in `campaign/table.tsv` (e.g. base1 started 03:51:19, its
log's first line 03:51:24).

`tasks-guest-virtiofs-484242-…log` (base1, snapshot mtime 11:54:26, source stamp
`1793073265 18861 1793073266`):

    [2026-10-27T03:53:19.182807Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 1000000 requests served
    [2026-10-27T03:53:19.182840Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                     540997   54.10%
    [2026-10-27T03:53:19.182852Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                      211877   21.19%
    [2026-10-27T03:53:19.182876Z WARN  krun_devices::virtio::fs::server] fs:   READ                        32674    3.27%
    [2026-10-27T03:53:19.182884Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     29280    2.93%
    [2026-10-27T03:53:19.183116Z WARN  krun_devices::virtio::fs::server] fs:   lookup 45851 positive, 495146 negative, 0 error (91.52% of lookups found nothing; 54.10% of requests)

`tasks-guest-virtiofs-517294-…log` (flush1, snapshot mtime 11:57:15, source stamp
`1793073397 18626 1793073435`):

    [2026-10-27T03:56:37.013200Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 1000000 requests served
    [2026-10-27T03:56:37.013240Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                     688493   68.85%
    [2026-10-27T03:56:37.013271Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     38782    3.88%
    [2026-10-27T03:56:37.013280Z WARN  krun_devices::virtio::fs::server] fs:   READ                        36461    3.65%
    [2026-10-27T03:56:37.013528Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                           2    0.00%
    [2026-10-27T03:56:37.013609Z WARN  krun_devices::virtio::fs::server] fs:   lookup 49997 positive, 638495 negative, 0 error (92.74% of lookups found nothing; 68.85% of requests)

`tasks-guest-virtiofs-546442-…log` (neg1, snapshot mtime 11:59:38, source stamp
`1793073564 15140 1793073578`) — last milestone, the arm never reaches 1 M:

    [2026-10-27T03:58:15.459347Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 300000 requests served
    [2026-10-27T03:58:15.459386Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                       98002   32.67%
    [2026-10-27T03:58:15.459396Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                      88194   29.40%
    [2026-10-27T03:58:15.459412Z WARN  krun_devices::virtio::fs::server] fs:   READ                        18412    6.14%
    [2026-10-27T03:58:15.459427Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     12444    4.15%
    [2026-10-27T03:58:15.459666Z WARN  krun_devices::virtio::fs::server] fs:   lookup 88195 positive, 0 negative, 0 error (0.00% of lookups found nothing; 29.40% of requests)

`tasks-guest-virtiofs-571648-…log` (both1, snapshot mtime 12:01:42, source stamp
`1793073656 15024 1793073702`):

    [2026-10-27T04:00:42.553170Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 300000 requests served
    [2026-10-27T04:00:42.553212Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                     110509   36.84%
    [2026-10-27T04:00:42.553240Z WARN  krun_devices::virtio::fs::server] fs:   READ                        26772    8.92%
    [2026-10-27T04:00:42.553248Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     21671    7.22%
    [2026-10-27T04:00:42.553401Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                           2    0.00%
    [2026-10-27T04:00:42.553462Z WARN  krun_devices::virtio::fs::server] fs:   lookup 110509 positive, 0 negative, 0 error (0.00% of lookups found nothing; 36.84% of requests)

`tasks-guest-virtiofs-702478-…log` (neg60, snapshot mtime 12:14:07, source stamp
`1793074440 15140 1793074447`) — the GETATTR shift at `entry_timeout=60`:

    [2026-10-27T04:12:44.373414Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 300000 requests served
    [2026-10-27T04:12:44.373497Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                       99995   33.33%
    [2026-10-27T04:12:44.373507Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                      74963   24.99%
    [2026-10-27T04:12:44.373514Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     21408    7.14%
    [2026-10-27T04:12:44.373536Z WARN  krun_devices::virtio::fs::server] fs:   READ                        18559    6.19%
    [2026-10-27T04:12:44.373790Z WARN  krun_devices::virtio::fs::server] fs:   lookup 74964 positive, 0 negative, 0 error (0.00% of lookups found nothing; 24.99% of requests)

All ten guest arms: `OPEN=3`, `RELEASE=1`, `OPENDIR=1`, `RELEASEDIR=0` at every
milestone — zero-message open intact, unchanged by either switch.

## 7. What went wrong, what was discarded, and what to distrust

1. **No teardown histogram.** Every arm's log ends at its last milestone: cang
   kills the VMM at teardown, so the final `[teardown]` histogram (which would
   give exact totals) is never flushed. That is why total volume is an
   extrapolation (§5) and why the `neg`/`both` totals are bounded only as
   "≥ 300 k, < 1 M" by direct observation.
2. **The first smoke was discarded as histogram evidence.** It did the right
   file operations (f.txt = `hello\nmore\n` on the host, both negative stats
   ENOENT) but issued <5 k requests, so no milestone fired, and the brief's narrow
   watcher (globbing only `guest-virtiofs`) captured nothing for a workspace
   named `smoke`. I re-ran it with a 1200-iteration create/read/stat loop and a
   watcher that globs every workspace; the §1 evidence is that second run.
3. **A plain `cp -f` watcher destroys the evidence it exists to preserve.** While
   the broad watcher ran it re-copied every *stale* task dir from earlier
   sessions each second, refreshing their mtimes (a 900 KB co-tenant log
   `live-ioctl-…` and several `cang-flag-*` logs appeared under my evidence
   dir). Replaced by a watcher that only copies files whose source mtime is newer
   than a marker taken at campaign start, and that records the source mtime/size
   in a `.stamp`. Do not quote an mtime from a watcher that rewrites its own
   outputs.
4. **The rig's identical-setting spread is 8.9 %, not ~4 %.** Two identical `base`
   arms came out 179.5 s and 163.4 s. The V-shape of the interleaved order argues
   this is not monotonic drift (both ends of the campaign are the slow ones, the
   middle is the fast one). Treat every single-pair delta under ~8 s as
   undecided — which is exactly why `flush`'s −17.9 s is reported as marginal
   while its request removal (211,877 → 2) is not in doubt.
5. **A stale sampler from an earlier session was running throughout**: pid 1146062,
   `mon-threads.sh /home/dev/cang-bench-cang9/logs/threads-guest-virtiofs-1.txt`,
   up for ~24 h, plus a dozen `tail -F` orphan processes over dead helper logs. Not
   mine, so not killed (project rule 195); they add a little scheduling noise to
   every arm equally.
6. **`lookup_negative` is unreadable under the switch** (see §2): it counts only
   `-ENOENT` error replies (`server.rs:339-351`), so `neg`/`both` arms always print
   `0 negative`. Not a bug to fix before shipping, but the field's meaning should
   be documented next to the switch.
7. `neg60` and `both60` are single arms (the brief's order gives one each) and
   their deltas (−3.5 / −3.8 s vs the 5 s variants) are inside the noise — that
   conclusion is "no demonstrated gain", not "proven no gain".
8. Not touched, by rule: no host-global pkill, no `podman prune/rmi/system reset`,
   no git checkout/reset, no tracked file in the repo modified. All artifacts
   under `/home/dev/cang-bench/evidence/levers/` (`campaign/`, `coherence/`,
   `helper2/`, `helper/`, `helper-all/`, `smoke.log`); `/home` stayed at ~90 %.

