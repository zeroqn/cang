# /workspace on a btrfs block device - live verification and the write-path numbers

2026-11-20. Ticket 18 (`tickets/18-workspace-target-on-block.md`). Artifacts:
`nix build .#cang` = `/nix/store/7i1kgpqsbcyyjgnq8kdbp8a41fbnsw7s-cang-0.22.0` (first
run), rebuilt several times as the guest half changed; the last binary used is
the one built after the bind-order fix (see "what the arms exposed"). Guest-init
override: `nix build .#cang-musl` -> `/nix/store/ms4w1brvx2xh7cl7jgwpjvi3maxqp54h-cang-static-x86_64-unknown-linux-musl-0.22.0`
(the loaded `localhost/cang:latest` image predates this change, so every arm
passes `--guest-init`). No VM-free claim is used for any number below.

## What the flag does (as built)

`--workspace-disk PATH` (+ `--workspace-size SIZE`, default 64G):

- the host creates a sparse image at PATH on first use and formats it
  `mkfs.btrfs -L CANG_WORKSPACE`, or validates an existing one: not btrfs ->
  refuse (no reformat), symlink/directory -> refuse, smaller than the requested
  size -> refuse naming `truncate -s`;
- `block_device_new("cang-workspace", path, false)` is the attachment; the guest
  finds it by btrfs label, mounts it at `/workspace`, chowns it to the developer
  identity, creates `cargo/` inside the tree, seeds it once from the registry the
  launch mounted at `~/.cargo`, then binds `cargo/` at `~/.cargo`;
- no `/workspace` virtio-fs bind mount and no build disk (`cang-build.raw` is
  neither created nor attached) - `target/` is cargo's own `./target` in the tree;
- `/nix` stays the host-composed overlay; `--nix=lower:blk` with this flag is
  refused at plan time (one block disk per launch, not two).

## The live arm

```
/tmp/cang-host/bin/cang --workspace-disk /tmp/ws-test.raw --workspace-size 4G \
  --guest-init /tmp/cang-musl/bin/cang-guest-init -- bash -lc "<probe>"
```

Observed in the guest (verbatim from the last run):

```
NIX:
/nix none virtiofs
WS:
/workspace /dev/vdb btrfs
CARGO:
/home/dev/.cargo none virtiofs          <- the launch's registry mount, not yet covered
/home/dev/.cargo /dev/vdb[/cargo] btrfs <- after the guest step: the tree's cargo/
DISKS:
/dev/vda /dev/vdb
BUILDDISK:
none                                    (findmnt /home/dev/.cache/cang-build)
WRITE:
/home/dev/.cargo/probe-file
/workspace/cargo/probe-file             (a write through ~/.cargo landed in the tree)
```

Earlier run of the same flag, with a build inside the tree:

```
+ cd /workspace && cargo new --vcs none p && cd p && cargo build --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.35s
+ du -sh /workspace/p/target
4.5M	/workspace/p/target
+ stat -f -c ws_fs=%T /workspace
ws_fs=btrfs
```

Host-side refusals (no VM started, sub-second):

```
$ cang --workspace-size 4G -- true
cang: --workspace-size needs --workspace-disk: it sizes the image that flag names
$ cang --workspace-disk /tmp/garbage.raw -- true         # 4 GiB of /dev/urandom
cang: failed to prepare the cang workspace disk: existing cang workspace disk
'/tmp/garbage.raw' is 4294967296 bytes, below the requested 68719476736 bytes; ...
$ cang --workspace-disk /tmp/ws-test.raw --nix=lower:blk -- true
cang: --workspace-disk cannot be combined with --nix=lower:blk: ...
```

## The write-path numbers

Same archive (`/tmp/bench/repo.tar`: this checkout, 342 MB, 2136 entries,
`.git`/`target`/`.scratch` excluded), extracted twice into the workspace by the
same script (`tar -xf /mnt/bench/repo.tar -C <dir>`, archive read from a `-v`
read-only volume so the source path is identical), `rm -rf` between runs:

| arm | run 1 (cold) | run 2 (warm) |
| --- | --- | --- |
| `/workspace` = btrfs on virtio-blk (`/dev/vdb`) | 844 ms | **496 ms** |
| `/workspace` = host directory over virtio-fs | 14 506 ms | **14 290 ms** |

~29x on the warm run. This is the "what a build writes never crosses FUSE"
claim, not a compile measurement: the cold 2401-crate build arm is still open
(it needs a warm registry in the image and more host disk - see below).

## What the arms exposed (fixed, not papered over)

1. **`/home/dev/.cargo` must not be a pre-existing mount to be bindable.** The
   first version skipped the bind when `findmnt` said the path was already
   mounted, which is exactly the launch's registry mount, so `~/.cargo` stayed
   the host's empty directory. Now the bind always runs (it covers the mount).
2. **Prepared-root validation listed `~/.cargo`**, which a workspace-disk launch
   does not have grafted by the host; it is filtered out of the expected set for
   this mode, and a guest that cannot mount the disk is fatal rather than
   degrading to "empty `/workspace`" (which would silently measure nothing).
3. **The seed source matters.** The image ships no `/home/dev/.cargo`
   (`podman run --rm --entrypoint /bin/ls localhost/cang:latest -la /home/dev/.cargo`
   -> no such file), so the launch's `state-root/cargo` mount is the only
   registry source; keeping that mount for this mode is what makes the one-time
   seed possible. In this state root it happened to be empty, so the seed copied
   nothing.

## Open / next

- Reverse share (tickets 22/23) is what makes the launch directory show the
  guest tree; today it shows its own contents, which is why this is called a
  measurement surface in the help text.
- The cold-build arm needs a registry claim and >= ~10 GiB proposer headroom;
  at the time of these arms `/home/dev/disp` was 98% full (3.3 GiB free), which
  is also why the *share* arm printed "/nix block lower unavailable ... only
  3290 MiB are available" and ran on the overlay.
- The share arm's 14.3 s is much worse than the 8.2 s compile-test iteration in
  the map: that figure is a *compile* on a warm target, this one is 2136 file
  creations of 342 MB into the share.
