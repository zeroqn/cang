# main-ship verification — virtio-fs no-flush + 250 ms negative entry (defaults)

**Verdict: PASS.** The binary under test (`/nix/store/l1b7jdq856qwc4mccsik9rx6l8rz8ws2-cang-0.15.1`,
`cang 0.15.1`) ships both switches ON by default and behaves correctly on every path checked:
the new `--fs` keys parse, both bogus values are refused, the guest runs on the default path and
on both explicit overrides, the filesystem payload is byte-exact from the host's view, a
host-created name becomes visible in the guest within one ~0.22 s probe (<= 250 ms + one probe),
and the bench arm shows `FLUSH 2` (was ~211 k at 1 M requests) with `LOOKUP 40.39 %` of the mix
under 2401 crates compiled. Guest build 138.372 s vs host anchor 114.203 s (+21.2 %).

## 0. Artifact under test

    $ readlink -f /home/dev/cang/cang/result-ship/bin/cang
    /nix/store/l1b7jdq856qwc4mccsik9rx6l8rz8ws2-cang-0.15.1/bin/cang
    $ cang --version
    cang 0.15.1

Repo was not modified; no rebuild was performed.

## 1. CLI surface (new code) — PASS

`cang --help` (evidence `cli-help.txt`) documents all three keys:

    98:      --fs <KEY:VALUE>
    101:            zero-message-open:on|off
    104:            no-flush:on|off
    107:            negative-entry:on|off|<milliseconds>

Three launches of a trivial payload in `/home/dev/cang-bench/evidence/main-ship/ws-cli`, each
backgrounded under `timeout 300` (`cli-surface.sh`), each guest writing its own marker file back
into the share:

| arm | extra args | cang rc | guest evidence (`/workspace/marker-<tag>.txt`) |
|---|---|---|---|
| default | *(none)* | 0 | `GUEST_RAN tag=default uname=7.2.10-hardened1` + `/workspace` listing |
| off | `--fs=no-flush:off --fs=negative-entry:off` | 0 | `GUEST_RAN tag=off uname=7.2.10-hardened1` |
| neg1000 | `--fs=negative-entry:1000` | 0 | `GUEST_RAN tag=neg1000 uname=7.2.10-hardened1` |

All three `cli-*.log` files are 0 bytes (no warnings on the launch path). Guest kernel
`7.2.10-hardened1` in all three.

Refusals (both exit **2**, both naming the option):

    $ cang --fs=no-flush:maybe -- true
    error: invalid value 'no-flush:maybe' for '--fs <KEY:VALUE>': no-flush:maybe: expected on or off

    $ cang --fs=negative-entry:soon -- true
    error: invalid value 'negative-entry:soon' for '--fs <KEY:VALUE>': negative-entry:soon: expected on, off, or a lifetime in milliseconds

## 2. Smoke / correctness on the default path — PASS

Launch: default path, only `CANG_FS_STATS=1` set, payload `/workspace/smoke.sh`
(`run-smoke.sh`, `ws-smoke/smoke-out.txt`):

    CREATE_READBACK=hello
    APPENDED
    WC=14
    STAT1_MISSING_RC=1
    STAT2_MISSING_RC=1
    DIRLIST=inner.txt
    SYMLINK_READ=inner
    SYMLINK_TARGET=sdir/inner.txt
    RM_RC=0 LEFTOVER=smoke-out.txt smoke.sh
    --- smoke end 2026-10-27T06:23:38+00:00 SMOKE_OK
    GUEST_EXIT=0

The first payload deleted its own files, so a second short launch (`persist.sh`) created, appended
and *kept* the file; verified on the HOST:

    $ od -c ws-smoke/keep-s1
    0000000   h   e   l   l   o  \n   A   P   P   E   N   D   E   D
    0000016
    $ wc -c ws-smoke/keep-s1
    14 ws-smoke/keep-s1
    $ od -c ws-smoke/keep-status.txt
    0000000   S   M   O   K   E   _   O   K  \n
    0000011

(`\n` is a real newline; rendered as `APPENDED` above.) `stat` of a missing name twice returned
1 both times (the 250 ms negative entry hides neither error nor presence). `env | grep -c '^CANG_FS'`
in the guest was 0 — `CANG_FS_STATS` is host-side only, so the guest was not steered by env.

## 3. Coherence at the shipped default — PASS (217 ms, one probe)

Guest (`ws-coh/coh.sh`) statted `/workspace/late-file` every ~0.2 s for 20 s printing
`LT_MS=<ms> SEEN|ENOENT`; host slept 10 s after launch then `touch`ed it:

* samples: **100** (29 `ENOENT`, 71 `SEEN`), guest span 24 698 ms
* probe interval: min 211 ms, median 217 ms (one 1814 ms outlier at the boot boundary)
* last `ENOENT`  `LT_MS=1793082293489`; first `SEEN` `LT_MS=1793082293706`
  -> **guest-side delay 217 ms = exactly one probe interval**
