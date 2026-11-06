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

That 701 s is **three cold builds racing**, not three warm ones: the per-worktree
logs show 387 / 397 / 391 `Compiling` lines, so each worktree compiled the whole
graph itself and the last writer won in the shared directory. serially, with the
cache already populated, the same worktree is far cheaper - one worktree, target
wiped, built twice in a row:

| | wall | sccache |
| --- | --- | --- |
| first wiped-target build | 393 s | 1375 hits / 20 misses (**98.6 %**) |
| second | 242 s | 2770 hits / 20 misses (**99.3 %**) |

So sccache does what it says as long as the compiles are **sequential**: the cache
had this configuration from earlier runs, and a wiped target still hit 98.6 %. The
concurrent arm missed 1318 of 1395 requests because three builds started together
and each crate was in flight in three jobs at once - sccache has no in-flight
deduplication, so every duplicate is a miss. Ordering a warm-up build before the
others, or building worktrees in sequence, is what converts that to hits; it costs
one build, and only the first round of a fresh tree pays it.

Even at 99 % hits a wiped-target build of the workspace costs 242 s, because cargo
still invokes rustc 811 times: sccache removes the codegen, not the process or the
artifact copy. What the disk removes is the round trips those 811 invocations make
- which is why the same shape on the share was 464 s (note 52) and 1152-1161 s for
three worktrees.

A single shared `CARGO_TARGET_DIR` for three worktrees also serialises them on
cargo's own `.cargo-lock` (10 `Blocking waiting for file lock on package cache`
messages in that arm) and re-fingerprints the local path crates per worktree; the
per-worktree layout (`target/par1`, ... under the same disk) avoids both. That is a
policy choice for the workspace owner, not a limitation of the disk.

## What is still on the share

- `sccache` - deliberately: a *global* cache shared by concurrent guests cannot be
  one btrfs image (two guests mounting one btrfs read-write corrupts it). The
  options are a primed image cloned per task with a merge on exit, or an exclusive
  lock, or leaving it on virtio-fs as it is now. Not started.
- the sources themselves - that is the point of the share.

## Does `SCCACHE_BASEDIRS` buy anything for Rust? Measured: no

sccache's own docs (`docs/Configuration.md`) describe it as the ccache-style
lever: "base directory (or directories) to strip from paths for cache key
computation... enables cache hits across different absolute paths when compiling
the same source code", separated by `:` on this platform (a first attempt used
commas, which is the Windows spelling, and tested nothing).

Measured with it and without, same sources at two different paths
(`/workspace/ap1`, `/workspace/ap2`), separate target dirs, a real dependency
graph (serde_json + rayon), cache warm, server restarted per arm:

| arm | build 1 | build 2 (different path) |
| --- | --- | --- |
| `SCCACHE_BASEDIRS` unset | 22 requests, 11 hits | 44 requests, 22 hits (+11) |
| `SCCACHE_BASEDIRS=/workspace/ap1:/workspace/ap2` | 22 requests, 11 hits | 44 requests, 22 hits (+11) |

Identical. The 11 hits the second path got without it are the *registry*
dependencies - their sources live in the shared `CARGO_HOME`, and sccache already
normalises `--out-dir`, so a different worktree path does not stop them hitting.
What cannot hit is the workspace's own crates: cargo derives `-C metadata` from
the package's source path, so the argument vector differs before sccache sees it,
and a `bin`/`cdylib`/`proc-macro` crate is not cacheable at all (it invokes the
system linker). `SCCACHE_BASEDIRS` cannot rewrite an opaque metadata hash, so it
changes neither.

Conclusion: leave it unset. It also has a failure mode worth avoiding - a path
sccache does not like prevents the server from starting, which turns a cache
optimisation into a broken build.
