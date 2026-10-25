---
label: wayfinder:task
title: Re-record the baseline in a clean window (no co-tenant VM on the host)
status: open
blocked_by: []
claimed_by: ""
---

## Question

The recorded numbers were taken while another session may have been running a live
smoke against the same host, so they are indicative rather than canonical. Re-run
and replace them:

1. Confirm the window is clean: no other `cang` task running (`cang ps`), no
   `libkrun-vm-worker` process, and bob has confirmed the other session's smoke is
   finished.
2. `tools/rust-compile-bench/run.sh --runs 3` (host, guest-virtiofs, guest-local),
   one arm at a time.
3. Replace the README's baseline table and the map's numbers with the clean
   figures, keep the old ones only if they differ by more than the spread, and say
   which window each number came from.
4. Re-take the `fs_worker` CPU sample (`notes/` has the recipe) against the clean
   window - it is the number ticket 04 was decided on, and it is worth having it
   uncontended.

## Resolution

(to follow)
