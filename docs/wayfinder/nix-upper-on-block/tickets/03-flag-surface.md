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
2. **Who composes the overlay.** An axis pair decides it: both virtio-fs = the
   host composes (today's path, `runtime/session/nix_overlay.rs`), any block =
   the guest composes (`components/nix/root.rs:mount_block_lower`). Then
   `lower:virtiofs,upper:blk` needs a guest-visible lower that is not a block
   image - a *new* read-only virtio-fs bind of `<rootfs>/nix`. Support that
   combination, or refuse it by name (it is the slowest possible pairing and
   exists only to A/B the upper)?
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
