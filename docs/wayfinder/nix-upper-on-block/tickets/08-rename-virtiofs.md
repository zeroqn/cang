---
label: wayfinder:task
title: Rename `--fs=` to `--virtiofs=` and retire its env vars
status: open
blocked_by: []
claimed_by: "pi (2026-11-10)"
---

## Question

The flag perimeter settles on one flag per subsystem: `--virtiofs=KEY:VALUE`
where `--fs=` is today, with `CANG_VIRTIOFS_*` where `CANG_FS_*` is today, and
**no aliases** (bob, 2026-11-10 - hard break, 0.x). This ticket is the mechanical
half, independent of the `/nix` upper and of ticket 01's verdict, so it can ship
first.

Scope:

- rename the clap argument (`crates/cang/src/cli/mod.rs`, the `fs` field and its
  long help) and every `--fs=` mention: 39 sites across `README.md`,
  `runtime/vm/libkrun/launcher.rs`, `docs/` and the repository tests;
- rename the env vars `CANG_FS_ZERO_MESSAGE_OPEN`, `CANG_FS_NO_FLUSH`,
  `CANG_FS_CACHE_SYMLINKS`, `CANG_FS_NEGATIVE_ENTRY_TIMEOUT`, `CANG_FS_STATS` to
  `CANG_VIRTIOFS_*`, with their constants, warning messages and tests; keep
  `CANG_FS_ROOT_TAG` and `CANG_FS_WORKER_THREADS` **as they are** if they are not
  user-facing CLI knobs - decide per constant and say which moved and why;
- the long help says what it replaced, so a user following an old command line
  finds the new name;
- no behaviour change: same keys, same values, same defaults.

Verification: `cargo test`, `clippy -D warnings`, nixfmt untouched, and the
repository-invariant tests that assert flag text updated. Manual: `cang --help`
shows `--virtiofs`, and `cang --fs=cache-symlinks:on -- true` fails with clap's
unknown-argument error rather than being accepted.

## Resolution

Implemented in commit 4624b72 (claimed 2026-11-10).

**What moved.** `--fs=KEY:VALUE` → `--virtiofs=KEY:VALUE` (same grammar, keys and
defaults: `zero-message-open`, `no-flush`, `negative-entry`, `cache-symlinks`),
with the field (`fs` → `virtiofs`), the `value_parser`
(`parse_fs_option` → `parse_virtiofs_option`) and the four resolution helpers
(`fs_*_option` → `virtiofs_*`) renamed after it. The five user-facing environment
variables became `CANG_VIRTIOFS_ZERO_MESSAGE_OPEN`, `_NO_FLUSH`, `_CACHE_SYMLINKS`,
`_NEGATIVE_ENTRY_TIMEOUT`, `_STATS` (constant names included, so a constant and
its environment string cannot drift apart), and their doc comments in
`crates/cang-libkrun/src/lib.rs` name the new spellings.

**What deliberately did not move**, decided per constant rather than by sweep:
`CANG_FS_ROOT_TAG` (the tag the guest kernel resolves as its root filesystem -
not a share option, and renaming it would imply a different concept) and
`CANG_FS_WORKER_THREADS` (a device default with neither a flag nor an
environment override, documented as "no CLI flag: cang sets it, a user has no
reason to"). The `FsOption` type keeps its name: it is one share option, not the
flag.

**No aliases.** `--fs=...` now fails as an unexpected argument (exit 2) and the
old environment variables are simply not read, with the long help saying what
the flag replaced.

**Verification.** `cargo fmt --check`, `cargo clippy --all-targets
--all-features -- -D warnings`, `cargo test --workspace` (all green, including
the parser tests now named `virtiofs_option_*`); `--help` shows `--virtiofs` and
the rename note; `--virtiofs=no-such-thing:on` still lists the supported keys.

**Still open:** the live boot that proves the renamed *environment* variable
reaches the share - `CANG_VIRTIOFS_STATS=1` on a real launch, checking the
opcode-mix log at VM stop. It was deferred because a sibling measurement owned
the only VM window on this host; run it before calling the ticket closed.
