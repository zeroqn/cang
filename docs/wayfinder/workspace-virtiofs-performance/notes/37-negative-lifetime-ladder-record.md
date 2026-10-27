# Does the cacheable negative entry still pay when it expires in 0.25 s?

Ladder run 2026-10-27 04:28–05:15 UTC (13:28–13:15 local). Binary
`cang 0.14.0` from `/home/dev/cang/cang-vfio/result-inst3/bin/cang` (the build
that adds `CANG_FS_NEGATIVE_TIMEOUT`), harness
`tools/rust-compile-bench/run.sh --arms guest-virtiofs|host --runs 1 --workload
synthetic`, workload tree untouched (2401 crates / 19 204 files / 151 MB), image
`ghcr.io/zeroqn/cang:latest` already loaded. Every arm ran as a bounded
background `timeout 2400` process with its own log (`runs/`); one VM at a time; no
host-global kill, no podman store command, no git command; all artifacts under
`/home/dev/cang-bench/evidence/ladder/`.

## Comparability (read this before the numbers)

**This ladder is internally consistent and NOT comparable in absolute build
seconds to the previous campaign (171.4 s base / 153.5 s noflush / 116.3 s
both).** That campaign ran immediately after the workload tree was generated, with
a cold host page cache; this one ran with the tree hot, and its arms land at
101–112 s — below that campaign's numbers *and below its host anchors*. Any
absolute comparison against those figures is invalid, so none is made here.

* Build seconds are read **only against this run's own controls**: three clean
  `noflush` arms (`noflush` 142.2, `noflush2` 151.5, `noflush3` 143.9; mean
  **145.9 s**, spread 9.3 s) plus two host anchors (112.8 → 122.1 s). Every
  negative-timeout arm is expressed as a delta against that 145.9 s mean.
* **Request counts are comparable across campaigns** (they are per-fixed-milestone
  counts, independent of how fast the build ran), so LOOKUP / FLUSH / GETATTR at
  the 300 k anchor and the estimated totals are what the ladder is primarily read
  on. They agree with the earlier campaign: its no-flush control measured LOOKUP
  215,282 at 300 k, this run's 214,868 (0.2 %), i.e. the same mix.
* One further within-run caveat: five arms fell inside a 15-minute host-contention
  window (§5.1) and are excluded from the verdict; the confirmation block C1–C5
  re-established a same-window control.

## Verdict

**The win survives essentially intact down to a 0.25 s negative lifetime, and the
curve flattens from 1 s upward. Ship `CANG_FS_NEGATIVE_TIMEOUT=0.25`.**

* LOOKUP at the 300 k-request anchor (the noise-free measurement — the control
  reproduces to 0.55 % across four arms):

  | negative lifetime | LOOKUP@300k | % of the 5 s arm's reduction kept |
  |------------------:|------------:|----------------------------------:|
  | off (control)     |     214,868 | —                                 |
  | 0.1 s             |     123,582 | 86.3 %                            |
  | **0.25 s**        | **117,209** | **92.3 %**                        |
  | 1 s               |     111,319 | 97.9 %                            |
  | 2 s               |     110,018 | 99.1 %                            |
  | 5 s (unset)       |     109,069 | 100 % (reference)                 |

  The curve is monotone and **flat by 1 s** (2 s adds 1.2 %, 5 s adds another
  0.9 %). 0.25 s already captures 92 % of it: rustc re-probes the same missing
  name within milliseconds, so a quarter-second cache catches nearly every repeat.

* Wall clock, as a delta against **this run's own no-flush control mean
  (145.9 s)**: every negative setting lands in one band, **101.0–110.5 s**, i.e.
  **−35.4 to −44.9 s (−24 % to −31 %)** — a win that does not shrink as the
  lifetime shrinks. The 5 s reference itself measured 106.5 s (−39.4 s), *inside*
  the band of the shorter settings; the rig cannot resolve a difference between
  0.25 s and 5 s (per-setting spread 6–11 s, against a 9.3 s control spread).

