---
label: wayfinder:prototype
title: A workspace server VM that serves the image whenever no task VM runs
status: open
blocked_by: []
claimed_by: "pi (stage 1 shipped 2026-11-21)"
---

## Measured baseline this builds on (2026-11-20, [note 62](../notes/62-fuse-relay-spike.md))

A real mount already exists in the spike harness: `fusermount3` unprivileged, the
guest serving its own btrfs image through a `/dev/fuse`<->vsock pump, **236 µs median
per FUSE frame**, 2.9-3.6 k frames/s serial, host `git status` **0.211 s** against
0.040 s native. So this ticket is lifecycle and plumbing, not feasibility - except
for the two items ticket 31 owns (death semantics for `mmap`, and the 4 KiB write
cap). The pump reconnects after a restart (unmount + restart re-connects to the same
guest), which is what makes a *server VM* lifetime independent of a mount's.

## Stage 1 shipped (2026-11-21, commit `e47250c6`, [note 64](../notes/64-relay-stage1-live-arms.md))

The relay itself is now cang's own code and runs in a real launch:
`--workspace-relay` + `--workspace-disk` mounts the guest's tree on the launch
directory through a FUSE relay over vsock (guest half `crates/cang-guest-relay`,
host half `crates/cang-relay-pump`, guest port 50 430). Live numbers: host
`git status --porcelain` **75 ms** against **11 ms** warm guest-native; INIT
`flags=0x406020 max_write=1048576 max_pages=256` (the big-write patch is in the
product path). Two defects were found by those arms and fixed: the acceptor socket
was created as the worker identity so the pump got `EACCES` for ever (the worker now
hands it to the host user), and the guest server served its own `/` instead of
`/workspace` because `fuse-backend-rs` reads the tree from `config.root_dir`.

**What stage 1 deliberately does not have** - the rest of this ticket: the mount
dies with the launch (so `-EIO` has no window, and between launches the host sees
its own directory again), no read-only-under-a-task flip, no reconnect across a
server restart, and no single-writer lease between a server and a task VM.

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

## Decisions for stage 2 (2026-11-21, this session - taken where the ticket asks, so the build has a shape)

Recorded before implementing, so a later session can see what was chosen and why:

1. **Surface - a new `cang workspace serve <disk>` subcommand**, not `--daemon` and not
   the no-command shell. It reuses the *session* machinery (task rootfs, supervisor,
   managed attach, exec socket) because that is what gives it `cang exec`, `cang ps`
   and `cang kill` for free, and it **does not** reuse the per-launch task-rootfs
   cost: the rootfs is created once per server and its btrfs snapshot is reused on
   restart. `--daemon` is wrong here (it returns while the task runs; the mount must
   live exactly as long as the server).
2. **Ownership - a lease file beside the image, flock-based**, exactly the shape
   `ImageLock` already uses for the block image: the server takes an *exclusive* flock
   for its lifetime; a task launch that names the same disk takes it *shared* (it
   already does for its own image) and **refuses by name** while the server holds it,
   telling the user to `cang workspace stop` first. No automatic stop, no waiting: a
   silent stop would turn "start a build" into "kill the session someone is editing
   in". Handoff (server releases, task takes, server re-takes) is *not* stage 2.
3. **Mount lifecycle - the mount is up for the server's lifetime and read-write**,
   because nothing else may touch the image while it runs (§2). The read-only flip of
   ticket 23 §3 only becomes necessary once a task can take the image while the
   server survives, which this stage forbids; the flip stays open.
   A host process with its cwd inside the mount makes `fusermount3 -u` fail
   (`EBUSY`); the server reports that by name and exits, rather than looping.
4. **Live proof**: `cang workspace serve work.raw`, host `ls`/`git status`/write
   through the mount; a `cang --workspace-disk work.raw -- …` launch refused by
   name; `cang workspace stop`, then the launch works; restart the server, the
   mount returns.

## Resolution

(to follow)
