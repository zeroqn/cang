# The build disk is retired, resize moved, and the workspace image grows itself

2026-11-21. Ticket 27, implemented and verified independently (my own arms, not
only the child's). Validation on this tree: `fmt --check`, `clippy
--all-targets --all-features -D warnings`, `cargo deny check`, `cargo test`
(**1 336 tests, 0 failures**), `nix build .#cang .#cang-musl .#container`, image
loaded as `localhost/cang:latest 3f8fba598267`.

## What changed

1. **No launch creates or attaches `cang-build.raw` any more.** `target/` is
   cargo's own `./target` in the tree and the cargo home is the host's `~/.cargo`
   mount; `CARGO_TARGET_DIR`/`CARGO_HOME` are no longer exported.
   `persistent_disks/build.rs` is deleted; its two surviving roles moved to
   `retired_build_disk.rs` (the reclaim) and `raw_btrfs.rs` (the grow).
2. **An existing build disk is reclaimed at launch**, once, under an exclusive
   `flock` (busy means "leave it alone, silently"), never outside the state root.
3. **`cang disk resize --build` is retired**: it parses, and refuses by name
   pointing at the replacement. The new surface is
   `cang disk resize --workspace-disk PATH SIZE` (positional size), sharing the
   same maintenance-VM growth the container store uses; `--containers` is
   unchanged.
4. **The workspace image grows itself on `cang workspace serve`.** The guest
   records `cang-workspace-free-bytes<TAB>free<TAB>total` in
   `/workspace/.cang/free-bytes` every time it mounts the disk; a serve pays one
   short read-only maintenance VM to read that marker **before** it mounts, and
   grows when free is under 10 % while the host keeps 2× headroom for the step
   (twice the space in use plus 16 GiB). A one-shot `--workspace-disk` launch
   deliberately does *not* auto-grow - that would add a VM boot to every launch -
   and the refusal line names the manual command.

## My arms (binary /nix/store/vhxvlrw929jk56im1fsbhyvv7ki47ywp-cang-0.22.0, image 3f8fba598267)

**(a) reclaim**, on a real state root holding a planted 96 GiB sparse build disk:

```
cang: reclaimed 96.0 GiB from '/home/dev/disp/cang/ws/cang-build.raw' - a warm
target/, the cargo home and the retired /nix upper lived there; they now live in
the workspace image's tree, so cargo's ./target and the tree's cargo home replace
it and cang no longer creates or attaches a build disk
```

and that directory afterwards holds only `cang-containers.raw`, `cargo`, the
`nix-overlay` bookkeeping and `tasks/` - no `cang-build.raw`. The launch itself
completed (`launch-ok`, `/workspace` = fuse).

**(b) auto-grow on serve**, image on a filesystem with headroom:

```
before: 2147483648
after:  21474836480
cang: grew the workspace image from 2.0 GiB to 20.0 GiB because it had 0 bytes free
guest: /dev/vdb  20G  6.1M  20G  1% /workspace
```

The same arm on a filesystem without headroom refuses instead, naming both
numbers, the headroom it needed and the manual command:

```
cang: did not grow the workspace image '/tmp/v27/b.raw' from 2.0 GiB to 20.0 GiB
because it had 0 bytes free (under 10% of the image) but the host has only 8.8 GiB
free, under the 2x headroom the 18.0 GiB step needs; free some space, or grow it by
hand with `cang disk resize --workspace-disk /tmp/v27/b.raw 20.0 GiB`
```

A marker above 10 % prints nothing and leaves the file untouched (2 GiB before and
after).

**(c) resize surface**: `--build 100G` refuses by name; `--workspace-disk 8G` grew
a 2 GiB image to 8 589 934 592 bytes and the guest reports `blocks=2097152
size=4096` = 8 GiB (`btrfs` on `/workspace`), i.e. the guest expanded the filesystem.

**(d) `--containers`** still resizes (`resized-container-store … 69793218560`). A
shrink request is refused by name first (`requested … 4294967296 bytes must be
greater than current size 68719476736`).

## Defects found and fixed while verifying

- **A duplicated unit in the refusal line**: `human_bytes(0)` is `"0 bytes"`, and the
  message appended another `bytes`, printing "the host has only 0 bytes bytes free".
  Fixed, and the test now asserts the rendered number rather than a paraphrase.
- **The child's auto-grow tests booted a real maintenance VM** (the read path
  materialises a task rootfs through Buildah), so they failed on the host rather than
  testing the policy. `auto_grow` is now split: `read_free_bytes` (VM) and
  `decide_and_grow` (pure policy with the grow step injected), and the tests drive
  the second one.
- **The marker's format is the whole contract** (`cang-workspace-free-bytes<TAB>free<TAB>total`,
  the file *is* the report). My first arm planted `0\n524288` and auto-grow
  correctly read "no marker": worth knowing before anyone hand-writes one.

## Still open

- Auto-grow runs only for `workspace serve`; a long-lived launch that never serves
  still relies on `cang disk resize --workspace-disk`.
- Deleting a user's build disk is irreversible by design (bob's decision): a warm
  `target/` and registry in it are lost, and the reclaim line says so.