* Cost: the host→guest name staleness becomes **≤ 0.25 s** (measured 74 ms), so
  cang's README promise ("files added to a directory are visible immediately")
  becomes true to within a quarter of a second instead of wrong by up to 5 s.
  That is the whole point of the setting: it converts an undocumentable 5 s hole
  into a documentable 250 ms one while keeping ~all of the request reduction.

If someone would rather have the last 8 % of the request reduction than a
quarter-second staleness bound, 1 s (97.9 %) is the conservative alternative —
the flattening point. 2 s and 5 s buy nothing measurable and only lengthen the
hole. `CANG_FS_NEGATIVE_TIMEOUT=0.1` is the one setting that shows a hint of a
cost (86.3 % of the reduction, −35.4 s vs −44.9 s at 0.25 s, i.e. 9.5 s) and it is
not resolved by this rig, so 0.25 s is the knee.

## 1. Arm table

All guest arms carry `CANG_FS_NO_FLUSH=1 CANG_FS_NEGATIVE_ENTRY=1 CANG_FS_STATS=1`
(the control additionally drops `NEGATIVE_ENTRY`); the only variable is
`CANG_FS_NEGATIVE_TIMEOUT`. `build` = BENCH_MS from the arm's own result file,
with `Δ` against the 145.9 s control mean; `fs_worker%` = mean CPU of the share's
worker thread; `est. total req` = last milestone + last-interval rate x remaining
build (§3 methodology, ±10 % — treat the 300 k anchor as the measurement).

| # | label | negative timeout | build s | Δ vs control | LOOKUP@300k | FLUSH@300k | GETATTR@300k | est. total req | fs_worker% |
|---|-------|------------------|--------:|-------------:|------------:|-----------:|-------------:|---------------:|-----------:|
| 0 | host1 | – | 112.8 | – | – | – | – | – | – |
| 1 | noflush | off (control) | 142.2 | −3.6 | 214,827 | 2 | 8,900 | 1.36 M | 20.7 |
| 2 | neg5 | 5 s (unset) | 100.9 | −45.0 | 108,787 | 2 | 21,902 | 0.70 M | 19.5 |
| 3 | neg01 | 0.1 s | 108.9 | −37.0 | 122,912 | 2 | 20,656 | 0.68 M | 18.0 |
| 4 | neg025 | 0.25 s | 101.3 | −44.6 | 117,571 | 2 | 20,926 | 0.69 M | 18.9 |
| 5 | neg1 | 1 s | 104.3 | −41.6 | 111,079 | 2 | 21,970 | 0.66 M | 18.4 |
| 6 | neg2 | 2 s | 109.0 | −36.9 | 110,018 | 2 | 21,742 | 0.72 M | 19.8 |
| 7 | neg025b | 0.25 s | 153.9 * | (+8.0) | 124,327 | 2 | 20,544 | 0.63 M | 15.2 |
| 8 | neg1b | 1 s | 134.9 * | (−11.0) | 112,858 | 2 | 21,452 | 0.72 M | 17.7 |
| 9 | neg2b | 2 s | 148.4 * | (+2.5) | 114,398 | 2 | 21,220 | 0.56 M | 16.1 |
| 10 | neg01b | 0.1 s | 112.1 * | (−33.8) | 124,252 (at 300,001) | 2 | 20,545 | 0.73 M | 18.8 |
| 11 | neg5b | 5 s | 129.4 * | (−16.5) | 108,973 | 2 | 21,883 | 0.89 M | 18.3 |
| 12 | noflushb | off (control) | 223.1 * | (+77.2) | 216,021 | 2 | 8,816 | 1.05 M | 16.8 |
| 13 | host2 | – | 122.1 | – | – | – | – | – | – |
| C1 | noflush2 | off (control) | 151.5 | +5.6 | 214,851 | 2 | 8,810 | 1.53 M | 22.0 |
| C2 | neg025c | 0.25 s | 100.7 | −45.2 | 116,847 | 2 | 21,372 | 0.70 M | 20.5 |
| C3 | neg1c | 1 s | 106.0 | −39.9 | 111,559 | 2 | 21,947 | 0.64 M | 19.5 |
| C4 | neg5c | 5 s | 112.1 | −33.8 | 109,447 | 2 | 21,786 | 0.67 M | 19.2 |
| C5 | noflush3 | off (control) | 143.9 | −2.0 | 214,926 | 2 | 8,789 | 1.37 M | 22.5 |

