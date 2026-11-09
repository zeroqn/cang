---
label: wayfinder:task
title: What does the writable /nix upper cost today, and what would the block disk save?
status: closed
blocked_by: []
claimed_by: "pi task child (2026-11-09)"
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

**Measured 2026-11-09 in four boots and three arm sets; the only uncontended
data is a four-arm pair taken after a co-tenant left the host.** Note:
[`../notes/01-upper-write-cost.md`](../notes/01-upper-write-cost.md).

Arms: a Nix chroot store (`nix copy --to local?root=...`) written through kernel
overlayfs over the same read-only `/run/cang/nix-lower`, differing only in the
upper's filesystem - **A** = the host directory share over virtio-fs (today),
**B** = btrfs inside `cang-build.raw` on the guest-local block disk. Verified from
the guest's `/proc/mounts` (`fuse`/`65735546` vs `btrfs`/`9123683e`).

**Clean pair (admissible, 2 pairs, alternating lead, page cache cold, 684 paths /
7.98 GB):**

| metric | A (virtio-fs upper) | B (block-disk upper) | B vs A |
|---|---|---|---|
| **store write** | 111 177 ms median (109 850 / 112 504) | 99 105 ms median (105 483 / 92 728) | **10.9 % faster** (paired -4.0 %, -17.6 %) |
| metadata walk, all 49 066 files | 13 907 ms | 1 813 ms | **7.7x faster** |
| `du -s` of the written store | 38 520 ms | 2 508 ms | **15.4x faster** |
| delete the written tree | 55 184 ms | 4 165 ms | **13.3x faster** |
| cold re-read, 40 largest files (3.85 GB) | 21 147 ms | 23 781 ms | 12.5 % slower |
| `sync` after the copy | 28 ms | 168 ms | the FUSE upper defers its commit |

**Verdict against this ticket's threshold (`>= 20 %` off arm A on the
store-write workload): NOT CLEARED - and overridden by bob, see below.** The clean estimate is **10.9 %**, i.e. half
the threshold and inside the spread of the three contended sets, whose write
ratios were +24.7 %, -6.5 % and +14.5 % (dead-heat, sign-unstable; kept as a
discarded appendix, not comparable). A write-throughput argument for moving the
upper does not exist in these data.

What the move *does* buy is metadata and garbage collection, now confirmed
uncontended: walk 7.7x, `du` 15.4x, delete 13.3x, so a write-then-delete cycle is
166.4 s on A against 103.3 s on B (38 % cheaper). Ticket 02's conclusion that a
btrfs upper is the *less* constrained upper is visible in the same run: the
virtio-fs upper logs `overlayfs: upper fs does not support tmpfile`, the btrfs one
does not.

**Two limits this ticket could not remove, both stated in the note:**

1. **Shape.** The frozen workload is bandwidth-shaped at the path level -
   685 paths / 8.26 GB = **12.06 MB per path**, with 259 paths carrying 98.6 % of
   the bytes, against the named target (harmony upper: 2 501 paths / 3.0 GB =
   ~1.2 MB per path). The requested metadata-shaped arm was attempted and **cannot
   exist with this image**: the smallest paths that fit 3 GB expand through
   `nix path-info -r` to 684 paths / 7.98 GB (references pull the big toolchains
   back), and the store holds only 685 paths, so no closed ~3 GB subset can be
   2 500-paths-shaped. The clean pair is therefore an honest number for a
   bandwidth-shaped store write, not for a real user's upper.
2. **Contention.** Three sets were voided: `FOREIGN_VMS=2` throughout run 1
   (140/140 samples) and run 3's six arms (240/241, 14:10:55-14:31:39 +0800), and
   from 96 s into run 2 (128/128). The admissible four arms are the ones that ran
   after that campaign left at 14:31:39. A contended set is worse than none: arm
   B's own write ranged 81.9 s - 172.9 s across those windows.

Caveats travelling with the numbers: the loaded image `localhost/cang:latest`
(`aeceb975...`) was reused, not rebuilt (`.#container` unrealised, `/nix/store` at
6.9 GiB free), with `--guest-init` from this tree's `.#cang-musl` because the
image's own guest-init (0.17.2) predates block mode and the build disk - so these
arms measure filesystem behaviour, not HEAD's guest-init; nothing was built,
loaded, pruned or deleted outside this session's own arm directories.

Raw logs: `/home/dev/disp/nix-upper-cost/{logs,results*.txt,ws/evidence}`.

**Decision (bob, 2026-11-10): the cycle is the metric, not the bare write.** The
bar was set on store-write wall time before any data existed; the data then showed
the write is bandwidth-bound on both filesystems (12.06 MB per path) while the
costs a `/nix` upper actually pays every day - walking the store, `du`, deleting a
generation - are 7.7-15.4x cheaper on the disk, i.e. a **37.9 % cheaper
write+delete cycle**. bob's call: take the cycle as the win, record the override
and its reason here so it is not quietly re-litigated, and unblock the design
tickets. The 12.5 % cold-re-read regression on the block upper is carried into
those tickets as a cost to weigh, not dropped.
