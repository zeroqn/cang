---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Rebuilding the corrupt build disk fixed it: `/nix` now composes over the block lower and
# walks 2.6-2.9x faster (1 239 ms -> 422-478 ms)

Deleted the EUCLEAN `cang-build.raw` (540 KB of content, no cargo cache, no upper generation -
see note 25), let cang recreate it, and re-ran the same cold walk. The guest now composes the
overlay itself:

```
findmnt /nix                 -> /nix overlay overlay
mountinfo: 48 26 0:46 / /nix rw,relatime shared:20 - overlay overlay
  rw,lowerdir=/run/cang/nix-lower,
     upperdir=/home/dev/.cache/cang-build/nix-overlay/sha256-99e47…/upper,
     workdir=/home/dev/.cache/cang-build/nix-overlay/sha256-99e47…/work,
     redirect_dir=nofollow,uuid=on,userxattr
stat -f /nix                 -> overlayfs        (was: fuse)
stat -f /home/dev/.cache/cang-build -> btrfs     (was: fuse, EUCLEAN)
```

Same 3 897-file cold walk, guest page cache dropped every arm and the host image dropped
(mincore-proved) before each:

| root | this boot | previous boot (`/nix` on virtiofs) |
|---|---|---|
| `/nix/store` | **478 / 422 / 431 ms** | 1 239 ms |
| `/run/cang/nix-lower/store` (raw) | 404 / 461 ms | 526-559 ms |

So `/nix` is now indistinguishable from the raw lower - **2.6-2.9x faster than the virtiofs
path** - and the overlay still costs nothing (matching note 25's 442-472 ms with a tmpfs
upper). None of this needed a cang change: the blocker was a corrupt persistent build disk,
which cang validates only by `blkid` TYPE, so a broken-but-btrfs superblock passes and the
guest fails at mount.

## What this settles and what it does not

- Settles: `--nix=lower:blk` + `--nix=upper:blk` really do compose a guest overlay over the
  block lower, at block speed, with the writable layer on the guest's own disk.
- Settles: notes 24-25's `/nix` numbers were a *broken disk* artefact, not cang behaviour.
  Everything measured before that on `/nix` (the 800-1 239 ms walks) should be read that way.
- Does not change the metadata picture: the walk is still 941 requests and ~420-480 ms, and
  every transport-side lever measured today (compression, erofs, direct-to-guest, vhost-user,
  DAX, noatime, locality) leaves it there.
- Worth a follow-up in cang, not in this map: `raw_btrfs` validates the image with `blkid`
  only. A cheap `btrfs check`-free detector (or recreating on a mount failure) would have
  turned a silent 3x regression into a repair.

## Evidence

`run-nixovl-20261116T125327Z`: `setup.txt` (the mountinfo line, `stat -f` per path, the fresh
`CANG_BUILD` UUID), `results.txt` (per-arm times and device counters), `progress.txt`. The
before numbers: `run-ovl2-20261116T124416Z` (`/nix` 1 239 ms on virtiofs) and
`run-ovl-20261116T123851Z` (overlay + tmpfs upper 442/472 ms). Probe `probe-nixovl.sh`, runner
`run-nixovl.sh`, control `control11.sh`.
