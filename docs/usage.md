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
  a busy error instead of sharing the PTY.
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
duplicate guest targets are rejected. Cang intentionally does not support Podman SELinux
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
./result/bin/cang container-store resize --size 128G
./result/bin/cang container-store reset --force
```

These commands manage only the current workspace's `cang-containers.raw` disk
used by the raw-disk container store. They do not inspect or migrate any legacy
host-directory container store and do not resize or reset cang's host `/nix`
overlay state.
`resize` is grow-only: `--size` accepts bytes or binary suffixes such as `K`,
`M`, `G`, `T`, `KiB`, `MiB`, `GiB`, and `TiB`, and the requested size must be
larger than the current raw file. It grows the host sparse file first, then
starts a narrow one-shot direct-libkrun maintenance VM that runs
`cang-guest-init internal resize containers` to expand the guest btrfs
filesystem. If that guest resize fails after the host file has grown, cang does
not shrink or roll back the file; fix the reported VM/guest problem and rerun
the same resize command.

`reset` is destructive and requires `--force`. It refuses to run while the
current workspace has running, pid-reused, unreadable, or unscannable task
records, deletes an existing regular `cang-containers.raw`, and recreates the
default 64 GiB sparse btrfs image without launching a VM. Stale-only task
records are reported as cleanup information and do not block either command.
For a manual smoke test on a host with Buildah, btrfs-progs, and libkrun
available, run `cang --container-store raw-disk` once, then run the `resize`
and `reset --force` commands above.

## Guest-init override

For cang guest-side debugging, `--guest-init <host-binary>` validates the
host binary as an executable regular file, discovers the image's existing
`/nix/store/.../bin/cang-guest-init`, and bind-mounts the host binary
read-only over that exact in-image target. Cang still execs the discovered
`/nix/store/.../bin/cang-guest-init` guest path and preserves the same
`CANG_*`, `KRUN_CONFIG`, arguments, and final guest command; it does not copy
or chmod the task-rootfs `/nix/store` file.
