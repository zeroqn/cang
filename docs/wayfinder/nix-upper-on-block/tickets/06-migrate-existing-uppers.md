---
label: wayfinder:grilling
title: How an existing host-directory upper becomes guest-side state
status: open
blocked_by: ["04-upper-home-and-auto-grow", "05-seeding-parity"]
claimed_by: ""
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
