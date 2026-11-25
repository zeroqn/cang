# Usage reference

Day-to-day cang command reference beyond the [README](../README.md) quick
start and command list.

## `cang exec`

`cang exec <task-id-or-handle-selector> -- COMMAND...` runs a non-PTY foreground
command in an active task with separate stdin, stdout, and stderr streams. It
uses the task's `/workspace` directory and returns the guest command's exit
status. Tasks launched by older cang versions do not have the exec transport;
relaunch them with the current version before using `cang exec`.

## Sessions: detach and attach

- A normal foreground `cang` run starts a managed guest PTY session and then
  attaches the host terminal to it. The foreground experience is still an
  interactive shell or command, but the guest process is not tied to the host
  terminal lifetime. Managed guest PTY sessions preserve the launching host
  terminal identity by passing non-empty UTF-8 `TERM`, `COLORTERM`,
  `TERM_PROGRAM`, and `TERM_PROGRAM_VERSION` values into the guest; this is
  limited to the managed attach path and does not enable broad host environment
  passthrough. The guest init also defaults missing or empty `LANG` and
  `LC_CTYPE` to `C.UTF-8` so locale-sensitive terminal programs such as `tmux`
  can use UTF-8 character widths, including CJK text. Explicit locale values
  are preserved, `LC_ALL` is not set, and this does not broaden host
  environment passthrough. When the managed guest reports an exit status, cang
  propagates that status as the foreground process result; a guest status such
  as 127 is distinct from cang helper or VM infrastructure failure diagnostics.
  Managed helper diagnostics are mediated by the parent after terminal raw mode
  is restored, so infrastructure errors start on a fresh terminal line instead
  of racing with guest PTY output. Detached or `--preserve-debug` sessions keep
  helper stderr in the task state directory as `helper.stderr.log` for later
  inspection; normal managed cleanup removes it with the task state.
