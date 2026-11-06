# Note 54: over-commit, and why the release path cannot rescue it

2026-11-06. Follows note 53. Guest 18-22 GiB on a 20.6-26.7 GiB host (the host's
own `MemTotal` moved from 26.68 to 20.65 GiB mid-session with no reboot, so the
same `--mem` crossed the guard's lines between runs - measured per run, never
assumed).

## The pathology, reproduced

`--mem 22` (82-93% of host) with three concurrent builds in three worktrees at
`JOBS=4`, guest RAM fully resident:

| host during the build | value |
| --- | --- |
| swap-in | 45-145 MB/s sustained |
| swap-out | 47-262 MB/s |
| available | 3.1-4.0 GiB of 20.6 GiB |
| iowait | 27-35% |
| outcome | builds did not finish inside a 60-minute window |

That is the reported symptom: `kswapd0` on the host, everything slow, the guest's
own compiles crawl. It is not a virtiofs problem - the share is merely the first
thing to notice.

## What the guard does (shipped, verified live)

An explicit `--mem` is now classified against host `MemTotal`:

- `<= 80%` - the budget cang's own default already uses: accepted silently;
- `80% < x <= 90%` - accepted with a warning naming the percentage;
- `> 90%` - refused, naming the largest `--mem` this host can back.

Measured: `--mem 22` on the 20.65 GiB configuration is refused ("93% of this
host's memory ... use at most 21 GiB"); `--mem 18` (82%) boots with the warning;
`--mem 16` (77%) boots silently. Unit tests pin all four bands, including the
"host memory unknown" case, which must never refuse.

The guard would have prevented the reproduced run - the launch that thrashed for
an hour is the launch it now refuses.

## What the release path cannot do (falsified, reverted)

Extending the existing pressure release to escalate - when dropping the clean
page cache frees less than RAM/16, have the guest inflate itself (allocate and
touch RAM/4, then free it) so its own kernel evicts anonymous memory into zram -
was implemented, unit-tested, and then measured. Two findings killed it:

1. **The guest cannot answer in time.** Under the pressure the release exists
   for, touching 4 GiB in an 18 GiB guest whose RAM is held took **116.8 s**;
   the host's release exchange timed out at 5 s (before the change) and still at
   60 s (with the timeout raised). Every attempt logged `EAGAIN` on the reply
   while the host stayed in swap.
2. **The work itself is the pathology.** Those 116.8 s were spent thrashing: the
   guest's `MemFree` fell to 112 MiB, 1 GiB of anonymous memory went to zram
   (`pswpout` +264k pages), and the host's own swap-out peaked at 220 MB/s during
   it. A rescue that costs two minutes of the failure mode it is rescuing from is
   not a rescue.

Both halves are reverted; the release keeps doing what it did (drop clean page
cache, which is a synchronous syscall and answers in milliseconds).

## What would work

The balloon's inflate queue, in the fork. Three properties the guest-side
escalation lacks:

- **the guest's kernel chooses** what to evict, in LRU order, instead of a
  userspace allocation racing it;
- **no guest userspace work at all** - the request is a queue kick, so there is
  no latency cliff and no timeout to tune;
- **the host knows exactly what it got** (`actual` in the balloon config), so it
  can act while it still has room rather than when it is already on fire.

That is a fork change (inflate queue processing, config-change interrupt, a
runtime target reachable after `attach`), i.e. bob's option 2, and it is the
answer for the future rather than a day's work.
