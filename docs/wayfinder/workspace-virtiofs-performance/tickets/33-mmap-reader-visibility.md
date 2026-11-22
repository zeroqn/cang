---
label: wayfinder:research
title: What does an mmap'd guest reader see after a host in-place rewrite of the same file?
status: closed
blocked_by: []
claimed_by: "pi (2026-11-21)"
---

## Question

Ticket 20 resolved on the strength of two reader shapes - a held descriptor and a
fresh reopen - both of which saw a host in-place rewrite at the first 200 ms sample
([note 70](../notes/70-in-place-rewrite-window-does-not-reproduce.md)). An `mmap`'d
reader was **not** measured, and the map has no answer for it (note 63 said the same
of the reverse share).

Measure it, on the shipped configuration, with one arm per mapping flavour:

1. a MAP_SHARED mapping of a file the guest has read, page-faulted, then the host
   rewrites the file in place (same size and changed size) - when do the mapped pages
   change, and which mechanism (page invalidation, a fault that re-issues a read,
   nothing at all)?
2. MAP_PRIVATE (a copy-on-write mapping) - is the guest's view frozen by design here,
   and does that differ from the host's file?
3. the same two with `--fs zero-message-open:off`, so the answer says whether the
   lever is what decides it.
4. a truncated-then-rewritten file (`O_TRUNC` from the host) while the guest maps it,
   which is the shape where a stale mapping is most dangerous (SIGBUS or stale bytes).

Report: the observed freshness per shape, the page-cache/writeback mechanism visible
in `/proc/meminfo` or a `mincore` probe if it helps, and a verdict on what the
user-facing docs may claim. If a shape cannot be measured without root in the guest,
say so and leave it unclaimed rather than inferring it.

## Resolution

Measured 2026-11-21 ([note 71](../notes/71-mmap-reader-never-sees-host-rewrite.md)):
**an already-faulted mapping never sees the host's in-place rewrite** - stale for
the whole 11.5 s observation, well past the 5 s attribute timeout - and it heals only
when the reader re-maps. The same for MAP_SHARED and MAP_PRIVATE, for a same-size
rewrite and a grown file, for a host truncation (stale bytes, no `SIGBUS`), and with
`--virtiofs zero-message-open:off`, so it is not the lever: nothing tells the guest
kernel that the host changed the file (in FUSE that is `INVAL_INODE`, and only the
guest->host relay watcher sends one).

`read(2)` readers stay fresh (ticket 20's shape), so the honest claim is:
`/workspace` contents are fresh for `read(2)` in the shapes measured; an `mmap`'d
reader can stay stale until it re-maps. Documented in `docs/usage.md` next to the
zero-message-open note. Left unmeasured and unclaimed: guest `MAP_SHARED` *writes*
into the host file.
