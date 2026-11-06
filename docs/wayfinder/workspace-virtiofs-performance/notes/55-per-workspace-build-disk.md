# Note 55: the per-workspace build disk, and what `.cargo` was costing

2026-11-06. Follows note 54. Two disks were proposed; this one is built and
measured, the global sccache disk is not (see the end).

## What was on the share that should not have been

`/proc/mounts` in a cang guest: `/` itself is **virtiofs** (`/dev/root`), and
`/workspace` is the workspace share. Everything a build touches therefore crosses
FUSE unless something explicitly mounts a disk:

| path | before |
| --- | --- |
| `/workspace` (sources) | virtio-fs |
| `/home/dev/.cargo` (registry, git) | virtio-fs - the `cang-cargo` share of `<state>/cargo` |
| `/home/dev/.cache/sccache` | virtio-fs |
| `target/` | whatever `CARGO_TARGET_DIR` said, i.e. the share |

So a build read its dependency sources through FUSE as well as writing its
artifacts there - the `READ` 22.3 % and `LOOKUP` 12.3 % of note 52's histogram.

## The disk

One per-workspace btrfs image (`<state>/cang-build.raw`, 96 GiB, id `cang-build`,
label `CANG_BUILD`), attached as virtio-blk, mounted by the guest at
`/home/dev/.cache/cang-build`, with both cargo working sets on it:

- `CARGO_TARGET_DIR=/home/dev/.cache/cang-build/target`
- `CARGO_HOME=/home/dev/.cache/cang-build/cargo`

On the first task of a workspace the guest seeds `cargo/` from the image's
virtio-fs cargo home (`cp -a`, 1.5 GiB, once), so the first build is not a
re-download. A failed mount removes both variables, so a build falls back to the
share instead of filling a rootfs directory that dies with the task.

A btrfs *subvolume* cannot be attached as a block device - virtio-blk needs a file
with its own block address space - so this is a raw image with btrfs inside, which
is also what lets the guest mount it independently.

## Verified in a guest

```
CARGO_TARGET_DIR=/home/dev/.cache/cang-build/target
CARGO_HOME=/home/dev/.cache/cang-build/cargo
/home/dev/.cache/cang-build /dev/vdb btrfs rw,relatime,discard=async,noacl,...
/dev/vdb   96G  1.4G  93G   2% /home/dev/.cache/cang-build
cached crates: 1023          size: 1.5G
```

Warmth across tasks, on a fresh test workspace: first task 75 s and 11 crates
downloaded, second task (a new VM, same disk) 22 s with the same 11 crates and the
same 69 MB `target/` - nothing re-fetched, nothing rebuilt from cold.

Three concurrent worktrees of the 23-crate workspace, `JOBS=2`, `--mem 15`
(the `shared` layout: one `CARGO_TARGET_DIR` for all three):

| | result |
| --- | --- |
| wall | **701 s**, all three rc=0 |
| `target/` on the disk | 11 GiB total |
| sccache | 77 hits / 1318 misses |
| host | avail 7.0 GiB, `kswapd0` 2.1 %, **no swap-out**, iowait 0 |
| guest fork+exec | 33 ms at rest, 480 ms under the storm |
| guest 4 KiB share read | 57 ms at rest, 161 ms under |

Before this, the same three-worktree shape on the share failed outright twice
(`rayon-1.12.0/src/iter/filter.rs` not found - three cargos extracting the same
crate into the one shared registry at once) and took 1152-1161 s when it did
finish, against **285-288 s** for the same three worktrees at `JOBS=4` with
`target/` on a virtio-blk disk. The registry race is another reason `.cargo`
should not be one shared writable tree: with this disk each workspace has its own.

Caveat, measured: one `CARGO_TARGET_DIR` shared by three worktrees makes cargo
serialise on its `.cargo-lock` and re-fingerprint per worktree path, so the
per-worktree layout (`target/par1`, `target/par2`, `target/par3` under the same
disk) keeps both warmth and parallelism. Both layouts were run; the numbers above
are the shared one, and the choice is a policy question for the workspace owner,
not a limitation of the disk.

## What is still on the share

- `sccache` - deliberately: a *global* cache shared by concurrent guests cannot be
  one btrfs image (two guests mounting one btrfs read-write corrupts it). The
  options are a primed image cloned per task with a merge on exit, or an exclusive
  lock, or leaving it on virtio-fs as it is now. Not started.
- the sources themselves - that is the point of the share.