* cross-clock (guest epoch ms vs host epoch ms, timesync active): host touch
  `HOST_TOUCH_MS=1793082293634`, guest first `SEEN` **72 ms** after it
* 29 ENOENT samples before the switch, i.e. the miss stayed cached and then expired inside
  250 ms as designed. Both estimates are under 250 ms + one probe interval.

## 4. Bench arm + host anchor (default launch path, `CANG_FS_STATS=1` only)

    tools/rust-compile-bench/run.sh --arms host,guest-virtiofs --runs 1 --workload synthetic

| arm | build (BENCH_MS) | outer | fs_worker | crates |
|---|---|---|---|---|
| host (anchor) | **114.203 s** | 128.535 s | - | 2401 |
| guest-virtiofs | **138.372 s** | 146.312 s | 15.4 % | 2401 |

guest/host = **1.212 (+21.2 %)**. Both rows appended to `/home/dev/cang-bench/summary-synthetic.tsv`
as its last two lines; rustc/cargo 1.95.0 in both. The `host` row above mine (118 623 ms, earlier
campaign) is not part of this run. Per project guidance this is one arm, not a noise-bounded
campaign: the +21.2 % is a paired same-window observation, and the absolute 138.372 s should not
be compared to a different campaign's 171.4 s without such a control.

### Verbatim histogram, highest milestone (300 001 requests)

File: `/home/dev/cang-bench/evidence/main-ship/helper/guest-virtiofs-1368256-1793082526753153342.log`
source mtime `2026-10-27 14:31:10.527790142 +0800`, size 14 569 B (captured by the 1 s watcher
before cang's teardown deleted the task dir):

    [2026-10-27T06:30:07.867537Z WARN  krun_devices::virtio::fs::server] fs: opcode histogram [milestone]: 300001 requests served
    [2026-10-27T06:30:07.867578Z WARN  krun_devices::virtio::fs::server] fs:   LOOKUP                     121169   40.39%
    [2026-10-27T06:30:07.867592Z WARN  krun_devices::virtio::fs::server] fs:   WRITE                       30564   10.19%
    [2026-10-27T06:30:07.867601Z WARN  krun_devices::virtio::fs::server] fs:   GETXATTR                    29265    9.75%
    [2026-10-27T06:30:07.867613Z WARN  krun_devices::virtio::fs::server] fs:   READ                        25575    8.52%
    [2026-10-27T06:30:07.867624Z WARN  krun_devices::virtio::fs::server] fs:   GETATTR                     20522    6.84%
    [2026-10-27T06:30:07.867811Z WARN  krun_devices::virtio::fs::server] fs:   FLUSH                           2    0.00%
    [2026-10-27T06:30:07.867885Z WARN  krun_devices::virtio::fs::server] fs:   lookup 121171 positive, 0 negative, 0 error (0.00% of lookups found nothing; 40.39% of requests)

Earlier milestones from the same file (5000 / 20000 / 100000) show the same two fingerprints:
`FLUSH 2` flat at every milestone, and `lookup ... 0 negative` at every milestone.

**Reading the numbers**
* `FLUSH 2` — not ~211 k. The `-ENOSYS` reply made the guest stop sending FLUSH entirely; the 2
  are the early requests before the client learned. Shipped no-flush default confirmed.
* `LOOKUP 40.39 %` — down from the ~54 % share before the change, and `0 negative` at the server is
  the by-construction fingerprint of the 250 ms negative entry (a cached miss never reaches the
  server). 40.39 % sits at the top of the 30-40 % band, not below it; the synthetic workload's
  LOOKUP share also rises early (80 % at 20 k) before rustc's write phase dilutes it.
* Both arms compiled all 2401 crates.

## 5. What went wrong / caveats

1. The first smoke run deleted its own files, so the host byte check needed a second short launch
   (`persist.sh`) that keeps the file. Both launches are recorded.
2. One coherence probe interval was 1814 ms (boot/scheduler hiccup); it sits before the touch and
   does not affect the last-ENOENT -> first-SEEN measurement.
3. The 1 s helper-log watcher globbed `.../cang/guest-virtiofs/tasks/*/helper.stderr.log` only, so
   it captured the bench arm (the only arm whose histogram was needed) but not the throwaway
   `ws-cli`/`ws-smoke`/`ws-coh` workspaces. No histogram was required from those.
4. Co-tenant state on the host: stale `tail -F` processes on old task dirs and an idle
   `mon-threads.sh` from another session; no live co-tenant VM during the bench arm (verified from
   `ps` before launch), so the arm was not CPU-contended. No host-global kills, no podman store
   commands, no git commands were issued.
5. Disk: `/home` at 91 % (11 G free) throughout; no workspace state was deleted.
