---
label: wayfinder:task
title: Supply-chain and lockfile policy for the enlarged graph
status: closed
blocked_by: ["03-nix-cang-derivation-libkrun-source"]
claimed_by: pi session (2026-09-28)
---

## Question

cang's build gains libkrun's dependency graph plus a git dependency (`ffier`, via
`krun-init-blob`). Decide and implement the licence/advisory policy, the update
path, and the CI cache-key consequences.

## Resolution

**`cargo deny check` passes; the update procedure is documented.**

- `deny.toml` gained three allowances with the reason written next to them:
  `BSD-3-Clause`, `ISC` and `Zlib` (from `zstd-sys`, `bzip2-sys`, `imago`,
  `linux-loader` and friends). All OSI-approved and copyleft-free. Before that,
  only the three failures were licence ones - `advisories ok` and `bans ok`
  throughout, including the fork's double-vendored `ffier` at one name-version.
- `sources` needs no change: the `ffier` git dependency is covered by
  `unknown-git = "warn"` (not `deny`). Tightening that to deny would require an
  `allow-git` entry and is not necessary for this migration.
- The `ffier` dependency is not removed: it is an unconditional (though unused
  without `ffi`) dependency of `krun-init-blob`. Making it optional in the fork
  was proposed in ticket 01/03 and left undone - cost is build time only.
- Update path documented in `docs/maintenance.md` ("Updating the `deps/libkrun`
  submodule"): pointer bump + `Cargo.lock` + *two* vendor hashes
  (`nix/pkgs/cang-rust.nix`'s `cargoDeps`, `nix/pkgs/libkrun-source.nix`'s
  `libkrunCargoDeps`) + live boot and GPU smoke.
- CI cache keys: `publish_release.yml`'s sccache key already hashes
  `Cargo.lock`/`flake.lock`/`nix/pins.nix`. A libkrun bump changes `Cargo.lock`
  only if the fork's dependency versions move; when it does, the key changes.
  The Nix-level caching (the derivation's `cargoDeps` and the compiled libkrun)
  is keyed by the derivation hash, which moves with the submodule source.
