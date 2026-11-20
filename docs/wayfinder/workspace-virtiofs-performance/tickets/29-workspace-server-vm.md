---
label: wayfinder:prototype
title: A workspace server VM that serves the image whenever no task VM runs
status: open
blocked_by: []
claimed_by: ""
---

## Question

Ticket 23 settled the contract: the host always sees the workspace image, served by
whichever guest owns it - the task VM while one runs, and a **long-lived workspace
server VM** in between. This ticket is that server's design and its smallest
implementation.

Decide/do, one session:

1. **Surface.** `cang workspace serve <disk>` (a start/stop/status lifecycle), or a
   mode of the existing session machinery? cang already has a long-lived VM
   (`cang` with no command enters a shell) and `--daemon`; say which one this
   reuses and what it does *not* inherit (a task rootfs per launch is wasteful for a
   server that may live for hours - is there a lighter rootfs path?).
2. **Ownership handoff.** The image must have exactly one guest at a time: the
   server releases it before a task VM attaches, and re-takes it afterwards. Where
   does that handshake live (the host CLI, or a lease file the way
   `nix_overlay_lease`/`ImageLock` already work), and what does a
   `cang -- build` do when the server is running - stop it, wait, or refuse by name?
3. **Host mount lifecycle.** The mount appears at the launch directory when the
   server starts and disappears when it stops; read-only while a task VM holds the
   image (ticket 23 §3). What happens to a host process sitting in that directory,
   and to `cwd`-relative paths, when it flips read-only mid-session?
4. **The smallest live proof**: `cang workspace serve work.raw`, host `ls`/`git
   status`/editor save through the mount, then `cang -- build` refuses to take the
   image (or takes it after the server stops), then the mount returns. Measured,
   with the relay's op cost recorded.

Build on the relay spike (ticket 22's harness + the FUSE relay), not on a new
transport.

## Resolution

(to follow)