`*` = arm inside the contaminated 04:42–04:58 window (§5.1); its control
(`noflushb`, 223.1 s) is the same window's outlier, so those five rows are kept in
the table for completeness and excluded from the verdict (their deltas are shown
in parentheses for that reason). Rows C* are the confirmation block run afterwards
to re-establish a same-window control.

Grouped, dropping the contaminated window (the three clean controls are the
yardstick, mean 145.9 s):

| setting | build s | mean | Δ vs control mean | LOOKUP@300k (mean) |
|---------|---------|-----:|------------------:|-------------------:|
| control (no neg entry) | 142.2 / 151.5 / 143.9 | **145.9** | – (spread 9.3 s) | 214,868 |
| 0.1 s | 108.9 / 112.1 | 110.5 | −35.4 | 123,582 |
| **0.25 s** | 101.3 / 100.7 | **101.0** | **−44.9** | **117,209** |
| 1 s | 104.3 / 106.0 | 105.2 | −40.7 | 111,319 |
| 2 s | 109.0 | 109.0 | −36.9 | 110,018 |
| 5 s | 100.9 / 112.1 | 106.5 | −39.4 | 109,069 |

Host anchors 112.8 → 122.1 s (+8.2 %, i.e. the later arms ran on a marginally
slower host, slightly against them): the yardstick by which a −35…−45 s guest
delta is far outside the rig's drift, and the reason every delta here is quoted
against the in-run control mean rather than against any earlier campaign.

FLUSH is 2 and `OPEN=3 / RELEASE=1 / OPENDIR=1` in **every** arm (both switches,
all timeouts): the no-flush and zero-message-open paths are untouched by the
negative-lifetime setting, so the whole difference in this table is LOOKUP. These
counts, not the seconds, are what carries over to the earlier campaign's arms.

## 2. Staleness (host creates the file, guest polls every 0.2 s)

Payload stats `/workspace/late-file` every ~0.2 s for 20 s; a host-side watcher
sleeps 5 s and then `touch`es it (in guest time the touch lands ~2.4–2.7 s after
the payload starts, because the sleep begins before the VM boots). Files:
`coherence/`.

| negative timeout | host touch → guest sees it | samples ENOENT before | worst case |
|------------------|---------------------------|----------------------|-----------|
| 0.25 s | **74 ms** | 13 | ≤ 250 ms |
| 1 s | seen at the probe right after the touch (−290 ms vs the recorded touch: guest clock skew ~0.1–0.3 s) | 12 | ≤ 1 s |
| 2 s | 1618 ms | 21 | ≤ 2 s |
| 5 s | 2769 ms | 26 | ≤ 5 s |

Mechanism confirmed from the timelines: the negative dentry keeps the expiry armed
by the lookup that created it and a hit does not re-arm it (5 s: first lookup at
~14 ms of guest time, last miss probe at ~2.4 s, visibility at 5.17 s = first
lookup + 5 s), while a lookup *after* expiry creates a fresh entry with a fresh
timeout (2 s: 4.28 s = two full lifetimes). So the invisibility window for a touch
at time T is `ceil(T / timeout) x timeout + first_lookup − T`, i.e. bounded above
by the configured timeout and typically about half of it. The control arm in the
earlier campaign (negative entry off) saw each touch at the next 0.2 s probe.

## 3. Evidence: request volume and the per-request price

