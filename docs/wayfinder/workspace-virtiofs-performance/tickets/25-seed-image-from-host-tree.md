---
label: wayfinder:task
title: Seed the first workspace image from the host tree, once, without a hand-written copy script
status: open
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

(to follow)
