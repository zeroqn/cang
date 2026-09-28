---
label: wayfinder:task
title: Supply-chain and lockfile policy for the enlarged graph
status: open
blocked_by: ["03-nix-cang-derivation-libkrun-source"]
claimed_by:
---

## Question

cang's build gains libkrun's dependency graph (~150 crates: kvm-ioctls,
rutabaga_gfx, imago, vm-memory, polly, bindgen, ...) plus a git dependency
(`ffier`, via `krun-init-blob`). Decide and implement:

- `cargo deny check` (the repo's gate: `nix develop --command cargo deny check`)
  over the new graph: licenses, advisories, duplicate versions. Either the graph
  passes, or the exceptions live in `deny.toml` with a written reason. Expect
  findings; this gate is also the honest inventory of what cang now links.
- Whether the fork's one-line `ffier` fix (make it optional; ticket 03) is taken,
  which removes the only git dependency from cang's lock.
- The update path: today `scripts/update-libkrun.sh` only refreshes a prebuilt
  asset hash. With a path dependency, a libkrun bump is a *submodule pointer*
  bump, and cang's `Cargo.lock` + the Nix vendor hash must be regenerated in the
  same commit (or the build fails in a way that looks unrelated). Say where that
  is documented (`docs/maintenance.md`?) and, if useful, fold it into a script.
- CI cache keys: `publish_release.yml`'s sccache key hashes
  `Cargo.lock`/`flake.lock`/`nix/pins.nix`; confirm a libkrun bump invalidates
  what it must.

Done when: `cargo deny check` passes with a written rationale for every
exception, and the libkrun-bump procedure is documented and exercised once.
