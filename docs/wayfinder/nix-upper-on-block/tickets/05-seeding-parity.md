---
label: wayfinder:grilling
title: Seeding parity - does the guest reproduce the host's per-generation seeding, or reconcile instead?
status: open
blocked_by: ["01-upper-write-cost", "02-overlayfs-btrfs-upper"]
claimed_by: ""
---

## Question

Today, when a workspace moves to a new image digest, the *host* seeds a new
generation (`runtime/session/nix_overlay.rs:318,345,521,551`): clone the previous
upper (a `btrfs subvolume snapshot`, ~25 ms, 0 bytes), install the new image's
Nix DB at `.cang-image-db`, park the previous generation's DB at
`.cang-previous-db` for the guest to merge (`components/nix/root.rs`, `PROFILE_*`
`nix-prep:*`). A raw image cannot be mounted rootless on the host, so this step
must move into the guest - the question is *how much* of it moves.

- **Parity**: the guest, as root, snapshots the previous generation subvolume,
  installs the image's DB (already inside the read-only lower, because the lower
  is `mkfs.btrfs --rootdir=<rootfs>/nix`), parks the previous DB and merges it.
  An image move stays ~25 ms and nothing is re-substituted. Cost: a real
  seeding step in `cang-guest-init`, and its failure modes at boot.
- **Reconcile-only**: one persistent upper, no generations; the new image's DB
  is not installed, and the existing guest-side `cang-nix-store-repair` (already
  run every boot, `PROFILE_REPAIR_STORE`) reconciles the DB against the merged
  store. Much less code, but what does a Nix DB that predates the image's lower
  actually do - does `nix` re-substitute everything the image supplies, or does
  it see paths on disk it has no rows for and stay confused?
- **Hybrid**: keep generations but only re-register the new image's rows (a
  merge without the clone), or clone lazily.

Answer which shape, against the assertions `tools/nix-generation-smoke` already
makes the contract: the generation is a btrfs subvolume (inode 256), a store
closure verifies (`verify_bad=0`) and runs, the staged image DB is consumed, the
previous generation's DB is merged, the previous closure is still registered
after the move, `cang-nix-store-repair --dry-run` reports no
registered-but-absent and no present-but-unregistered, and a second boot is a
no-op. Also say what happens to the *host* overlay and its workspace lease
(`nix_overlay.rs:46`) once both axes are block - do they disappear for that
launch, or stay for parity?

## Resolution

(to follow)
