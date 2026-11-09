---
label: wayfinder:task
title: Rename `--fs=` to `--virtiofs=` and retire its env vars
status: open
blocked_by: []
claimed_by: ""
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

(to follow)
