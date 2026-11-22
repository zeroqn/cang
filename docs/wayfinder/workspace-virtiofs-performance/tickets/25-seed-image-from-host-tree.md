---
label: wayfinder:task
title: Seed the first workspace image from the host tree, once, without a hand-written copy script
status: closed
blocked_by: [18]
claimed_by: ""
---

## Question

Under the redraw the image is authoritative, but every existing checkout starts
life as a host directory. Something has to put that tree into a brand-new btrfs
image, once, and then say what the host directory is afterwards (ticket 23).

Do:

1. **Copy semantics.** What is copied (everything? excluding `target/`, `.git`
   objects with alternates pointing at absolute host paths, sockets, hardlinks,
   xattrs, sparse files), how (`cp -a` inside the guest after mounting an empty
   image, or a host-side copy before boot), and how a failure leaves the image
   (never half-seeded: sentinel last, the way the `/nix` upper seeding does it).
2. **Trigger and idempotence.** Which command does it (`cang workspace init`,
   or implicitly on first launch of a checkout with no image), what it prints,
   and what happens if it is run again.
3. **Size.** The image is created at some size; a tree larger than it must fail
   before copying, not halfway through.
4. **The honest check**: seed this repo's own checkout into an image, boot, and
   prove `git status` and a `cargo check` behave (no need for a full build -
   ticket 18 measures).

## Resolution

Shipped 2026-11-21, commit `5175b4ef`, verified live
([note 69](../notes/69-seeding-from-a-host-tree.md)).

1. **Copy semantics**: everything except `target/`, `.scratch/` and the tree's own
   `.cang/`; `tar` rather than `cp -a` (the latter aborts on the first socket);
   `.git/objects/info/alternates` detected and warned about (absolute host paths).
2. **Trigger and idempotence**: explicit `cang workspace init <disk> --from DIR`
   (nothing seeds implicitly - a `--workspace-disk` launch must never copy gigabytes
   before the user's command). Re-running it on a seeded image is refused by name.
3. **Size**: the fits check is host-side, before any VM boots - the tree may fill at
   most 85 % of the image, and the refusal names both sizes. The marker
   `.cang/seed-complete` is written last, so an interrupted seed is still acceptable
   to a later one.
4. **The honest check**, on this repository: 50 471 entries seeded into a 16 GiB
   image; booting it with `--workspace-disk` reproduces the host's own
   `git status` (three edited files, dirty submodule, `?? .cang/`) and the same
   `HEAD` via `git log`, and `cargo check -p cang-guest-relay` finishes inside the
   guest. A whole-workspace build needs the `nix develop` devshell, on either side of
   the VM - that is a property of the repo, not of the seed.
