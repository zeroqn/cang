---
label: wayfinder:task
title: Implement the `--nix=` axis and retire `--nix-lower`
status: open
blocked_by: []
claimed_by: "pi (2026-11-10)"
---

## Question

The flag surface is settled (the flag-surface ticket): `--nix=KEY:VALUE`
repeatable, keys `lower`/`upper`, values `auto|blk|virtiofs`, pair-wide
resolution, contradictions refused at clap parse time, unservable block layers at
launch with a named reason, `--nix-lower` and `CANG_NIX_LOWER_BLOCK` gone with no
alias. This is the mechanical half - it needs no disk, no verdict and no guest
work, so it ships first and the upper disk then plugs into it.

## Resolution

Implemented in commit df5037f (claimed 2026-11-10).

**Grammar.** `--nix=lower:MODE` / `--nix=upper:MODE`, repeatable, one pair per
occurrence; a later occurrence of the same key wins, as with `--virtiofs`. Unknown
layers, unknown modes and a bare `--nix=blk` are refused by the value parser with
the accepted set in the message. Pair-wide resolution: `blk` on either axis moves
both, `virtiofs` on either keeps both on the host share, the two disagreeing is a
named error (exit 2), and `auto` on both is today's behaviour. `--nix=upper:blk`
is refused by name until the disk exists, naming `--nix=lower:blk` as the part
that already works.

**Retirement.** `--nix-lower` and the host-side `CANG_NIX_LOWER_BLOCK` switch are
gone. `CANG_NIX_LOWER_BLOCK` survives only where it belongs: as the switch the
host sets for the guest, which is also the capability marker a pre-block-mode
image is recognised by (`nix_lower.rs` `GUEST_HALF_MARKER`). `block_mode_requested`
no longer reads the process environment at all.

**Plumbing.** `into_action`/`into_runtime_options` became fallible
(`try_into_action`/`try_into_runtime_options`), because a contradiction between
the two axes is a user error with a message rather than a panic; `main` prints it
and exits 2. The lower axis maps onto the existing `NixLowerMode`, so nothing
below the CLI changed.

**Docs and harness.** `README.md`, `docs/images-and-storage.md` and
`tools/nix-lower-cang-smoke/nix-lower-smoke.sh` speak the new flag (the smoke's
opt-out arm is now `--nix=lower:virtiofs`).

**Verified:** `cargo fmt --check`, `cargo clippy --all-targets --all-features --
-D warnings`, `cargo test --workspace` (all green, including the new pair-wide
resolution tests); `--help` shows both axes; `--nix-lower=blk` is an unexpected
argument; the contradiction message, the `upper:blk` refusal and the bare-value
error all read correctly.

**Still open:** the live boot - a `--nix=lower:blk` launch and a
`--nix=lower:virtiofs` launch through `tools/nix-lower-cang-smoke` - deferred
because a sibling measurement owned the VM window. Run it before closing.
