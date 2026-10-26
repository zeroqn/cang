---
label: wayfinder:grilling
title: Does the zero-message-open win (2.66x on a build) justify relaxing ticket 03's "exact host visibility" by up to attr_timeout?
status: open
blocked_by: []
claimed_by: ""
---

## Question

Note 23 implemented zero-message open and measured it: a full synthetic rustc arm
went **372.8 s -> 140.2 s mean (2.66x)** over four interleaved runs per side, and
the `open` probe cell 1788 -> 4788 ops/s at one thread. It is on by default in
`crates/cang-libkrun` (`CANG_FS_ZERO_MESSAGE_OPEN = true`), with
`CANG_FS_ZERO_MESSAGE_OPEN=0` as the escape hatch.

The one cost: with no `FUSE_OPEN` to carry open flags, the guest kernel defaults to
`FOPEN_KEEP_CACHE | FOPEN_CACHE_DIR` (`fs/fuse/file.c:155-156`), so **a file the
guest has read stays cached after the open that read it**. A host-side *in-place*
rewrite of such a file becomes visible when its attributes refresh
(`attr_timeout`, 5 s) instead of at the next `open`. Measured in note 23: the
guest saw the host's rewrite after 6 s, not at 1 s; the arm without the lever saw
it immediately. Rename-based host writes (every editor, `git checkout`, `sed -i`)
are new inodes and unaffected, and a host-added file in a listed directory is
visible immediately in both arms.

That is the thing [ticket 03](03-workspace-cache-coherence-contract.md) decided
against: "**no** - cang promises exact host visibility, so no write caching ever".
No *writes* are cached and nothing is lost - this is a read-visibility delay
bounded by the metadata timeout the map already accepted - but it is a change to
what `/workspace` promises, so the decision belongs to bob, not to a measurement.

## What would settle it

Bob answers one of:

1. **Accept the window**: keep the default on, and say in the user-facing docs that
   a host-side in-place edit is visible to a running guest within
   `attr_timeout` (5 s), not immediately - with the escape hatch named.
2. **Keep exact visibility**: flip `CANG_FS_ZERO_MESSAGE_OPEN` to `false` (one
   line) and leave the lever as an opt-in experiment, losing 2.66x on every build.
3. **Split the difference**: bind `/workspace` as its own virtio-fs device and turn
   the lever on only for the image rootfs (which the host never rewrites in place).
   That is a cang-side mount-layout change (two request queues, two pools, two
   `Fs` devices) and the map's "no cang-side plumbing for its own sake" reading
   says it needs its own ticket.

Whichever way it goes, note 23's numbers and the `CANG_FS_STATS` histogram stay
valid; only the default and the docs move.