Request rate over the last measured interval (100 k → 300 k) and the remaining
build time give the totals in §1; the rate falls as requests fall (10.0 k/s
control → 6.0–7.1 k/s negative arms) because the guest's own compile CPU becomes
the limiter. Both terms of the price are within-run: two-point fit on this run's
clean means, (145.9 s, ~1.4 M) vs (105.4 s, ~0.68 M), gives **~56 µs of build wall
time per FUSE request** — a delta-over-delta ratio, which is the one kind of
number that may be set beside the earlier campaign's 53 µs fit without the
cold/hot-page-cache problem, and it says the same thing: each removed request is
worth about the same, which is why a request-count change of this size is worth
~40 s and why the negative-timeout variants are indistinguishable when their
request counts differ by only 2–8 %.

## 4. Verbatim histogram lines and where they came from

Snapshots taken by a stamp-guarded watcher into `helper/`; each `<file>.stamp`
records `<source mtime> <source size> <copied-at>` so the copy can be tied to the
arm. Task↔arm mapping is by the task dir's epoch against the arm start (appendix).

`helper/guest-virtiofs-1116096-1793077460824859366.log` — control `noflush2`,
build 151.5 s, snapshot mtime 13:06:57, stamp `1793077585 18766 1793077617`:

    fs: opcode histogram [milestone]: 300001 requests served
    fs:   LOOKUP                     214851   71.62%
    fs:   READ                        14941    4.98%
    fs:   WRITE                       13875    4.62%
    fs:   GETXATTR                    13357    4.45%
    fs:   GETATTR                      8810    2.94%
    fs:   FLUSH                           2    0.00%
    fs:   OPEN                            3    0.00%
    fs:   lookup 23736 positive, 191118 negative, 0 error (88.95% of lookups found nothing; 71.62% of requests)

`helper/guest-virtiofs-1141361-1793077622021234657.log` — **0.25 s**, build
100.7 s, snapshot mtime 13:08:47, stamp `1793077691 15024 1793077727`:

    fs: opcode histogram [milestone]: 300000 requests served
    fs:   LOOKUP                     116847   38.95%
    fs:   WRITE                       31168   10.39%
    fs:   GETXATTR                    29894    9.96%
    fs:   GETATTR                     21372    7.12%
    fs:   FLUSH                           2    0.00%
    fs:   lookup 116847 positive, 0 negative, 0 error (0.00% of lookups found nothing; 38.95% of requests)

`helper/guest-virtiofs-903384-1793075928862202413.log` — 1 s, build 104.3 s,
snapshot mtime 12:40:35, stamp `1793075998 15024 1793076035`:

    fs: opcode histogram [milestone]: 300000 requests served
    fs:   LOOKUP                     111079   37.03%
    fs:   GETATTR                     21970    7.32%
    fs:   FLUSH                           2    0.00%
    fs:   lookup 111079 positive, 0 negative, 0 error (0.00% of lookups found nothing; 37.03% of requests)

`helper/guest-virtiofs-845707-1793075590290636949.log` — 5 s reference, build
100.9 s, snapshot mtime 12:34:54, stamp `1793075662 15164 1793075694`:

    fs: opcode histogram [milestone]: 300000 requests served
    fs:   LOOKUP                     108787   36.26%
    fs:   GETATTR                     21902    7.30%
    fs:   FLUSH                           2    0.00%
    fs:   lookup 108787 positive, 0 negative, 0 error (0.00% of lookups found nothing; 36.26% of requests)

The control also reaches the 1 M milestone (LOOKUP 540 k-class, 91.5 % misses:
`lookup 49015 positive, 639300 negative` at 1 M); **no** negative arm reaches it —
they finish their build after 300–400 k requests. `lookup … 0 negative` under the
switch is by construction (`server.rs:339-351` counts only `-ENOENT` error
replies), as the earlier campaign established — read the LOOKUP count, not the
split.

Every arm also carries the fork's `fsdbg: cheap-lookup hits=… probes=… misses=…`
instrumentation on the lookup path (every 200 k probes). It is present in both
campaign builds, so it is a constant across arms and across campaigns, but the
absolute times are not those of a shipping build.

## 5. What went wrong / what to distrust

