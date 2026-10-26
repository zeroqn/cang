---
label: wayfinder:prototype
title: Caching for /workspace: auto is already on, writeback is rejected
status: closed
blocked_by: []
claimed_by: pi + bob (2026-10-26)
---

## Question

bob's decision (2026-10-26) reopens what ticket 03 closed: caching may exist, but
**only for `/workspace`, only opt-in, and the launch must print a warning that
touching the mounted workspace from the host can corrupt what the guest sees**.

Facts that constrain the shape:

- FUSE cache options are per **connection**, and cang has exactly one virtiofs
  connection mounted as `/` - so "cache only /workspace" cannot be a flag on the
  existing share. It needs a **second virtiofs device** whose tag is mounted at
  `/workspace` with its own options (`/nix`, `~/.cargo`, sccache stay uncached on
  the root share).
- The guest currently bind-grafts the host workspace into the prepared root
  (`crates/cang/src/runtime/vm/prepared_root/mod.rs`); a second device means the
  guest mounts `cang-workspace` at `/workspace` instead, and the root share no
  longer needs to carry it.
- The fork's per-device surface is `FsDevice` + `krun_fs_device_*` and
  `crates/cang-libkrun/src/linked.rs::fs_device_new`; a cache mode has to be
  expressible there (see the cache-semantics note for what "auto" vs "always"
  costs).

Decide, on the cache-semantics research (`notes/13-cache-semantics-and-scoping.md`):

1. **Which mode.** `cache=auto`-equivalent (attribute/entry timeouts, guest page
   cache for reads, writes still synchronous) or `cache=always`-equivalent
   (writeback cache: writes land in the guest and reach the host late). State
   exactly which FUSE_INIT flags each one sets in this server.
2. **The opt-in surface.** A `cang --workspace-cache=<mode>` flag (or
   `[workspace] cache =` in `cang.toml`), default off, and what the flag does to
   the *device* and to the *mount*. Note the flag is per run, so two runs in
   different modes never share a VM.
3. **The warning.** Exact text and where it prints (on launch, on stderr, and
   repeated in the docs): guest writes may reach the host late or never, a host
   process touching `/workspace` while a cached guest runs can corrupt the tree
   the guest sees, and a killed VM can lose writes. Include what the guest sees if
   the host edits a file mid-build (stale read vs torn read).
4. **What must be proved before it ships.** The three-arm benchmark with the flag
   on (`guest-virtiofs` arm gains a `cached` variant) showing the win over the
   uncached arm and against the `/dev/shm` control, plus the four regression gates
   from ticket 03 run *with the flag on* where they still make sense, and one run
   with the flag on that deliberately exercises the corruption case to document
   what a user sees (a copy of the tree, never a real workspace).
5. **Split of work.** Which part is cang-side plumbing (device, mount, flag,
   warning) and which is fork work (a `set_cache_mode` on `FsDevice`), remembering
   that a fork change means a tag, a pin move in the submodule *and* `flake.lock`,
   and the live smokes (rule #223/#229).

## Resolution

(to follow)

## Resolution (2026-10-26, bob's verdict on note 13)

**No cache change ships from this ticket: `auto` is already the behaviour, and
writeback is rejected because a cang guest can exit ungracefully.**

- **`cache=auto` is a no-op here.** It is the *absence* of per-open `KEEP_CACHE`
  and `FOPEN_DIRECT_IO` flags plus per-reply timeouts - which is exactly what the
  fork does today. Nothing to enable.
- **`WRITEBACK_CACHE` is the only connection-wide lever, and bob ruled it out**:
  a cang guest can be killed, OOM-killed or lost with dirty pages still in its
  page cache, and there is no `FUSE_NOTIFY_INVAL_*` channel in this server, so the
  loss window has no bound the host can detect. It also suppresses mtime/size
  invalidation for **every** regular file on the connection (note 13), which makes
  the host's view stale even after the writes do land.
- **What survives** as a candidate is per-path and per-open, which needs no
  writeback: keeping `KEEP_CACHE` for read-mostly paths, or - bob's block-device
  question - putting only `target/` on a non-virtiofs device so sources stay live
  and build output stops paying FUSE round trips. The second one needs no cache
  semantics change at all, which is why it is the better next experiment (see the
  block-device ticket).

Recorded for the map: the launch warning bob asked for is only needed if a
caching mode ever ships; with `auto` there is nothing to warn about.
