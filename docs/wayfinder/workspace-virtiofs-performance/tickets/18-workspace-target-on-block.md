---
label: wayfinder:prototype
title: Put the whole /workspace on a raw btrfs image - smallest cang change, and does it close the gap?
status: open
blocked_by: []
claimed_by: ""
---

## Question

bob redrew the destination on 2026-11-20 (see the map's `## Destination`): the
*whole* workspace tree - sources, `target/`, the cargo registry, worktree
subvolumes - becomes a btrfs filesystem on a block device the guest mounts at
`/workspace`, and the host gets its own live read/write view back through a
reverse share (tickets 22/23). This ticket is the **cang half**: the device, the
mount, the identity, and the first numbers.

**Decided (2026-11-20), do not re-litigate here:** the image is authoritative
(not a synced copy of the host tree); `target/` and `CARGO_HOME` live on it; the
`/nix` upper does not; the backing device is a raw image file in v1 (a real
`/dev/sdX` is ticket 28). The old objection in this ticket's first draft -
"the host stops seeing guest writes until a sync, which is strictly worse" - is
answered by the reverse share, which is why the whole-tree shape is now chosen
over `target/`-only.

Decide/do, one session:

1. **The smallest cang change.** A per-workspace raw image (reuse
   `raw_btrfs.rs:102/115/197`, `RawDiskSpec`) + `block_device_new`
   (`launcher.rs:526`) + a guest mount step that puts it at `/workspace` where the
   virtio-fs bind graft is today (`launcher.rs:450` is the share;
   `prepared_root/mod.rs` does the graft). Name the flag (`--workspace-disk=<path>`,
   creating it when absent), the default (off - no disk named means today's
   host-directory share), and the identity rule, now **settled by ticket 24**:
   the named disk is the workspace and the cwd becomes the reverse view of its
   content, so the state-root slug keeps only task roots, logs, leases and the
   guest hostname.
2. **What must refuse.** A launch that cannot mount its workspace image (missing
   image, wrong fs, an image another VM has open) must refuse by name, the way
   `--nix=lower:blk` does today - not silently fall back to the share, because a
   fallback would silently measure a different arm. Keep a
   read-only/`--workspace=<host dir>` escape for diagnostics only.
3. **Seed or empty.** The first launch of a checkout that has no image needs its
   tree inside one - either this ticket or ticket 25 does it; say which, and make
   the other depend on it.
4. **Measure it**, with `tools/rust-compile-bench` gaining a `workspace-on-block`
   arm, on the two recorded workloads: the cold 2401-crate build (virtio-fs
   baseline 116.3 s at `v2.0.0-cang.13`, guest-local floor 131 s vs host 130 s in
   the same window) and the compile-test iteration (8.2 s on the share). Record
   whatever the numbers are. A flat result closes the axis - do not chase a
   second FUSE knob afterwards.

Acceptance: a live launch whose `/workspace` is btrfs on the image (proved in the
guest: `findmnt /workspace`, `btrfs subvolume show`), plus one number per workload
against the recorded virtio-fs arm, in a note under `notes/`.

## Resolution

(to follow)
