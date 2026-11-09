---
label: wayfinder:task
title: Implement `--nix=lower:,upper:` and the guest upper disk
status: open
blocked_by: ["01-upper-write-cost", "04-upper-home-and-auto-grow", "05-seeding-parity", "06-migrate-existing-uppers"]
claimed_by: ""
---

## Question

Build the feature, to the contract the closed tickets fixed:

- **flag** (the flag-surface ticket): `--nix=KEY:VALUE` repeatable, keys
  `lower`/`upper`, values `auto`/`blk`/`virtiofs`, `--nix-lower` and
  `CANG_NIX_LOWER_BLOCK` gone, contradictions rejected at parse time, unservable
  block layers rejected at launch with a named reason, pair-wide resolution and
  the three-step `auto` ladder;
- **host**: attach the upper's filesystem from `cang-build.raw` (layout, grow
  rule and removal semantics from the upper-home ticket), keep the lower's
  `ImageLock` discipline, and skip the host-composed overlay and its workspace
  lease when both layers are block;
- **guest**: mount the upper's subvolume, compose the overlay with the placement
  rule note 02 recorded (one mount, not the lower's filesystem, store tree in one
  subvolume, `userxattr`, no new options), seed the generation as the
  seeding-parity ticket decides, migrate a legacy host-directory upper once, and
  report every failure loudly - there is no host overlay left to fall back to;
- **env contract**: `CANG_NIX_UPPER_*` beside the existing `CANG_NIX_LOWER_*`,
  and a second capability marker so a pre-contract image is refused by name for
  `upper:blk`;
- **docs**: `docs/images-and-storage.md` (`/nix` layering, the disk, the grow
  rule, migration), `README.md`, and the `--nix=`/`--virtiofs=` surface.

Do not start until ticket 01's verdict says the win clears the threshold - a
build on a number that did not clear it is exactly what "measure first" rules out.

## Resolution

(to follow)
