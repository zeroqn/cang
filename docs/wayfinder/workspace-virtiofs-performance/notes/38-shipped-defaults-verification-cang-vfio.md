# Ship verification - the two virtio-fs defaults on the default launch path

Date: 2026-10-27 (host local, UTC+8) - all times below are from the evidence files, not typed in.
Binary: `CANG=/home/dev/cang/cang-vfio/result-ship/bin/cang`

## Verdict

**YES.** The shipped defaults are live end to end from a plain `cang` launch with no
`CANG_FS_*` variable set (only the diagnostics switch `CANG_FS_STATS=1`), and the guest
builds the full workload against the host's files.

Three numbers that prove it (guest arm, 2401-crate synthetic build, default launch):

| # | what | measured | what the un-shipped behaviour was |
|---|------|----------|------------------------------------|
| 1 | `FLUSH` served at the highest milestone (300 000 requests) | **2** (and 2 already at 5 000, i.e. boot only, zero growth) | 211 877 at 1 M requests |
| 2 | `lookup` split + share | **122 188 positive, 0 negative, 0 error = 40.73 % of requests**, and the arm never reached the 1 M milestone at all (pre-ship: 1.71 M requests) | 91.5 % of lookups were `-ENOENT` (not cacheable), 54 % of requests |
| 3 | host-created name visible to the guest | **93 ms** after the host `touch` (bounded by one ~220 ms probe interval), 9 ENOENT probes before it | `-ENOENT` never caches, so the next probe always saw it (0 ms), the price now paid for #2 |

Secondary numbers: guest build 146.5 s vs the same-window host anchor 118.6 s (1.24x),
FS worker 17.2 % of a core, smoke payload all-OK, guest write host-verified with `od -c`.

Why `0 negative` is the signature of the shipped default, not a missing one: the
passthrough server answers a miss with the protocol's negative entry
(`linux/passthrough.rs:1230` -> `negative_entry()`, `inode: 0` + `entry_valid`), and
`Server::lookup` (`server.rs:336-350`) counts every `Ok(entry)` as *positive* and only an
`Err` as negative/error. So under the switch a cached miss is a successful reply and the
negative counter is 0 by construction; a `-ENOENT` server would show it in the hundreds of
thousands on this workload.

## Everything under test, proven to be the pushed flake input

    $ readlink -f /home/dev/cang/cang-vfio/result-ship/bin/cang
    /nix/store/6x9l3d9dmyp4522wl9cfx0xl94fgy0k2-cang-0.14.0/bin/cang
    $ .../bin/cang --version
    cang 0.14.0

Provenance (read from files, no git command): `flake.lock` `libkrun-src` = github:zeroqn/libkrun
rev `e8a771592ec78e5041f8d3abe44a37572a73a977` (lastModified 1793080191), and
`deps/libkrun` in the worktree is on branch `cang-no-flush` at exactly that SHA
(`.git/worktrees/cang-vfio/modules/deps/libkrun/refs/heads/cang-no-flush`), so the store
path's flake input and the in-tree fork checkout are the same revision. `CANG_FS_STATS` is
`nix/store`-built: nothing here used `--override-input` (no `path:` build, `result-ship` is a
plain flake output symlink).

No levers were set by hand: `env | grep CANG` in the session has no `CANG_FS_*` entry, the
smoke/coherence launches explicitly `env -u CANG_FS_NO_FLUSH -u CANG_FS_NEGATIVE_ENTRY_TIMEOUT`,
and `tools/rust-compile-bench/run.sh` contains no `CANG_FS` string at all (grep: no match).

## 1. Smoke - default launch, `CANG_FS_STATS=1` only

Workspace `/home/dev/cang-bench/ship-smoke`, log
`/home/dev/cang-bench/evidence/ship/smoke.log` (mtime 13:57:46, 158 bytes).
Launched as:

    cd /home/dev/cang-bench/ship-smoke && \
    env -u CANG_FS_NO_FLUSH -u CANG_FS_NEGATIVE_ENTRY_TIMEOUT CANG_FS_STATS=1 \
      timeout 600 $CANG --image ghcr.io/zeroqn/cang:latest -- sh /workspace/smoke.sh