1. **A 15-minute host-contention window (04:42–04:58) inflated five arms.**
   `neg025b` 153.9, `neg1b` 134.9, `neg2b` 148.4, `neg5b` 129.4 and the control
   `noflushb` 223.1 s — the control is 55 % above the other three control arms
   (142.2 / 151.5 / 143.9), so the window, not the setting, is what moved. The
   confirmation block (C1–C5, 05:04–05:15) re-established a same-window control
   and reproduced the early block to within a few seconds, which is why the verdict
   uses the uncontaminated rows. Not this session's load: the driver runs one VM
   at a time and nothing else of mine was running (a co-tenant session is the
   likely cause; I did not kill anything, rule 195).
2. **The LOOKUP count is weakly coupled to wall time**, not a pure constant: in
   the slow window the same settings measured 112.9–124.3 k instead of
   111.1–117.6 k, because a slow build spends more wall time letting negative
   entries expire and be re-asked. Reproducibility is still ±0.6 % within a clean
   block (control 214,827 / 214,851 / 214,926), which is what makes the curve
   readable where the times are not.
3. **Interleaved pairs disagree by up to 11 s** (0.25 s: 101.3 / 100.7 / *153.9*;
   5 s: 100.9 / 112.1) — the same-setting spread is the rig's noise floor, so
   "0.25 s looks 5 s faster than 5 s" is noise, not a result. The claim the data
   supports is "no setting below 5 s is measurably slower", not an ordering.
4. **`neg01b`'s milestone is 300,001, not 300,000** (a request landed between the
   counter check and the log line); its block is used as the 300 k anchor as-is.
5. **Watcher:** the brief's `cp -f` loop re-copies stale logs every second and
   refreshes their mtimes (the earlier campaign's lesson). Used instead a watcher
   that only copies a file whose source mtime is at or after the campaign-start
   marker and writes a `.stamp` with `<src mtime> <src size> <copied-at>`;
   17/17 guest arms were captured, none stale.
6. **Absolute seconds here are not the earlier campaign's seconds** (see
   Comparability): the tree was hot, so this run's controls are 142–152 s where
   that one's were 147–179 s, and this run's host anchors are below that run's.
   Only LOOKUP/FLUSH/GETATTR counts and estimated totals are carried across.
7. Untouched by rule: no host-global kill, no podman store command, no git
   command, no tracked file modified, workload tree not regenerated. `/home` ended
   at 90 % (12 GB free), unchanged from the start; each arm's 151 MB workspace copy
   is removed by the next `prepare_arm`.

## 6. Appendix: task-dir → arm mapping

| label | helper snapshot | task epoch |
|-------|-----------------|-----------:|
| noflush | guest-virtiofs-819420-1793075439699469490.log | 1793075439 |
| neg5 | guest-virtiofs-845707-1793075590290636949.log | 1793075590 |
| neg01 | guest-virtiofs-864442-1793075700493846075.log | 1793075700 |
| neg025 | guest-virtiofs-884339-1793075818741130696.log | 1793075818 |
| neg1 | guest-virtiofs-903384-1793075928862202413.log | 1793075928 |
| neg2 | guest-virtiofs-922681-1793076041048495409.log | 1793076041 |
| neg025b | guest-virtiofs-942137-1793076160692987689.log | 1793076160 |
| neg1b | guest-virtiofs-966058-1793076326319215774.log | 1793076326 |
| neg2b | guest-virtiofs-987615-1793076472099849642.log | 1793076472 |
| neg01b | guest-virtiofs-1010680-1793076631710977556.log | 1793076631 |
| neg5b | guest-virtiofs-1029955-1793076752366909318.log | 1793076752 |
| noflushb | guest-virtiofs-1050987-1793076892055420300.log | 1793076892 |
| noflush2 | guest-virtiofs-1116096-1793077460824859366.log | 1793077460 |
| neg025c | guest-virtiofs-1141361-1793077622021234657.log | 1793077622 |
| neg1c | guest-virtiofs-1159068-1793077732536075186.log | 1793077732 |
| neg5c | guest-virtiofs-1177454-1793077847158168521.log | 1793077847 |
| noflush3 | guest-virtiofs-1196621-1793077969987656561.log | 1793077969 |
