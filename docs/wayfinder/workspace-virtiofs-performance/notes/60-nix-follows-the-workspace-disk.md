# `/nix` follows the workspace disk: the `--nix` axis removed, the upper moved into the tree

2026-11-20. Ticket 26 (`tickets/26-retire-nix-block-disks.md`), in the shape bob
corrected it to: *"`--nix` cli argument can be removed. workspace raw automatically
mean `/nix` block lower. Otherwise old method virtiofs and overlayfs in host then
mount to `/nix` in cang guest is used."* Answering his three questions first, then
what was built and what could be measured.

## Q1 - may the launch's `~/.cargo` virtio-fs mount be unmounted?

Yes, and it now is. The guest seeds the tree's `cargo/` from it on the first boot of
a workspace image, then `umount /home/dev/.cargo`, and only then binds
`/workspace/cargo` there - so exactly one filesystem sits behind the path instead of
two stacked mounts. Live: `findmnt -rno TARGET,SOURCE,FSTYPE /home/dev/.cargo` prints
a single line, `/home/dev/.cargo /dev/vdb[/cargo] btrfs`, where before the change it
printed the virtiofs mount and the btree bind on top of it.

## Q2 - "make sure that when `/nix` lower is changed, the merged code still work correctly"

That is the generation-keyed upper, which was already the shape in the guest:
`/workspace/.cang/nix/<digest-key>/` is a btrfs *subvolume*, `upper`/`work` are plain
directories inside it, `.cang-upper-seeded` is written last, a new digest is a
`btrfs subvolume snapshot` of the newest completed sibling, and the image's own Nix
DB is staged from the read-only lower for the post-mount reconcile. A lower change
therefore cannot reuse the old generation's cache by accident: the sentinel lives
*inside* the generation directory, so a different digest key starts a fresh one.

## Q3 - can `upper` be the child of a subvolume root (`/workspace/.cang/nix/`)?

Yes, and that is the only legal shape. A btrfs **subvolume root** as overlayfs
`upperdir` fails every copy-up with `EXDEV`; a plain directory *inside* a subvolume
is what overlayfs wants, and making the generation directory itself the subvolume is
what keeps a snapshot cheap (`btrfs subvolume snapshot` is O(1), and a nested subvolume
would not be copied into a snapshot at all). The snapshot-rotate idiom bob described
works on that layout; nothing in this change forces a rotation policy.

## What was removed / changed

- **The whole `--nix` surface is gone**: flag, `NixLayer`/`NixLayerMode` grammar,
  `parse_nix_arg`, `resolve_nix_layers` and its pair-wide rules, `NixLowerMode`, and
  the `--workspace-disk` + `--nix=lower:blk` refusal that existed only to refuse the
  flag. `cang --nix=lower:blk` is now a plain clap error.
- **The mode is derived**: `LaunchPlan.nix_block_lower = workspace_disk.is_some()`;
  `prepare_nix_lower_block(intent, guest_init, want_block_lower: bool)`.
- **The upper moved from the build disk to the workspace tree**:
  `WORKSPACE_UPPER_ROOT = "/workspace/.cang/nix"`, the host creates nothing there
  (it cannot mount the image), the host graft of the generation directory
  (`bind_mount()`) and its `CANG_NIX_LOWER_IMPORT_UPPER` are deleted, and the guest's
  `is_on_build_disk` (which decides "seed it yourself") now also accepts paths under
  `/workspace`.
- **A launch that cannot get the block lower no longer refuses**: with no flag left
  to ask for a refusal, it keeps the host-composed overlay and names the reason on
  stderr (`cang: /nix block lower unavailable, keeping the host-composed overlay:
  <err>`).
- **`cang disk reset` cannot reach a workspace image**: it only offers `--containers`
  (and `--force`), asserted by a new CLI test that pins both `disk` subcommands'
  option lists. A `--workspace-disk` path is the user's, not cang state.

## The live arm (and what it could not measure)

```
/tmp/cang-host/bin/cang --workspace-disk /tmp/ws-test.raw \
  --guest-init /tmp/cang-musl/bin/cang-guest-init -- bash -c "<probe>"
```

```
cang: /nix block lower unavailable, keeping the host-composed overlay: could not
build the /nix lower image: building the /nix lower image needs about 8398 MiB free
but only 3240 MiB are available
NIX:    /nix none virtiofs
CARGO:  /home/dev/.cargo /dev/vdb[/cargo] btrfs
WS:     /workspace /dev/vdb btrfs
DISKS:  /dev/vda /dev/vdb
UPPER:  /workspace/.cang/nix absent (only created when the pair is served)
```

So: the mode is derived live (the host *tried* the block lower for a workspace-disk
launch), the cargo mount is single, the workspace is on the disk, no build disk.
**The guest-composed `/nix` overlay itself was not exercised**: the host is at
3.24 GiB free against the build's ~8.4 GiB floor, so the lower could not be built
this window. That is the one claim this note does not make.

## Known gap

`cang nix gc` still attaches `cang-build.raw` and collects generations there, so a
workspace-disk launch (whose generations are in the tree) collects nothing; recorded
in `docs/usage.md` and ticket 26.
