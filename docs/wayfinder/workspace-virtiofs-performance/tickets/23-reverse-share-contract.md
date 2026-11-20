---
label: wayfinder:grilling
title: What does the host's view of its workspace promise - mount point, authority, staleness, failure?
status: open
blocked_by: [22]
claimed_by: ""
---

## Question

Once the transport exists (ticket 22), the share's *contract* has to be written
down, because it replaces a promise cang made for free when the workspace was a
host directory: "the host tree is live and authoritative" (ticket 03).

Decide, with bob:

1. **Where the host sees it.** The launch directory is the natural mount point -
   but it is also the directory pi and the editor are already sitting in, so
   either the mount lands on it (and the pre-mount host copy becomes a shadow),
   or the view appears somewhere else and the launch directory stops meaning
   "the workspace". Pick one and say what the other becomes.
2. **Authority.** Settled: the image is authoritative (map, 2026-11-20 Q1). What
   is *not* settled: what happens to the host copy that existed before the image
   (ticket 25), and whether a host write is ever allowed to be served from a
   cache rather than the guest.
3. **Staleness and concurrency.** With both sides mounting the same btrfs tree
   (does the *host* mount it, or does every host operation go through the
   guest?), what does a host `stat` see after a guest `rm`, and what does a
   concurrent write from both sides do. If the host must not mount the same
   btrfs twice, say which side owns the mount and how the other side's operations
   are routed.
4. **Failure.** Guest dead, VM killed, `cang` exit 0 while the guest was still
   working (ticket 09): what a host tool calling `read()` sees, and whether host
   writes are queued, refused, or lost. "Refused, by name" is the default; a
   lost-write window is a decision, not an accident.
5. **What the first acceptance proves**: one host-side edit visible in the guest
   and one guest-side write visible on the host, with the tool being a real one
   (`git`, `rust-analyzer`, pi's file tools), not a `cat`.

## Resolution

(to follow)
