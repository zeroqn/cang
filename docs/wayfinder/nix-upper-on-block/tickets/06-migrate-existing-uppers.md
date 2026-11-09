---
label: wayfinder:grilling
title: How an existing host-directory upper becomes guest-side state
status: closed
blocked_by: ["04-upper-home-and-auto-grow", "05-seeding-parity"]
claimed_by: "pi (2026-11-09)"
---

## Question

The cached objects are worth keeping (bob, 2026-11-10: migrate once). But the
destination is a raw image the host cannot mount rootless, so the old
`$STATE/cang/<slug>/nix-overlay/upper` tree (on this host: `harmony` 3.0 GB /
2501 store paths) has to reach the guest's new upper filesystem somehow. Note
that `mkfs.btrfs --rootdir=<upper>` - the recipe that builds the read-only lower
- is *not* available here: it creates a new filesystem image, and the
destination is a filesystem that already exists inside `cang-build.raw`.

Candidate shapes:

- **First-launch copy (no extra VM).** The first `upper:blk` launch binds the old
  upper read-only into the guest (the share already exists for the upper), and
  the guest copies it into the new subvolume before composing the overlay. One
  slow launch (a 3 GB local copy), no new machinery; the copy is a guest-to-
  guest filesystem copy across virtio-fs -> blk.
- **Maintenance VM.** A dedicated one-shot run (the `cang build-disk resize`
  precedent) mounts the disk read-write in a narrow guest and copies, so the
  user's first real launch is not the slow one.
- **No migration.** The old directory stays a cold cache, and the new upper
  starts empty: every path comes back from the substituters. Cheapest to build,
  expensive for the user exactly once per workspace.

Decide, and specify the mechanics that make it safe: what marks the migration
*done* (so it runs exactly once per workspace, and a second `upper:blk` launch
does not copy again), what an interrupted copy leaves and how the next launch
recovers (today's `preseed` markers are the precedent:
`.cang-nix-preseeded`/`.cang-nix-preseed-attempted`), whether ownership survives
(store objects in the upper carry the guest's uid, and the guest copies as root -
is a plain `cp -a` right, and do hardlinks in the store's `.links` farms need
preserving), what happens to the old directory afterwards (kept, renamed, or
deleted by `cang reset` only), and how the copy is reported to the user (a
3 GB copy must not look like a hang).

## Resolution

(to follow)

## Resolution

**The first-launch import**, no maintenance VM. The host share that already carries
the overlay generation root (`cang-nix-overlay-src` at `/run/cang/nix-overlay`) is
bound in block mode too, so the guest can read the host-seeded upper at
`/run/cang/nix-overlay/upper`. A fresh generation with no sibling to clone from
imports it, once, before the overlay is composed; a sibling generation on the disk
always wins, because it is a snapshot rather than a copy over virtio-fs.

**What is imported is the cache, not the directory**: `store` (hardlinks kept) and
`var/nix/db`, and nothing else. The whole-tree `cp -a` failed live and named the
reason - `cp: preserving permissions for '.../var/nix/daemon-socket/socket': No
such device or address`, because a `/nix` root that ran under the host overlay
carries a live daemon socket - and that failure was fatal, as it should be now that
no host overlay exists underneath.

Idempotence is the sentinel's, as everywhere else: an already-seeded generation is
only probed for its mount, so the import happens exactly once per digest. An upper
with no store objects in it is not worth a copy and is skipped.

**Live**: `mig1` booted with `--nix=lower:virtiofs` wrote a marker into `/nix/store`
(which lands in the host upper), then booted with no flag: `Imported("/run/cang/nix-overlay/upper")`,
`state=ready`, the marker readable at `/nix/store/zz-migrated-marker`, and the same
store entry count (683) as the host-share boot. Evidence:
`/home/dev/disp/nixupper-smoke/{n1-host-share,n2-block-import}.ev/`.

Generations are also pruned now, which this ticket's fog had only glimpsed: a seed
keeps the generation that booted and the newest other completed one, deleting the
rest. Live: a workspace on its third digest cloned from the second and pruned the
first (`pruning the retired /nix generation sha256-99e4…`, `Delete subvolume 256`),
leaving two subvolumes and `state=ready`.
