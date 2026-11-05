# 51 - A cold rust build's request mix, and why the job count does not move it

**Measured 2026-11-05** on `main` `ba84c6a` (cang 0.17.3, fork `v2.0.0-cang.17`),
binary `nix build .#cang-prebuilt` (`cang 0.17.3`), image `localhost/cang:latest`,
guest kernel `7.2.10-hardened1`, `tools/rust-compile-bench` synthetic tree
(2401 crates, 16 801 sources, 151 MB), one boot per arm, `RUNS=1` per arm, no
other VM on the host. Raw evidence: `/home/dev/cang-bench/jobs-attribution-20261105/`
(per-arm summary rows, the helper logs carrying the histograms).

This note exists because a repro kit from another session (`~/cang/repro-kit.tar`,
a 609 s cold build of a 88 GB / 82 342-file target dir at `CARGO_BUILD_JOBS=6`)
could not attribute its own wall time, and the obvious hypothesis was that a low
job count is what makes the share hurt. **It is not.** The share's cost is
request-count-bound and job-independent; more jobs only shrink the numerator it
is being compared against.

## 1. The 2x2: same tree, two job counts, interleaved arms

| `CARGO_BUILD_JOBS` | guest-virtiofs | guest-local (`/dev/shm`) | share's cost | ratio | `fs worker` |
|---|---|---|---|---|---|
| 6 | **58.061 s** | 25.890 s | **32.171 s** | 2.24x | 23.1 % of a core |
| 26 | **49.187 s** | 17.284 s | **31.903 s** | 2.85x | 29.6 % of a core |

Two facts fall out:

- **The share's absolute cost is the same at both job counts** (32.17 s vs
  31.90 s, 0.9 % apart). It is additive, not overlappable: 17.284 + 31.903 =
  49.19 s and 25.890 + 32.171 = 58.06 s, both to the millisecond, which is what
  a serial stage whose service time is fixed by the request count looks like.
- **The ratio worsens with more jobs** (2.24x -> 2.85x) because the CPU half is
  the only half that scales. Judging the share by its ratio to a fast guest-local
  arm is therefore the *conservative* reading, not the flattering one.

Consequence for the kit's session: a 6-job build does not pay more for the share
per request, it just spends a larger fraction of its wall clock on it, and that
fraction grows with the artifact count, not with the job count.

## 2. The request count, and the price of a second

`CANG_FS_STATS=1` (already shipped; the histogram is emitted at `warn` at
milestones 5 k/20 k/100 k/300 k/1 M and at teardown, into the task's
`helper.stderr.log` - *not* the CLI's stderr, which is why a killed arm still
loses the teardown line but keeps the last milestone).

`JOBS=6`: **464 193 requests** served for a 56 s build writing 182 MB into
`target/` -> 8.3 k requests/s. `JOBS=26`: the 300 k milestone passed and the
build took 47 s, i.e. ~9.9 k requests/s at a similar total. Both sit at the
ceiling note 44 measured (7-8 k creates/s, ~28 k requests/s), so **the build
spends essentially its whole wall clock at the share's request ceiling**.

That gives a price list worth quoting to anyone tuning this workload:
**10 k requests = 1 s of cold build.**

| class | JOBS=6 count | share | seconds at 10 k/s |
|---|---|---|---|
| `LOOKUP` | 156 959 | 33.8 % | 15.7 |
| `WRITE` | 54 171 | 11.7 % | 5.4 |
| `GETXATTR` | 50 652 | 10.9 % | 5.1 |
| `READ` | 44 410 | 9.6 % | 4.4 |
| `GETATTR` | 43 046 | 9.3 % | 4.3 |
| `CREATE` | 28 268 | 6.1 % | 2.8 |
| `READDIR` | 17 894 | 3.9 % | 1.8 |
| `FORGET` | 16 254 | 3.5 % | 1.6 |
| `READDIRPLUS` | 14 319 | 3.1 % | 1.4 |
| `MKDIR` | 9 653 | 2.1 % | 1.0 |
| `UNLINK`/`RMDIR` | 16 223 | 3.5 % | 1.6 |
| `RENAME` | 4 822 | 1.0 % | 0.5 |
| `SETATTR`+`IOCTL`+`LINK` | 7 314 | 1.6 % | 0.7 |

- **The shipped levers are live in this arm**: `OPEN` 3, `RELEASE` 1, `OPENDIR`
  1, `FLUSH` 2 (`zero-message-open` + `no-flush`), and lookups report
  **156 959 positive, 0 negative, 0 error** - a miss is answered with the
  cacheable negative entry, so it never shows up as `-ENOENT` (`negative-entry`
  at its shipped 30 s). The `JOBS=26` milestone table is the same mix within
  2 points (`LOOKUP` 36.4 %, `WRITE` 11.6 %, `GETXATTR` 11.1 %, `READ` 9.9 %,
  `GETATTR` 7.6 %).
