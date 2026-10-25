---
label: wayfinder:grilling
title: May the guest cache /workspace, given the host tree is live while the guest writes it?
status: closed
blocked_by: []
claimed_by: pi + bob (2026-10-25)
---

## Question

Ticket 01 narrowed the ship-first change to **WRITEBACK_CACHE**, and its whole
risk is coherence - which is a product decision, not a flag. `/workspace` is not
private to the guest: it is the host's own working directory, bind-grafted into
the single virtiofs export (`prepared_root/mod.rs:113`), watched by the host's
editors and `git`, and visible to cang itself. Today the fork advertises no
writeback cache (`cfg.writeback` false, `passthrough.rs:371`) and `/` is `rw`, so
a guest write lands in the host file immediately.

Settle, with bob:

- **What the contract actually is.** Is "the host sees guest writes immediately"
  a promise cang makes, or an accident of the current defaults? Name who would
  break: a host `git status`/`cargo check` during a guest build, an editor
  watching the tree, a second agent session, cang's own task bookkeeping.
- **What writeback costs.** Dirty pages live in the guest page cache and reach
  the host in <=1 MiB background requests; a guest crash or a `SIGKILL` of the
  VM loses them, and the host can read a partially-flushed file mid-build.
  Which paths may tolerate that? Is `target/` (disposable) different from
  sources (not), and can the split be expressed when `/workspace` is one
  directory inside one share - or does accepting writeback mean accepting it for
  the sources too?
- **The other half of the same question.** The same `cfg` also holds
  `attr_timeout`/`entry_timeout` (5 s virtio-fs default, `device.rs:88-93`): a
  tuned timeout serves stale metadata to the guest and lets the guest's own
  create/remove land behind the host's view. Ticket 01 ranked it third. Is a
  longer timeout acceptable where writeback is not (metadata only, no lost
  data), and where does it stop being safe?
- **Which escape hatch, if any.** A `cang --workspace-cache=writeback` opt-in, a
  flush on guest exit/shutdown, an `fsync`-on-demand contract documented for
  users, or nothing at all. Say what a user who gets it wrong sees - silently
  stale host files are worse than a slow build.
- **What would falsify "acceptable".** The concrete scenario that must still
  work after this decision (e.g. edit on the host, rebuild in the guest, commit
  from the host), so ticket 04 can name the check.

The answer decides whether writeback ships as a default, a flag, or not at all -
and therefore whether the first measurement in ticket 04 is writeback or the
worker pool.

## Resolution

(to follow)

## Resolution (2026-10-25, bob + pi)

**cang keeps exact host visibility: no write caching, ever - and that removes
writeback cache from the map entirely.** Decisions:

- **Writes are never cached.** `/workspace` is the host's own tree, so the
  promise is: a guest write reaches the host file as it happens, a host process
  can read what the guest just wrote, and a killed VM cannot lose writes. This
  is now a *stated* promise rather than an accident of the defaults, and it is
  the reason `WRITEBACK_CACHE` (ticket 01's first-ranked lever) is **out**.
  Rejected alternatives: an opt-in `--workspace-cache=writeback` flag, and
  writeback by default.
- **Metadata timeouts stay at the virtio-fs default 5 s.** The cheaper lever is
  not shipped either; it is to be *measured* against the new baseline, so a
  timeout change can only be decided on numbers later. Note the same reasoning
  applies to `CACHE_SYMLINKS` (caches symlink targets in the guest) and to any
  handle-reuse mode: exact visibility rules them out unless a measurement shows
  a stale-metadata variant is genuinely harmless.
- **What that leaves** as ship candidates: parallel FUSE dispatch (ticket 01's
  option 2 - a worker pool over the existing single request queue, no
  `VIRTIO_F_MULTIQUEUE`, no inode-map refactor), and DAX (ticket 05), whose
  `mmap` path is copy-avoidance for reads and does not delay writes - but see
  ticket 05's verdict before assuming it is compatible.
- **Regression gates, named by bob** (they belong to ticket 04's change and to
  the harness's README): a host edit followed by a guest rebuild followed by a
  host commit must see every file; after a build, a host-side `git status` must
  agree with what the guest wrote; a hard-killed guest must leave the host tree
  usable; and the existing smokes (Chromium/venus GPU, VA-API video, guest
  podman, `cang attach`) must keep working.

**Consequence for the frontier:** ticket 04 no longer chooses between writeback
and the pool - the pool (or nothing) is the first structural change, and the
map's expected win is now smaller and must be shown by ticket 02's numbers
rather than assumed from ticket 01's ranking.
