# The workspace server VM, live - and what it deliberately does not do

2026-11-21. Ticket 29, stage 2, commit `a14d2928`. This note records the
independent verification pass (mine, on the shipped binary), not the
implementing session's report.

## What shipped

`cang workspace serve|stop|status <DISK>`. `serve` runs one VM whose guest mounts
the image at `/workspace`, starts the relay, and mounts it on the host (cwd by
default, `--mount-at PATH`); it stays in the foreground and on `^C`/SIGTERM/SIGHUP
performs an **ordered teardown - unmount, then stop the VM** - and exits with that
result. Reuses the session machinery (`cang exec`, `ps`, `kill` work on it), and
skips the terminal attach on purpose: the attach client puts the host terminal into
raw mode, so a `^C` would reach the guest payload and kill the VM before the mount
came down.

The single-writer rule is a refusal, not a handoff: an exclusive `flock` keyed on
the **canonical** image path for the server's lifetime, shared for every
`--workspace-disk` launch, `LOCK_NB` so both parties fail fast with a named
message rather than waiting.

## The arms, re-run by me

Binary `/nix/store/413b9amf0x2d7pw34v1hdbqqrl915sga-cang-0.22.0/bin/cang`, image
`localhost/cang:latest 4edac48b4182` (built from that same tree), image
`/tmp/wsverify/ws.raw` (8 GiB btrfs, seeded with a one-file git repo):

| arm | result |
| --- | --- |
| serve, cold | mount up in **<10 s**; `findmnt` -> `cang-workspace fuse rw,nosuid,nodev`; host sees `a.txt`, `cargo` |
| host `git status --porcelain` | **58 / 59 / 67 ms** (note 64 had 75 ms on the same tree; the relay's cost is stable, and the child's own 122-247 ms run had `--log-level debug` inflating it by ~90 us/frame) |
| host write -> guest read | `echo host-write-... > <mount>/from-host.txt`, then `cang exec <task> -- cat /workspace/from-host.txt` returns it |
| guest write -> host read | guest wrote `/workspace/from-guest-serve.txt`; `cat <mount>/from-guest-serve.txt` returns it |
| task launch on the served image | refused, exit 1: *"the workspace image ... is being served by a cang workspace server, and only one guest may mount that image at a time: stop it first with 'cang workspace stop ...'"* |
| second server | refused, exit 1: *"another cang process is using the workspace image ...: a workspace server and a --workspace-disk task launch may not share one image ... (lease '/tmp/cang-1000/workspace-server/<hash>.lease')"* |
| `workspace status` | task id, pid + status, task dir, mountpoint + mounted, started, lease path |
| `workspace stop` | **267 ms**, mount gone, record removed; a `--workspace-disk` launch then works and reports `/workspace` as `btrfs` |
| restart | mount up in **~10 s**, showing `from-task-1795251500` - the file the task wrote while the server was stopped |
| EBUSY stop (a `sleep` with cwd inside the mount) | **exit 1** with both named messages, mount left for `fusermount3 -u`; after killing the holder, a manual `fusermount3 -u` clears it and `cang workspace stop` then correctly reports no server |

**Changed 2026-11-25** (from a `cang xxx.raw` report: "after exit the mounted path
cannot be accessed again"): the EBUSY row above is no longer the behaviour. A
refused plain `fusermount3 -u` is repeated lazily (`fusermount3 -u -z`) at exit,
so the mount leaves the namespace and the path is the host tree again; only the
holder keeps the dead superblock, and its shell has to `cd` out by an absolute
path (`cd ..` does not escape a detached mount). Live arm, cang 0.23.6: a
`sleep` with cwd inside the mount -> two refusal lines, mount left; then
`fusermount3 -u -z` -> rc=0, mountinfo empty, `stat` on the path back to the host
inode (`dev=46 ino=13515174`, was `dev=70 ino=256` through the mount).

Two input-error paths worth knowing: `--mount-at` at a directory that does not
exist fails with *"failed to canonicalize cang workspace mount: No such
Directory"* (the directory must exist), and a `--mount-at` that is already mounted
is refused by name.

## What this deliberately does not do (still open)

- **no read-only flip under a task**: the lease makes a task and a server
  mutually exclusive, so the flip of ticket 23 §3 has nothing to act on yet;
- **no handoff/reconnect**: a task cannot take the image while the server runs,
  and a server restart re-mounts rather than resuming;
- **no guest-side invalidation** (ticket 30): a host reader still sees stale files
  until it reopens them, and no host `inotify` event comes from a guest write;
- **`-EIO` still has no window**: with a single owner the mount never outlives the
  process that serves it. If the VM dies, the server exits and unmounts.