Guest output, verbatim (CRLF as the guest emitted it):

    SMOKE_START 1793080646848
    11 f.txt
    hello
    more
    STAT_ENOENT_OK
    STAT_ENOENT_AGAIN
    x
    f.txt
    hello
    more
    inner
    6 bulk/f1499.txt
    SMOKE_END 1793080666415

(create -> write -> append -> read back -> `wc -c` = 11; two stats of a missing name both
`ENOENT`; dir + file inside + read; symlink + `readlink` + read through it; removed again;
then 1500 create/write/read/stat-missing iterations to clear the server's 5 k milestone.)
Run 19.6 s wall (1793080646848 -> 1793080666415 on the guest's own clock).

Host-side verification of the guest's writes:

    $ od -c /home/dev/cang-bench/ship-smoke/f.txt
    0000000   h   e   l   l   o  \n   m   o   r   e  \n
    0000013
    $ od -c /home/dev/cang-bench/ship-smoke/smoke-result.txt
    0000000   S   M   O   K   E   _   O   K  \n
    0000011

So the guest's bytes, its appends, its `rm` and its result file all landed on the host.

### Smoke VMM histogram (second, identical run in `ship-smoke2`, so the VMM log could be captured)

The first smoke's `helper.stderr.log` was lost: the brief's watcher globs only
`/home/dev/disp/cang/guest-virtiofs/tasks/*`, and a smoke workspace is `ship-smoke`. Re-ran
the same payload (loop 3000) in `ship-smoke2` with a second watcher on that workspace:
`/home/dev/cang-bench/evidence/ship/smoke2.log` (mtime 13:59:31, guest output identical plus
`6 bulk/f2999.txt`), VMM log
`/home/dev/cang-bench/evidence/ship/helper2/ship-smoke2-1259053-1793080724271634577.log`
(10 894 bytes, mtime 2026-10-27 13:59:31.571476382 +0800). Verbatim, highest milestone (100 000):

    [2026-10-27T05:59:27.878914Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 100000 requests served
    [2026-10-27T05:59:27.878954Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                      11843   11.84%
    [2026-10-27T05:59:27.878967Z WARN  krun_devices::virtio::fs::server] fs:   READ                         4079    4.08%
    [2026-10-27T05:59:27.878974Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                      3014    3.01%
    [2026-10-27T05:59:27.878993Z WARN  krun_devices::virtio::fs::server] fs:   WRITE                        2813    2.81%
    [2026-10-27T05:59:27.878961Z WARN  krun_devices::virtio::fs::server] fs:   GETXATTR                     8393    8.39%
    [2026-10-27T05:59:27.879105Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                           2    0.00%
    [2026-10-27T05:59:27.879164Z WARN  krun_devices::virtio::fs::server] fs:   lookup 11843 positive, 0 negative, 0 error (0.00% of lookups found nothing; 11.84% of requests)

`FLUSH 2` from 5 000 through 100 000 (boot only) and not one negative lookup, on a payload
that stats a missing name 3 000 times: both defaults are already visible at smoke scale.
(The 66.8 % READLINK in that block is the payload's own process spawns, not the build's mix.)

## 2. Coherence - how long a host-created name stays invisible (shipped 250 ms)

Payload: `stat /workspace/late-file` every ~0.2 s for 20 s, printing `LT_MS=<ms> SEEN|ENOENT`;
a host watcher sleeps 5 s, `touch`es the file and logs `HOST_TOUCH_MS`. Two runs, defaults,
`CANG_FS_STATS=1` only.

Run 2 (`/home/dev/cang-bench/evidence/ship/coherence2/timeline.txt`, mtime 14:01:56;
`host-touch.log`, mtime 14:01:38):

    HOST_TOUCH_MS=1793080898342 (pre=1793080898329)
    HOST_FIRST_SEEN_LINE_MS=1793080898783
    HOST_DELAY_MS=441

    START_MS=1793080895864
    ...
    LT_MS=2351 EPOCH_MS=1793080898215 ENOENT
    LT_MS=2571 EPOCH_MS=1793080898435 SEEN     <- first SEEN

* host touch 2 478 ms into the run; **first SEEN 93 ms later** (guest clock adjusted); the
  previous probe, 127 ms before the touch, still said ENOENT.
* **9 ENOENT probes** preceded it (220 ms from the last ENOENT to the first SEEN, i.e. the
  file was picked up at the very next probe).

Run 1 (`.../coherence/timeline.txt`, mtime 14:00:34): touch 2 673 ms in, last ENOENT at
2 411 ms, first SEEN at 2 630 ms -> **219 ms after the last miss, 10 ENOENT probes before it**,
and the SEEN stamp is 43 ms *before* the host's touch stamp, i.e. inside the run's clock skew
(guest and host clocks differ by tens of ms; the payload stamps `EPOCH_MS` before the `stat`,
so treat single-digit tens of ms as noise).

How to read the numbers: the guest sees a host-created name at its **next probe**, which is
bounded by (negative-entry lifetime 250 ms) + (probe interval ~220 ms) = ~470 ms worst case.
Observed detection: **93 ms** (run 2) and <=219 ms (run 1). ENOENT samples before it: 9-10.
The previous campaign measured 74 ms on the same setting; this reproduces that.

The `HOST_DELAY_MS=441` figure is a *different* quantity and an upper bound: it is the host
noticing the guest-written `SEEN` line in the shared file (guest `stat` -> guest write ->
virtiofs -> host poll at 20 ms), so it includes the guest's write becoming visible on the
host. The negative-cache staleness itself is the guest-side figure above.

## 3. One bench arm on the default launch path + one host anchor

Commands (each backgrounded with an outer `timeout`, one VM at a time):

    cd /home/dev/cang/cang-vfio
    GUEST_TIMEOUT=2400 CANG=/home/dev/cang/cang-vfio/result-ship/bin/cang CANG_FS_STATS=1 \
      timeout 2400 tools/rust-compile-bench/run.sh --arms guest-virtiofs --runs 1
    CANG=/home/dev/cang/cang-vfio/result-ship/bin/cang \
      timeout 2400 tools/rust-compile-bench/run.sh --arms host --runs 1

| arm | build | outer | fs_worker | result file |
|-----|-------|-------|-----------|-------------|
| guest-virtiofs (default launch) | **146.5 s** | 154.6 s | 17.2 % of a core | `/home/dev/cang-bench/guest-virtiofs/bench-result-guest-virtiofs-1-1793080964.txt` |
| host (anchor) | **118.6 s** | 133.3 s | - | `/home/dev/cang-bench/host/bench-result-host-1-1793081201.txt` |

guest/host = **1.24x** (+27.9 s) in one window. Absolute seconds are only comparable inside
this window (the rig's host anchor has read 108-122 s across campaigns depending on host page
cache), so the honest claim is the paired ratio, not a cross-campaign delta; the lever
effects themselves were established by same-window A/B in the previous campaign (note 35).

The arm really compiled the whole workload: `bench-run-guest-virtiofs-1-1793080964.log`
(mtime 14:05:14, 144 277 bytes) has **2401 `Compiling ` lines** and
`Finished `dev` profile [unoptimized] target(s) in 2m 22s` / `inner: done`; the result file
reads `BENCH_MS=146507`.

### Histogram, verbatim, highest milestone the arm reached

Source: `/home/dev/cang-bench/evidence/ship/helper/guest-virtiofs-1269775-1793080964447694348.log`
(14 569 bytes, mtime 2026-10-27 14:05:16.866949460 +0800; the arm ended 14:05:19, and cang
deletes the task dir at teardown, so there is no `[teardown]` block - this is the last
milestone the live log carries).

    [2026-10-27T06:04:14.330715Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 300000 requests served
    [2026-10-27T06:04:14.330749Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                     122188   40.73%
    [2026-10-27T06:04:14.330761Z WARN  krun_devices::virtio::fs::server] fs:   WRITE                       30344   10.11%
    [2026-10-27T06:04:14.330769Z WARN  krun_devices::virtio::fs::server] fs:   GETXATTR                    29067    9.69%
    [2026-10-27T06:04:14.330777Z WARN  krun_devices::virtio::fs::server] fs:   READ                        25357    8.45%
    [2026-10-27T06:04:14.330785Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     20606    6.87%
    [2026-10-27T06:04:14.330960Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                           2    0.00%
    [2026-10-27T06:04:14.331028Z WARN  krun_devices::virtio::fs::server] fs:   lookup 122188 positive, 0 negative, 0 error (0.00% of lookups found nothing; 40.73% of requests)

Same log, the earlier milestones, for the shape of the mix (LOOKUP share falls as the build
proceeds): 5 000 -> 49.50 % of requests, 20 000 -> 80.06 % (waiting on the workload unpack),
100 000 -> 65.36 %, 300 000 -> 40.73 %. `FLUSH` is 2 in every one of them.

Readings:

* **FLUSH 2**, unchanged from 5 000 to 300 000 requests: the share answered `-ENOSYS` on the
  first attempt, the kernel set `no_flush`, and the build stopped sending FLUSH entirely.
  Expectation was 0-2 (211 877 at 1 M before); met exactly.
* **LOOKUP 40.73 %** at the highest milestone, against 54 % with `-ENOENT`. It is above the
  30-39 % band quoted for the 1 M milestone, and for a measurable reason: LOOKUP's share is
  front-loaded (80 % at 20 k) and the arms do not reach the same milestone - this arm never
  reached 1 M requests at all, because the two levers removed ~48 % of them (pre-ship the same
  build issued 1.71 M, with both defaults 0.89 M). Comparing my 300 k block to a 1 M block
  would compare two different moments of the build, so the milestone-matched facts are the ones
  above plus `0 negative`.
* `0 negative, 0 error`: every miss is now the cacheable negative entry, counted as a positive
  reply (see the Verdict note), and the guest stops re-asking. Pre-ship this line read
  `~91.5 % negative`.

## What went wrong (and what it cost)

1. **The first smoke's VMM log was lost** - the prescribed watcher only covers the
   `guest-virtiofs` workspace, so the 22 s smoke's task dir (and its `helper.stderr.log`) was
   gone at teardown before anything copied it. Fixed by re-running the identical payload in
   `ship-smoke2` with a second watcher aimed at that workspace (1 s/0.5 s poll). Both smoke
   runs are reported; the functional checks come from the first, the histogram from the second.
2. **`HOST_DELAY_MS=441`** is not the staleness; it is the host seeing the guest's write
   (see section 2). Reporting it raw would overstate the visibility hole 4-5x.
3. **Clock skew between guest and host is tens of ms** (`EPOCH` stamps are taken before the
   probe), which is why run 1's SEEN stamp lands 43 ms "before" the host touch. Both runs
   agree on the physical fact: picked up at the next probe, <=220 ms.
4. A benign `ERROR krun_devices::virtio::vsock::muxer: unexpected dgram pkt: 3` appears at
   guest shutdown in both guest logs (also present in pre-ship campaigns); it does not affect
   the arm, which had already written its result.
5. `/home` is 91 % full (11 GB free) and unchanged by this work: no arm copies live outside
   `/home/dev/cang-bench`, and no state-root directory was touched.

## Evidence index

    /home/dev/cang-bench/evidence/ship/
      smoke.log                     smoke run 1 guest output (13:57:46)
      smoke2.log                    smoke run 2 guest output (13:59:31)
      helper2/*.log                 smoke run 2 VMM log (13:59:31.571)
      coherence/{timeline.txt,host-touch.log,run.log}       run 1 (14:00:34)
      coherence2/{timeline.txt,host-touch.log,run.log}      run 2 (14:01:56)
      helper/*.log                  bench arm VMM log (14:05:16.866)
      bench-guest.stdout.log        harness stdout, guest arm
      bench-host.stdout.log         harness stdout, host anchor
      prev-logs/                    the harness logs this run overwrote
