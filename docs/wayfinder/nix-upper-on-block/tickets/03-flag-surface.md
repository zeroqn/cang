---
label: wayfinder:grilling
title: One flag per subsystem - the `--nix=lower:...,upper:...` grammar and the `--fs=` rename
status: closed
blocked_by: []
claimed_by: "pi (bob, 2026-11-10)"
---

## Question

Bob has settled the *principle* (2026-11-10): one flag surface per subsystem -
`--nix=` for the two overlay axes, `--virtiofs=` where `--fs=` is today, and
`--nix-lower` retired. Consequences still to settle, and they are public surface:

1. **Grammar.** `--nix=lower:blk,upper:blk` as one comma list, or repeatable
   `--nix=lower:blk --nix=upper:blk` (`--fs=` is repeatable `key:value` today,
   `cli/mod.rs:284`)? What does a bare `--nix=blk` mean, if anything? Is `auto`
   per axis, or one `auto` that implies the other axis (`lower:auto` +
   `upper:auto` = today's default; what is `lower:blk` alone)?
2. **Who composes the overlay, and the refusals.** Settled (bob, 2026-11-10): a
   blk upper **forces** a blk lower - `upper:blk` with `lower:virtiofs` is
   refused by name, not served, so no read-only virtio-fs bind of
   `<rootfs>/nix` is ever built. Both virtio-fs = the host composes (today's
   path, `runtime/session/nix_overlay.rs`); both block = the guest composes
   (`components/nix/root.rs:mount_block_lower`). What is left to settle:
   - where the refusal lands - clap parse time, launch planning, or after the
     block lower's capability/lock checks (the lower already has an
     `auto`-vs-`blk` distinction that names the reason it cannot serve:
     `nix_lower.rs` `ImageLock`, and the guest-init marker gate);
   - what `lower:auto` resolves to when `upper:blk` is given - does `auto`
     become `blk` (and refuse if the image cannot be served), or does it refuse
     the pair because `auto` could mean virtiofs?;
   - whether `upper:auto` follows the lower (`lower:blk` implies `upper:blk`
     and vice versa) or defaults independently.
3. **Retirement.** `--nix-lower` and `CANG_NIX_LOWER_BLOCK` (host side) /
   `CANG_NIX_LOWER_*` (guest contract, `components/env.rs:29-35`) - hard break in
   one release, or one release of aliases plus a warning naming the replacement?
   The guest half has a compatibility gate of its own
   (`guest_init_mounts_block_lower`, a string marker in the binary,
   `nix_lower.rs:407-430`): what is its marker for the new contract?
4. **The rename's blast radius.** `--fs=` appears 39 times across `README.md`,
   `cli/mod.rs`, `runtime/vm/libkrun/launcher.rs`, `docs/` and repository tests.
   Is the rename a mechanical sweep in the same change, and does any env var
   (`CANG_FS_*`?) or config key mirror it? Are there repository-invariant tests
   that assert flag names and must be updated
   (`crates/cang-repository-tests/`)?

Resolve with the exact accepted spellings, the accepted value set per axis, the
combination matrix (accepted / refused-with-message), and the retirement
schedule. This ticket does not build the flag - it fixes the contract the
implementation tickets then satisfy.

## Resolution

(to follow)

## Resolution

Settled with bob, 2026-11-10. Six answers, then the rules they imply.

**Accepted surface.** `--nix=KEY:VALUE`, repeatable, **one pair per occurrence**
(mirroring `--fs=`/`--virtiofs=`); keys `lower` and `upper`; values
`auto | blk | virtiofs`. `--fs=` becomes `--virtiofs=` with the same grammar and
the same four keys. The flag being absent means both axes `auto`.

**Refusals, at two levels.**

- *Clap parse time*, for what needs no environment: an unknown key (message lists
  `lower`, `upper`), an unknown value (lists `auto`, `blk`, `virtiofs`, in
  `parse_nix_lower_arg`'s existing voice), a bare `--nix=blk` (message names both
  axes), and a **contradiction** - `blk` on one axis with `virtiofs` on the other.
- *Launch time*, for what depends on the host: a block layer that cannot be
  served - image not buildable, `ImageLock` held by another launch, no
  `mkfs.btrfs`, or a guest image whose `cang-guest-init` predates the contract -
  refused with a named reason, exactly as `--nix-lower=blk` already behaves.

**Pair-wide resolution.** The two axes are one decision; only explicitness
differs:

| input | result |
|---|---|
| `blk` on either axis, nothing contradicting it | **both block** (`upper:blk` forces `lower:blk`, and `lower:blk` moves both) |
| `virtiofs` on either axis, nothing contradicting it | **both virtio-fs**, the host composes the overlay |
| `blk` on one axis, `virtiofs` on the other | parse error naming the contradiction |
| `auto` (or absent) on both | see the ladder below |

**The `auto` ladder** (bob: "auto means both blk"):
1. both block, when the block path is fully servable - the lower image can be
   built and locked, and the image's guest half carries the upper contract;
2. otherwise **the grandfather branch**: `lower:blk` + the host-composed upper,
   which is exactly today's default, so an image that predates the upper
   contract does not lose the shipped block lower (the one place `auto` may
   produce a pairing an *explicit* flag would be refused for - it exists to keep
   today's behaviour while images catch up, and it disappears once the image
   ships the contract);
3. otherwise both virtio-fs, with the reason named - today's `auto` behaviour.

**Retirement is a hard break** in one change: `--nix-lower` and `CANG_NIX_LOWER_BLOCK`
are removed (as flags and env vars), `--fs=` and the `CANG_FS_*` env vars
(`CANG_FS_ZERO_MESSAGE_OPEN`, `CANG_FS_NO_FLUSH`, `CANG_FS_CACHE_SYMLINKS`,
`CANG_FS_NEGATIVE_ENTRY_TIMEOUT`, `CANG_FS_STATS`) become `--virtiofs=` and
`CANG_VIRTIOFS_*`, and the sweep covers all 39 `--fs=` sites (`README.md`,
`cli/mod.rs`, `runtime/vm/libkrun/launcher.rs`, `docs/`, repository tests). No
deprecation shim: 0.x, and an alias is permanent tax.

**Guest env contract.** The `CANG_NIX_LOWER_*` names stay (they are still
accurate: `BLOCK`, `DISK_ID`, `DISK_LABEL`, `UPPERDIR`, `WORKDIR`); the upper
adds its own `CANG_NIX_UPPER_*` (disk id, disk label, mount point, subvolume) and
a **second capability marker** in `cang-guest-init`, so an image that knows only
the lower still serves `lower:blk` + host upper, while `upper:blk` against it is
refused by name instead of silently landing on the share.

Implementation splits into two tickets: the rename ships on its own
([Rename `--fs=` to `--virtiofs=` and retire its env vars](08-rename-virtiofs.md)),
and the `--nix=` axis plus the upper disk rides the verdict and the design tickets
([Implement `--nix=lower:,upper:` and the guest upper disk](09-implement-nix-upper.md)).
