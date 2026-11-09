---
label: wayfinder:task
title: What does the writable /nix upper cost today, and what would the block disk save?
status: open
blocked_by: []
claimed_by: ""
---

## Question

Every other ticket on this map hangs on one number that nobody has taken: the
cost of writing a nix store into the *virtio-fs* upper, against the same writes
landing on the guest-local btrfs disk this workspace already has. This ticket
does not build `upper:blk` - it measures the two filesystems cang can already
offer, so the decision to build it is made on evidence.

**The workload.** A store-shaped write set, not a synthetic `dd`: Nix's own store
paths are 0444 regular files, hardlink farms, dirs with sticky/group bits, and a
`.reginfo`/`nix-cache-info` bookkeeping tail; a build also rewrites and deletes.
Recommended shape: one pinned closure of 2000-2500 paths / about 3 GB (the
`harmony` workspace upper on this host holds 2501 store paths / 3.0 GB, so that
size is what a real user's upper looks like).

**The arms**, one guest, alternating order, at least 3 repetitions each:

- **A (today)** writes land in the real `/nix/store`, i.e. the host-directed
  overlay upper over virtio-fs.
- **B (candidate)** the identical writes land on the guest-local btrfs block
  disk. Two ways to get them there without touching cang; pick whichever holds
  up: (1) a Nix *chroot store* on the build disk -
  `nix copy --from /nix --to 'local?root=/home/dev/.cache/cang-build/armb' <closure>`
  or the equivalent `--store`, which copies bytes that already exist in `/nix`
  into a store rooted on the disk (network-free, and the source store is the
  image's read-only lower, so the closure must be one the image ships or was
  substituted once before the timed arms); (2) `CARGO_TARGET_DIR`-style: write
  the same tree with `tar`/`cp -a` into `/home/dev/.cache/cang-build` against a
  copy of the same tree in `/workspace` (a workspace write is the same upper
  filesystem as a `/nix` write) - cheaper to set up, a weaker proxy for the
  store's metadata tail. State which one the number came from.
- **C (control, optional)** the same writes into `/dev/shm`, the guest-local
  ceiling.

**What to record** per arm: wall time for the write, wall time for a re-read of
the same 40 files (`tools/nix-lower-cang-smoke`'s 1.7 GB cold-read set is a
ready-made reader), and `find /nix/store`-style metadata walk over the written
tree - the metadata cost is where a FUSE upper is expected to lose. Logs, the
exact closure, repetition order and the fresh-run check go in
`/workspace/evidence` and the note; quote raw log paths.

**Then decide.** State the win as a percentage of A, and judge it against a
threshold: recommended **>= 20 %** on the store-write workload, because that is
the scale at which a second filesystem, a migration and a seeding step are worth
their risk (the build disk's `target/` precedent was 4x). A win below the
threshold closes this map with the number recorded; a win above it unblocks the
design tickets.

Do not modify cang for this ticket. Two images, one guest each arm set, no
concurrent runs (the workspace's overlay is exclusive).

## Resolution

(to follow)
