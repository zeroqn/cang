---
label: wayfinder:grilling
title: One flag per subsystem - the `--nix=lower:...,upper:...` grammar and the `--fs=` rename
status: open
blocked_by: []
claimed_by: ""
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