- Press `Ctrl-\` twice to detach from the current terminal
  session. cang recognizes both raw `Ctrl-\` bytes and CSI-u/Kitty-encoded
  `Ctrl-\` events from terminals or multiplexers. The host-side filter
  intercepts the sequence before it reaches the guest TTY, where `Ctrl-\`
  would otherwise be the POSIX quit character. Closing the host terminal,
  killing the attach client, or losing SSH also behaves as detach: the guest
  shell or terminal-interactive command keeps running while the VM helper
  remains active.
- `cang --daemon` starts the managed guest PTY through the launching terminal,
  forwards startup input/output so the target program can complete terminal
  initialization, then detaches automatically after the first target output is
  followed by a short idle window. The heuristic is generic and does not parse
  shell prompts. This mode is TTY-only; if stdin or stdout is not a terminal,
  cang fails before sending the attach frame that starts the target program.
  Use `cang attach <task-id-or-handle-selector>` (or
  `cang a <task-id-or-handle-selector>`) to reconnect and
  `cang kill <task-id-or-handle-selector>` to terminate the detached task.
- Reconnect with `cang attach <task-id-or-handle-selector>` or its `cang a`
  shortcut. Selectors follow the same task-id/handle matching rules as
  `cang kill`; use `cang ps` to list running task IDs and handles. Reattach
  repaints the current visible terminal screen from bounded in-memory guest PTY
  state before forwarding new output, so a detached shell or TUI should be
  usable without pressing `Enter` just to
  redraw. This restore state is not persisted across helper or VM restart.
- Only one attach client is supported at a time. A second attach attempt receives
  a busy error instead of sharing the PTY. `cang attach --force
  <task-id-or-handle-selector>` takes the session over instead: the guest hands
  the PTY to the new client and the evicted client detaches (`cang: detached`),
  as `Ctrl-\` would have. Use it when the client holding the session is gone but
  the guest never saw it leave, for example after a dropped SSH connection or a
  terminal closed under a still-running attach process.
- `cang -- <command>` behaves like a process launcher: the command's output is
  streamed to cang's stdout while it runs, and cang exits with the command's
  status. A command killed by a signal reports `128 + signal` (a SIGTERM-killed
  command exits 143), so a killed command is never mistaken for a success. Host
  stdin bytes are forwarded to the command, and when stdin ends (a pipe, a
  redirect, `/dev/null`) cang keeps the session and hands the command the guest
  PTY's end-of-input instead of detaching. The command still runs on the
  managed guest PTY, so its stderr shares the stream with its stdout. The guest
  reports the status through the fork's virtio-fs exit-code ioctl before the VM
  ends, so it also survives a teardown that ends the attach session first.
- A launch into a workspace that already has a running cang task is refused
  with an error naming the task and its handle. Wait for it to finish,
  `cang attach` it, or `cang kill` it before launching again.
- Terminal and TUI programs inside the PTY are in scope, including
  `cang -- <interactive-command>`. Graphical X11/Wayland application
  preservation is not implemented; display sockets and GUI reconnect semantics
  need a separate design.
- The attach transport is libkrun's vsock-to-host-Unix-socket mapping. If the
  required `krun_add_vsock_port2` symbol or setup path is unavailable, managed
  attach fails clearly instead of falling back to another transport. On slow or
  heavily loaded hosts, managed attach readiness can need a longer guest boot
  window before the guest sends its initial `Hello` frame. Increase the bounded
  readiness windows with:

  ```bash
  CANG_MANAGED_ATTACH_READY_TIMEOUT_SECS=180 \
  CANG_MANAGED_HELPER_READY_TIMEOUT_SECS=190 \
  ./result/bin/cang -- bash -lc 'echo ok'
  ```

  `CANG_MANAGED_ATTACH_READY_TIMEOUT_SECS` controls how long the helper waits
  for the guest attach listener to complete the initial `Hello` handshake.
  `CANG_MANAGED_HELPER_READY_TIMEOUT_SECS` controls how long the parent waits
  for the helper to report readiness.
- Exiting the guest shell or command terminates the VM and removes the active
  task/rootfs unless `--preserve-debug` was used. Detached tasks can be
  terminated with `cang kill <task-id-or-handle-selector>`.

## Task control: `cang ps` and `cang kill`

Active task control is cang-native and does not use host Podman as a runtime
backend:

```bash
cang ps
cang kill <task-id-or-handle-selector>
```

`cang ps` scans cang's app state and lists active task VM records across all
workspaces by default. The human-readable table includes a short handle, full task
id, status, helper PID/session identity, start timestamp, image, and workspace
slug. For a full task id like `cang-4138-178109091122334455`, the handle is
`cang-4138`, so `cang kill cang-4138` can target that task without typing the
opaque suffix. You can also use a displayed-handle prefix of at least two
characters, such as `cang kill lo`, when that prefix uniquely matches one
visible handle. For handles shaped like `<name>-<number>`, `cang kill` also
accepts `<name-prefix>-<handle-number-prefix>` when it uniquely matches a
displayed handle; for example, `cang kill lo-18` can target
`cang-1845-<opaque>` through its displayed handle `cang-1845`. The
handle-number prefix is the numeric segment shown in the displayed handle, not
the helper process PID. Prefix matching is only against displayed handles, not
full task ids. It is an active-task view only: completed task history, log
inspection, JSON/API output, restart/pause/exec operations, and Podman-backed
management are intentionally out of scope.
`cang kill <task-id-or-handle-selector>` validates the recorded process and
session identity before signaling the task process group, sends `SIGTERM`, waits
briefly, and escalates to `SIGKILL` only if the task is still running. Ambiguous
handles or handle selectors, too-short prefixes, malformed abbreviated
selectors, reused process ids, or unreadable process identities are reported
instead of signaled. Stale records for already-exited tasks are eligible for a
cleanup retry without signaling. A successful kill request only returns after
the task rootfs/state cleanup succeeds, then removes the active record from
subsequent `ps` output. If cleanup fails after the VM process is gone, `cang
kill` returns a visible error and leaves or restores the active record so rerun
`cang kill <task-id-or-handle-selector>` can retry the same cleanup.

## A workspace on its own disk

`--workspace-disk PATH` serves `/workspace` from a btrfs filesystem on a raw image
or a block device instead of the host directory over virtio-fs. A bare positional
argument is the same setting - `cang ./work.raw` is `cang --workspace-disk
./work.raw` - and the guest command still needs its `--` delimiter. `PATH` is
created on first use (`--workspace-size`, default `64G`) and validated on reuse:
a file holding anything other than btrfs, a symlink, or a directory is refused
rather than reformatted, and so is an image smaller than a size you asked for
explicitly. `--workspace-size` is a *creation* size: an image first made at
32G stays usable without passing the flag again.

```bash
./result/bin/cang ./work.raw --workspace-size 32G
./result/bin/cang --workspace-disk ./work.raw -- bash -lc 'findmnt /workspace'
```

Cang grows that image itself, but only where growing it pays for itself. The
decision is taken *between* runs, because a guest mount and a mid-run grow are
mutually exclusive: virtio-blk fixes a device's size when it is probed. So the
guest records what its tree had free (`/workspace/.cang/free-bytes`) when it
mounts the disk, and the host reads that number back through one short read-only
maintenance VM. An image whose `cang-guest-init` predates that note reports
nothing, and the host grows nothing without a number.

- **`cang workspace serve` grows the image before it mounts it.** The server is
  long-lived, so one maintenance VM before its mount is a once-per-server cost.
  Under a tenth of the image free, the image grows by twice what is in use plus
  `16G` (so the next grow is not one compile away), and the host must keep twice
  that step's headroom. When the host cannot pay, the serve says so on one line
  with both numbers and starts anyway; the command that grows it by hand is
  `cang disk resize --workspace-disk PATH SIZE`.
- **a one-shot `--workspace-disk` launch does not auto-grow**: adding a whole
  maintenance VM boot to every launch is not worth the convenience. Grow it by
  hand instead, which is the same command the refusal line names:

```bash
./result/bin/cang disk resize --workspace-disk ./work.raw 100G
```

Only the host's sparse file grows either way: the guest expands its btrfs
filesystem to the new device size when it next mounts the disk. `--workspace-size`
stays a *creation* size, never a cap, so a refusal is about the host's free
space, not about the flag - and no `cang disk` subcommand deletes an image:
`cang disk reset` still only offers `--containers`.

The guest mounts the disk at `/workspace`, and everything a build writes stays
there: sources, cargo's own `./target`, and the cargo home, which lives in the
tree as `cargo/` and is bound at `~/.cargo` (seeded once from the registry the
launch mounted there, then kept on the disk). The command runs in that tree too:
a shell starts on the mounted btrfs root, not on the empty rootfs directory the
mount covers (`pwd` says `/workspace` either way, so `.` and `/workspace` are the
same tree only because the guest enters the mount once it is up). Three
consequences:

- the host-directory `/workspace` bind mount and the `~/.cargo` mount are dropped
  for such a launch, so the guest's tree is the only copy: an interactive launch
  shows it back on the host through the reverse share, which is on by default
  (next section), and `--no-workspace-relay` leaves the launch directory showing
  its own contents;
- the build disk is neither created nor attached: with `target/` and the registry
  inside the tree it has nothing left to hold;
- `/nix` follows the disk: the launch serves it from the read-only btrfs
  **block lower** built from the image's store, and the writable layer lives in
  the tree at `/workspace/.cang/nix/<digest-key>/` (one subvolume per image
  generation, `upper`/`work` as plain children, seeded by the guest once). The
  guest's overlay is then the only `/nix` mount. There is no `--nix` flag: a
  launch without `--workspace-disk` keeps the host-composed overlay instead, and
  if the lower cannot be built (host space, or an image whose `cang-guest-init`
  predates the guest half) the launch keeps that overlay and names the reason.

### Seeing the guest's tree on the host

An interactive `--workspace-disk` launch mounts the guest's tree on the launch
directory by default, for the duration of the task, so `git`, an editor and the
agent harness itself keep working on the real files:

```bash
cd /tmp/serve-here
./result/bin/cang ../work.raw          # the image beside the launch directory
./result/bin/cang work.raw             # the image inside the launch directory
```

Both spellings mount the same tree at the same place. The second hides
`work.raw`'s own pathname for as long as the launch lives, and cang can carry
that because the VM has the image open before the mount can exist: the pump
proves the guest's relay is listening before it calls `mount(2)`, and the guest
starts that relay only after `mount-workspace-disk` and after booting. The file
is not lost - it is visible at its path again once the launch ends and the mount
goes away. While the mount is up that pathname belongs to the guest's tree: a
second launch from the same directory is refused as a workspace that already runs
one, and `cang disk resize work.raw` works only from outside it or after the
launch has exited.

The mount lives exactly as long as the launch. On the way out cang unmounts it;
when a host process still holds it - a shell that `cd`ed into the tree while the
mount was up, an editor, a watcher - the plain `fusermount3 -u` is refused
`EBUSY`, and cang repeats it lazily (`fusermount3 -u -z`) and says so. The
detached mount leaves the namespace, so the path shows the host tree again
immediately and nothing has to be cleared by hand; only the shell sitting *inside*
the old mount keeps the dead superblock, and it has to `cd` out by an absolute
path (`cd ~`, then back in). `cd ..` does not escape a detached mount.

`--workspace-relay` *forces* that mount instead of taking the default, and
`--no-workspace-relay` turns it off. The default skips it in two cases:

- **the launch runs a trailing command** (`cang work.raw -- make`): its process
  ends with that command, so a mount that appears mid-run is not the host's view
  of a tree it can use. The skip says nothing, because a launch like
  `cang workspace init` never wanted a line about a mount. Pass
  `--workspace-relay` when a long command's tree is worth watching from the host;
- **`--daemon`**: the mount lives exactly as long as the cang process, and a
  daemon launch returns while the task keeps running. This one names its reason
  on one line.

When it is forced, a launch that cannot carry the mount is a named refusal
rather than a skip: `--daemon`, `--workspace-relay` without `--workspace-disk`
(the host directory already *is* the tree), and a launch that runs a trailing
command while the image sits inside the directory the mount would land on - that
command lives exactly as long as the mount and may reopen the image by path
itself (`cang workspace init` seeds *from* that very directory), so the refusal
says to move the image outside the tree or launch interactively.
`--no-workspace-relay` has nothing to suppress in a launch without a disk image.

The mount is a FUSE filesystem served by the guest over vsock: every host
operation is a round trip to the VM (measured ~143 us, so `git status` on this
repository takes about 75 ms against 11 ms inside the guest). That is the whole
point - writes and reads on a hot build path never leave the guest, and only the
lower-frequency host tooling pays the trip.

What it promises, and what it does not:

- **the image is authoritative**; the host view may be *stale* - without the
  invalidation watcher below, a host reader that keeps a descriptor open sees an
  older version of a file for as long as its attribute timeout (5 s), and for a
  same-size rewrite never. A plain `open` is always fresh (`CachePolicy::Auto`
  drops the cached pages on open), so this is the long-lived reader - the agent
  harness, an editor, a `tail -f` - and never `cat`/`git`. No host `inotify`
  event is synthesised either way: the FUSE protocol can invalidate cache, it
  cannot deliver an event;
- **an `mmap`'d reader is stale until it re-maps** - in both directions of the
  model. A guest process that maps a `/workspace` file keeps the pages it faulted:
  a host in-place rewrite is *not* visible to it (measured stale for 11.5 s, past the
  5 s attribute timeout, and unaffected by `--virtiofs zero-message-open:off`), and
  only closing and re-mapping shows the new bytes. `read(2)` readers are fresh in the
  shapes measured (a fresh `open`, a reopen, a held descriptor). The host-directory
  share has no way to invalidate a guest mapping; the relay's watcher sends
  `INVAL_INODE` in the guest -> host direction only.

- **the mount does not outlive the launch**: when the task ends, or the VM dies,
  cang unmounts it and the launch directory shows its own contents again, so a
  guest-side file is invisible on the host between launches. For a view that
  stays up between launches, run a workspace server (next section);
- while the guest is gone, requests are answered `-EIO` rather than hanging.
  That is the pump noticing the guest stopped serving - the guest exit path, not
  a fault - and it names the transition only in its debug output
  (`--log-level debug`, or `CANG_RELAY_PUMP_DEBUG=1`; `docs/diagnostics.md`).

The mount does not arbitrate a concurrent host write and guest write to one
file: it is for a launch whose tree the host reads, not one it writes at the same
time.

One asymmetry is worth knowing, because it is what a host-side `cp`/`tar` into
the mount runs into: a **create-family** request (`create`, `mkdir`, `mknod`,
`symlink`) is executed inside the guest as the user who owns the tree - the
guest's `dev` identity, which is your own uid:gid, since guest-init provisions it
from `CANG_HOST_UID`/`CANG_HOST_GID` - while every other mutation (a write into
an existing file, truncation, `unlink`, `rename`, `chmod`) runs as the relay's
own identity. Those four are the only handlers that impersonate a caller, so a
file created through the mount is owned by the tree's user and the mount stays
usable for a file made by hand. It is not how a tree gets imported: a FUSE
round trip per entry is the wrong price for bulk, and a host `uid` the guest
cannot assume is still refused. Import with a copy inside the guest instead
(`--volume`, below):

    cang ./work.raw --volume /host/tree:/mnt/import:ro -- \
      bash -lc 'tar -C /mnt/import -cf - . | tar -C /workspace -xf -'

**The invalidation watcher** (`CANG_WORKSPACE_RELAY_WATCH=1` on the `cang`
command, forwarded into the guest environment; off by default) closes the
held-descriptor window from the guest side: the guest relay watches the tree it
serves and sends the host's kernel `FUSE_NOTIFY_INVAL_INODE` (`off = 0`,
`len = 0`) for a file whose content the guest changed. A host reader that holds
its descriptor open then sees the new bytes in **~1 ms** - one FUSE round trip -
instead of 5 s, and a same-size rewrite stops being invisible; a `stat` of the
path is equally fresh. Names are left to the mount's own 5 s entry timeout: the
kernel's `INVAL_ENTRY` path takes the parent directory's inode lock, and the
host pump is the mount's only server, so a host lookup in flight against that
lock deadlocks the mount (measured); `INVAL_INODE` never waits that way, and it
cannot refresh an open descriptor anyway. It invalidates cache and does nothing
more: a tool waiting for a host `inotify` event still needs a reopen or a
re-stat. The price is one inotify watch per directory (679 for the workspace tree
measured here; 1 374 for this repository's sources plus `.git`, 3 641 with a
populated `target/`, against the guest's 122 262-watch limit) and per-path
coalescing within one inotify drain, so a `target/` write storm costs at most one
`INVAL_INODE` per file rather than the 3.4-4.4 raw events per file a `tar`
extraction produces, and a flood of them does not slow the mount (~5 000
notifications/s delivered, `git status` unchanged).

### A block device instead of an image

`--workspace-disk` also takes a **block device**, and that changes what cang is
allowed to do with it: a device belongs to the host, so cang attaches it and does
nothing else.

```bash
./result/bin/cang --workspace-disk /dev/sdb1 -- bash -lc 'make'
./result/bin/cang --workspace-disk /dev/sdb1 --workspace-format -- bash -lc 'make'   # empty device only
```

- a device that already holds **btrfs** is attached as it is - never formatted,
  never modified;
- a device holding **any other filesystem** is refused by name (cang does not
  reformat a host device it did not create);
- an **empty** device is refused unless `--workspace-format` is passed, and that
  flag only ever formats a device with no filesystem on it;
- `--workspace-size` is refused with a device (it has the size its owner gave it),
  `cang disk resize --workspace-disk <device>` is refused for the same reason, and
  auto-grow never runs on one - so the guest's btrfs expands to the device's size
  on its next mount instead;
- the launch prints one line naming the device, because this is the one disk cang
  attaches without having created it; the host must not mount it while the guest
  has it, and nothing stops a second agent session from doing so.

`cang workspace init`, `workspace serve` and `nix gc` accept a device too: a seed
sizes the fits check from the device, a serve mounts it, and gc only reads it.

### Seeding a new image from a tree you already have

An image is authoritative and starts empty, so the first step for an existing
checkout is one shot that copies it in:

```bash
./result/bin/cang workspace init /tmp/work.raw --from . --workspace-size 32G
cang: seeded 50471 entries into the workspace image
```

The copy runs inside one task VM with the source tree mounted read-only at
`/mnt/seed`, so it needs no root on the host. What it does **not** copy: `target/`,
`.scratch/`, and the tree's own `.cang/` bookkeeping; `tar` skips sockets and
FIFOs (with a warning) rather than failing the copy, and a
`.git/objects/info/alternates` that names paths outside the tree is reported, since
those objects may not exist inside the guest.

Three refusals, all before anything is written:
- the tree does not fit — it may fill at most 85% of the image, and the refusal
  names both sizes and points at `--workspace-size`;
- the image is already seeded (`.cang/seed-complete`) — a seed is a one-time act,
  not a sync, so use a different image;
- the image's tree already holds something the seed did not put there.

The marker is written **last**, so an interrupted seed leaves an image a later
seed still accepts instead of one that looks complete and is missing files.

Proven on this repository: 50 471 entries seeded into a 16 GiB image, then booted
with `--workspace-disk` — `git status` inside the guest reports the same working
tree the host has (`git log` shows the same `HEAD`), and `cargo check -p
cang-guest-relay` finishes there. (A `cargo check` of the whole workspace needs the
`nix develop` devshell, same as on the host.)

### A workspace server: the host's view between launches

`cang workspace serve PATH` starts one long-lived VM that mounts the image at
`/workspace`, starts the guest relay, and mounts the guest's tree on a host
directory - the current one by default, `--mount-at PATH` to name another. It
stays in the foreground until it is interrupted (`^C`, `cang workspace stop`), so
the host's view of the image is continuous instead of dying with each launch:

```bash
cd /tmp/serve-here
./result/bin/cang workspace serve /tmp/ws.raw
# another shell, from anywhere:
./result/bin/cang workspace status /tmp/ws.raw
./result/bin/cang exec <task-id-or-handle> -- bash -lc 'ls /workspace'
./result/bin/cang workspace stop /tmp/ws.raw
```

The image path is created on first use exactly like `--workspace-size` describes,
and a relative path resolves against the served directory. The mount appears only
once the guest relay is *listening* - the pump proves the transport before it
calls `mount(2)` - and the server stops itself with a named reason if it never
appears, rather than leaving a VM whose tree nobody can see.

**One writer, enforced.** A workspace server holds an exclusive lease on the
image for its whole lifetime (a `flock` keyed on the disk path, in cang's
per-user runtime directory). A `--workspace-disk` launch naming the same image
takes that lease *shared*, so it is refused by name - "stop it first with `cang
workspace stop PATH`" - while a server holds it, and a second `cang workspace
serve` of the same image is refused the same way. Cang never stops the server for
you: turning "start a build" into "kill the session someone is editing in" is not
a decision it makes. Two `--workspace-disk` launches still share one image
between them, as they always did.

**Ordering, enforced.** On `^C`/`SIGTERM`/`SIGHUP` the guest is first asked to
flush `/workspace` to its disk (`sync -f /workspace` in the guest, over the exec
channel), because the VM is stopped with a signal rather than shut down and its
uncommitted btrfs transaction would otherwise be dropped - a host write made
through the mount a second before a stop is lost without it. Then the mount is
unmounted **while the guest is still serving it**, and only then is the VM
stopped. A host process whose working directory or open file is inside the mount
makes `fusermount3 -u` fail `EBUSY`; the unmount is then repeated lazily
(`fusermount3 -u -z`) and reported by name, so the served directory is the host
tree again as soon as the server stops and a shell that was inside the old mount
has to `cd` out by an absolute path. Only when both forms are refused does the
server exit non-zero, naming the refusals and the command to run by hand.

The server keeps its managed session but attaches *no* terminal to it, on
purpose: an attach client puts the host terminal into raw mode, `^C` would then
reach the guest payload as a keystroke, and the VM would stop before its mount was
unmounted - the reverse of the order above. `cang exec`, `cang ps` and `cang kill`
work against it exactly as they do against any other task (with `cang exec`,
`cang ps` and `cang kill` run from the served directory, because they find their
state root through the current directory), and `cang workspace status` reports
the recorded process and whether its mount is up.

A running server is still one VM with its own task rootfs and its own `/nix`
upper inside the served image (see "Reclaiming the `/nix` upper"); stopping and
restarting it costs a boot, not a rebuild.

## Volumes

Use repeatable `-v, --volume SOURCE:TARGET[:ro|:rw]` to add host bind mounts
to the prepared root. `SOURCE` may be a host file or directory; relative
sources are resolved from the workspace. `TARGET` must be an absolute guest
path. Omitting the mode defaults to read-write, `:rw` is explicit read-write,
and `:ro` remounts the bind target read-only after grafting:

```bash
./result/bin/cang -v /host/cache:/home/dev/project-cache -- bash -lc 'ls /home/dev/project-cache'
./result/bin/cang --volume /host/config.json:/workspace/config.json:ro -- cat /workspace/config.json
```

User volumes are additive only: they do not replace `/workspace`, `/nix`, or
the built-in tool config/state, compiler-cache, and container-store mounts, and
duplicate guest targets are rejected. A target at or under `/workspace` is
refused on a launch that serves `/workspace` from its own disk
(`--workspace-disk`, and any `cang xxx.raw` spelling of it): the disk's btrfs
tree is mounted at `/workspace` after the host grafts are in place, so the mount
would cover the volume and it would never appear. Use a target outside
`/workspace` for those launches, and `-v SOURCE:/mnt/import:ro` plus a copy
inside the guest when the files belong in the tree. Cang intentionally does not support Podman SELinux
suffixes (`:z`, `:Z`), ownership mutation (`:U`), propagation flags, named
volumes, or anonymous volumes.

## Root shell handoff

```bash
./result/bin/cang --root
# inside the root shell:
cang-as-dev          # execs fish -l as dev
cang-as-dev id -un  # runs a command as dev
```

`cang-as-dev` is packaged only in the cang image. It is a narrow root-only
helper for dropping from an interactive cang root shell back to the materialized
`dev` account. With no arguments it launches `fish -l`; with arguments it runs
that command as `dev`. Exiting that fish or command returns to the invoking root
shell only when the helper was started as a child process from an interactive
root shell. The helper does not provide sudo/su, does not switch arbitrary users,
and cannot be used by `dev` to regain root.

## Container-store maintenance

```bash
./result/bin/cang disk resize --containers 128G
./result/bin/cang disk reset --containers --force
```

These commands manage only the current workspace's `cang-containers.raw` disk
used by the raw-disk container store. They do not inspect or migrate any legacy
host-directory container store and do not resize or reset cang's host `/nix`
overlay state.
`resize` is grow-only: `--containers SIZE` (and `--workspace-disk PATH SIZE`,
below) accepts bytes or binary suffixes such as `K`, `M`, `G`, `T`, `KiB`, `MiB`,
`GiB`, and `TiB`, and the requested size must be larger than the current raw file. It grows the host sparse file first, then
starts a narrow one-shot direct-libkrun maintenance VM that runs
`cang-guest-init internal resize containers` to expand the guest btrfs
filesystem. If that guest resize fails after the host file has grown, cang does
not shrink or roll back the file; fix the reported VM/guest problem and rerun
the same resize command.

`reset --containers` is destructive and requires `--force`. It refuses to run while the
current workspace has running, pid-reused, unreadable, or unscannable task
records, deletes an existing regular `cang-containers.raw`, and recreates the
default 64 GiB sparse btrfs image without launching a VM. Stale-only task
records are reported as cleanup information and do not block either command.
For a manual smoke test on a host with Buildah, btrfs-progs, and libkrun
available, run `cang --container-store raw-disk` once, then run the `resize`
and `reset --containers --force` commands above.

## The retired build disk

The workspace's `cang-build.raw` btrfs image used to hold cargo's `target/` and
`CARGO_HOME`. Both live in the tree now - `./target` is cargo's own, and the
cargo home is `cargo/` inside the workspace, bound at `~/.cargo` - so cang no
longer creates, attaches or exports a build disk, and `cang disk resize --build`
is a named refusal pointing at `cang disk resize --workspace-disk PATH SIZE`.
There is no `cang disk reset --build` (the reset subcommand names `--containers`,
the only disk with a recreate path), and no launch refuses to delete one: the
reclaim below is the only thing that removes it, plus `rm` if you would rather not
wait for the next launch.

The guest also stops looking for one. Its `enter` path used to scan `/dev` for a
btrfs disk labelled `CANG_BUILD` on every boot, and, with no such disk attached,
spend ~100 ms failing and then print `cang-guest-init: build target disk
unavailable, cargo builds onto the workspace`. The scan now runs only when the
host names a build disk (`CANG_BUILD_DISK_LABEL`), which no launch does, so
neither the probe nor that line appears.

A launch that finds a build disk from an earlier cang reclaims it, and says so on
one line with the bytes it recovered:

```
cang: reclaimed 96.0 GiB from '<state root>/cang-build.raw' - a warm target/, the
cargo home and the retired /nix upper lived there; they now live in the workspace
image's tree, ...
```

The deletion is deliberately narrow: only `cang-build.raw` under this workspace's
state root, only when it is a regular file, and only when an exclusive non-blocking
`flock` on it succeeds - an image another guest still has attached is left alone,
silently. A removal that fails is reported on one line and the launch continues,
because a file nothing needs any more must not fail a launch. What it costs you:
that warm `target/` and registry are gone, and the workspace image's own tree is
what replaces them.

## Reclaiming the `/nix` upper

The writable `/nix` upper lives inside a disk image, so cang collects it in a
short maintenance VM rather than on the host.

A `--workspace-disk` launch keeps its generations inside that image
(`/workspace/.cang/nix/`), so collecting them names the same disk the launch did:

```bash
./result/bin/cang nix gc --workspace-disk ./work.raw --dry-run  # what would go, and its size
./result/bin/cang nix gc --workspace-disk ./work.raw            # collect everything but the cache in use
./result/bin/cang nix gc --workspace-disk ./work.raw --all      # collect that too, so the next launch starts empty
```

`--workspace-disk` is required: the build disk that used to hold the upper
without one is retired (see "The retired build disk" above), so a run without the
flag refuses by name instead of collecting nothing.

That image is *validated*, never created: `gc` on a path with no image is refused
rather than formatted, and it attaches the disk read-write only long enough for the
maintenance VM to delete whole generations.

One generation directory per image digest keeps its cache. `gc` keeps the
generation this workspace used last and collects the rest; `--all` collects it as
well, which costs a re-substitution of anything the image does not ship on the next
launch. The guest prints each generation it keeps or collects with its size, and a
`--all` run is the only way to reclaim the space of the image you are currently
running.

The maintenance VM is booted from the image, so a `cang-guest-init` that predates
this subcommand cannot run it: reload the image (or pass
`--guest-init <path-to-a-newer-cang-guest-init>` on the launch that builds it)
before using `gc`.

## Resizing a workspace image

```bash
./result/bin/cang disk resize --workspace-disk ./work.raw 100G
```

This is the only resize that acts on a path you named rather than on a state-root
disk, so it validates the image (a regular file, btrfs, already there) instead of
creating it: creation belongs to the launch, and a resize on a typo must not
`mkfs` over it. It is also what a one-shot launch leaves to you, since only
`cang workspace serve` grows an image by itself (see "A workspace on its own
disk").

`SIZE` accepts the same bytes or binary suffixes as the container-store command
(`K`, `M`, `G`, `T`, `KiB`, `MiB`, `GiB`, `TiB`), must not be smaller than the
image's current size, and an equal size is accepted: a run that grew the host file
and then failed in the guest is retried by rerunning the same command. It grows
the host sparse file first, then starts one narrow one-shot direct-libkrun
maintenance VM and runs `cang-guest-init internal resize workspace`, which mounts
the image and expands its btrfs filesystem to the new device size. The
`cang workspace serve` auto-grow takes the same two halves, so a grown image needs
nothing extra: the next mount finds the new device size and expands to fit.

The extra space appears in the guest only after the guest's btrfs filesystem has
been expanded; if that guest step fails, the host image stays grown and the error
names the rerun command.

Like the container-store command, `resize` prepares the maintenance VM's root on
the host, and that step bind-mounts and so needs `CAP_SYS_ADMIN`: run it as root
or inside a rootless user namespace, for example
`podman unshare -- cang disk resize --workspace-disk ./work.raw 100G`.

## Guest-init override

For cang guest-side debugging, `--guest-init <host-binary>` validates the
host binary as an executable regular file, discovers the image's existing
`/nix/store/.../bin/cang-guest-init`, and bind-mounts the host binary
read-only over that exact in-image target. Cang still execs the discovered
`/nix/store/.../bin/cang-guest-init` guest path and preserves the same
`CANG_*`, `KRUN_CONFIG`, arguments, and final guest command; it does not copy
or chmod the task-rootfs `/nix/store` file.
