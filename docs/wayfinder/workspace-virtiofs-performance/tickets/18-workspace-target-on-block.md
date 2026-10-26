---
label: wayfinder:prototype
title: Put /workspace/target on a block device instead of virtiofs - does it close the 30%?
status: open
blocked_by: []
claimed_by: ""
---

## Question

bob asks whether a mounted directory can cross virtio-blk instead of virtiofs.
**Strictly no** - virtio-blk exports a block device, not a directory, so a host
tree can only reach the guest as a *filesystem image* (loop file, LVM volume, or a
real device) that the guest mounts, and coherence with the host tree then needs a
sync policy. Two shapes are worth distinguishing:

- **Whole `/workspace` as a btrfs/erofs image**: the guest gets a real page cache
  and parallel block IO (the fork's PR-865 read pool is already on: `ps` shows 8
  `block read work` threads), but the host stops seeing guest writes until a sync,
  which is strictly worse than the writeback cache bob just rejected.
- **Sources on virtiofs, `target/` on the block device**: sources stay live and
  coherent; the build's write-heavy, read-once output stops crossing FUSE. No
  cache semantics change, no corruption surface, and it can be measured with the
  existing harness by pointing `CARGO_TARGET_DIR` at the mounted block device.

Decide/do:

1. **Which shape** (whole tree vs `target/` only) is worth building, given the
   measured numbers: host 104 s, guest `/dev/shm` 232 s, guest virtiofs 299-314 s.
   The ceiling for `target/`-only is somewhere between 299 and 232 plus the
   sources' read cost - say what the arm would have to show to be worth the cang
   change.
2. **The cang change.** A plain guest mounts **no** containers disk today
   (`/dev/vdb` absent unless podman runs; `/home/dev/.local/share/containers` is
   not writable), which is why the guest-local arm uses `/dev/shm`. To measure a
   block device the launcher needs a second raw image + `block_device_new` +
   a guest mount (and a `cang --workspace-target=block` style flag, or
   `CARGO_TARGET_DIR` wired into the guest env). Name the smallest version.
3. **The sync question** if the whole tree is chosen: what writes the image,
   when, and what happens to host edits mid-build (deep-copy on launch? rsync
   back on exit? both directions?). Say plainly why `target/`-only avoids all of
   it.
4. **Measure it** with `tools/rust-compile-bench` (the `guest-virtiofs` arm gains a
   `target-on-block` variant) and record the number in the tool README, whatever
   the result - a negative result closes this axis for good.

## Resolution

(to follow)