- **A cold build is not the iteration shape.** Note 45 measured a *no-op
  rebuild* at `LOOKUP` 79.4 %; a cold build is 34 %, because the iteration's
  share is cargo re-deciding 69 k freshness paths while a cold build also
  writes, creates and links everything. Levers priced on the iteration shape do
  not transfer to the cold one, and vice versa.
- **`GETXATTR` is the largest class no shipped lever touches** (10.9 % of a cold
  build), and note 40's "entirely `security.capability`, one request per process
  start" understates it here: 50 652 requests over ~7 200 process starts is ~7
  per exec. Worth one profile before pricing `xattr = false`, which would stop
  the guest honouring file capabilities.

## 3. The layer beneath: the round trip is not the backing store

Same op set as the repro kit's tier 1, run with no VM on this host (node,
500-op loops, `/home/dev/disp` = the btrfs state root, `/home` = ext4):

| op | guest (kit, virtio-fs) | host ext4 | host btrfs | tmpfs |
|---|---|---|---|---|
| create | 850 us | 24 us | 24 us | 10 us |
| rename | 3 100 us | 24 us | 27 us | 7 us |
| unlink | 1 700 us | 9.6 us | 24 us | 5 us |
| readdir(500) | 700 us | 15 us | 25 us | 10 us |
| fsync 4 KiB | 5 900 us | 3 990 us | 3 990 us | 0 |
| 64 MiB in 4 KiB writes | 4.2 MB/s | 838 MB/s | 632 MB/s | 1 194 MB/s |

Metadata is **97-99 % round trip**, so no backing-filesystem work can explain
it. `fsync` is the exception and the trap: 4.0 ms of its 5.9 ms is this class of
host disk, and rustc never fsyncs - a `fsync` number must not be read as a FUSE
number.

## 4. Correction to "the share costs ~1 %"

Note 35's headline (171.4 s -> 116.3 s "against a 115 s host-native build, i.e.
the share costs ~1 %") compares the share to the **host**, with the VM penalty
folded in, and it was taken across a window where the host itself moved 104 s ->
130 s. Measured interleaved against a guest-local control in one session, the
share costs **31.9-32.2 s on a 17-26 s floor: 2.24-2.85x the tmpfs arm**, not
1 %. The two readings agree only because a fast host and a slow one bracketed
each other. Quote the guest-local ratio, never the host ratio, for a share
claim.

## 5. What this leaves

Unchanged for host-side work, now with a denominator: the ceiling is per request,
every host-side knob is measured flat (pool 4->16, queues 1->4, notification
batching, background credit, exits ~1 %, MSI-X), and a cold build's 464 k
requests are what a second of build is made of. A lever is now priced as
"requests removed / 10 000 per second of cold build", which puts `GETXATTR`
(5.1 s), `GETATTR`+`READDIR`+`READDIRPLUS` (7.5 s) and `FORGET` (1.6 s) on the
table as *classes* rather than as a vague "remove round trips", while `WRITE`
(5.4 s) and `CREATE` (2.8 s) stay where ticket 03 left them: removable only by a
write cache or by moving `target/` off the share.

## The diagnostic handed to the reporting session

`tools/virtiofs-diag/virtiofs-diag.sh` is the self-contained version of the
measurement above: run it inside the guest (`sh /workspace/tools/virtiofs-diag/virtiofs-diag.sh all`)
with `CANG_FS_STATS=1` on the host launch, and harvest `helper.stderr.log` while
it runs. `probe` prints per-op microseconds for a batch of builtins plus two
controls, `iterate` the no-op rebuild, `tree` the file/byte accounting, `cold` the
share-vs-tmpfs pair into throwaway target dirs (never the project's own).
Validated live in a guest on the reference tree, 2026-11-05: share cost 27.1 s /
31.3 s over two rounds (tmpfs 17.8 s, 2.52-2.78x), independently reproducing
§1's 31.9 s; per-op on the share - create 432 us, 4 KiB write+close 702 us, read
441 us, stat 143 us, readdir(300) 237 us, exec 3 948 us - against tmpfs 29/37/69/
20/210 us, so a create is 15x tmpfs and a process start is a share cost on its
own. `target/` had not been built when `tree` ran, hence `all` now runs `tree`
after `iterate`.

## Reproducing

```bash
export CANG=<result-0.17.3>/bin/cang CANG_FS_STATS=1 IMAGE=localhost/cang:latest
export BENCH_ROOT=/home/dev/cang-bench RUNS=1 WORKLOAD=synthetic
JOBS=6  tools/rust-compile-bench/run.sh --arms guest-virtiofs,guest-local --runs 1
JOBS=26 tools/rust-compile-bench/run.sh --arms guest-virtiofs,guest-local --runs 1
```

The histogram needs one extra step that the harness does not do: while the arm
runs, copy `$state_root/cang/<workspace>/tasks/*/helper.stderr.log` every second
(the file is deleted with the task, and the CLI's stderr carries none of it).
`RUNS=1` per arm here: the 32 s delta is 55-65 % of an arm, far outside the ~5 %
spread the map requires for a *shipped* change, but a headline number should be
re-recorded with two interleaved rounds per side.
