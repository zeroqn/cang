---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# A second launch on the same host saves 15-20 % of the walk, and `/nix` in these boots is
# virtiofs, not a guest-composed overlay

## 1. The host page cache is worth ~98 ms, and the guest still does every read

One boot, block lower, `--nix=upper:blk`; `c*` arms drop the guest page cache **and** the
image's host pages (mincore-proved), `w*` arms drop only the guest's cache - the "second
launch on the same host" case, since a guest's page cache cannot survive a reboot.

| arm | host image cache | walk | guest device reads |
|---|---|---|---|
| `c1` | dropped | 500 ms | 941 |
| `c2` | dropped | 474 ms | 941 |
| `c3` | dropped | 450 ms | 941 |
| `w1` | **warm** | **380 ms** | 941 |
| `w2` | **warm** | **375 ms** | 941 |

So a warm host costs **375-380 ms against 450-500 ms: ~98 ms, 15-20 %**. The guest still
issues all 941 requests; only the host's service per request is cheaper, which is why the
saving tracks the ~100 us of host service per read measured in note 21 rather than the whole
walk. That is the "cache lifetime" lever, in the only form available: it is per-host, and it
does not remove the guest's round trips.

## 2. A control defect made the first attempt at this invalid

`control11.sh` dropped the image for **every** arm - its `w*` skip did not exist (it lives in
`control9`/`control10`) - so the first `--nix=upper:blk` boot (`run-upper-20261115T231728Z`)
reported 474/443/467 ms cold against 486/452 "warm", i.e. no effect, because every arm was
cold. The proof log is where this is visible: `@@ w1 warm_no_drop` is the line that must
appear, and it was absent. Fixed (`control11.sh` now carries the skip), re-run, and the table
above is from the fixed boot. **Rule: a warm arm's absence of a drop must be asserted in the
proof, not assumed from the label.**

## 3. Correction: in this harness `/nix` is a virtiofs path, not the guest's overlay

`run-nixwhat-20261115T233220Z` says it plainly:

```
-- /        fstype=fuse  dev=24
-- /nix     fstype=fuse  dev=24      <- same filesystem as /, no /nix mount point
-- /run/cang/nix-lower  fstype=btrfs dev=39
no /nix mount point
```

So the block lower is attached and mounted, but `/nix` is the host-composed share inside the
guest's fuse root - the guest never overlaid it, and `CANG_NIX_LOWER_UPPERDIR` pointed at a
directory inside that share rather than at a guest-disk upper. `--nix=upper:blk` was
therefore **not exercised** by any boot here, and the 800-1 100 ms `/nix/store` walks I
measured in `run-upper2-*` are the virtiofs path (consistent with note 56's 864-2 033 ms),
not an overlay over the block lower.

What that leaves untested, and it is a harness question rather than a cang one: whether
`/nix` composed over the block lower behaves differently from `/nix` on the share, and what a
guest-disk upper costs. Both need a boot whose `/nix` really is an overlay mount - check
`findmnt /nix` first and refuse the arms otherwise.

## Evidence

`run-upper-20261115T233711Z` (the valid warm/cold pairs, `results.txt` + `proof.log`),
`run-upper-20261115T231728Z` (the invalid first attempt), `run-upper2-20261115T232245Z` /
`T232645Z` (the `/nix/store` walks that turned out to be virtiofs),
`run-nixwhat-20261115T233220Z` (`diag.txt`: mount table, `stat -f` per path, env).
Probe `probe-upper.sh`/`probe-upper2.sh`/`probe-nixwhat.sh`, runner `run-upper*.sh`, control
`control11.sh` (fixed here).
