# Seeding a workspace image from an existing host tree

2026-11-21. Ticket 25, `cang workspace init <disk> --from DIR`. Binary
`/nix/store/m6nkp0hj1c1avs88ywnc3g0dq7wdjy3p-cang-0.22.0`, image
`localhost/cang:latest 85e8cb8663af`. Validation: `fmt`, `clippy --all-targets
--all-features -D warnings`, `cargo deny check`, `cargo test` (**1 343 tests, 0
failures**).

## Shape

One task VM, not a host-side copy: the source tree is mounted read-only at
`/mnt/seed` through the existing `--volume` plumbing, the image is attached as the
workspace disk (so the guest mounts it at `/workspace`), and the guest runs a
`bash -lc` script. Unprivileged throughout, and it reuses machinery that is already
proven.

Decisions the ticket asked for:

- **What is copied**: everything except `target/`, `.scratch/` and the tree's own
  `.cang/`. `tar` (not `cp -a`) because `cp -a` aborts on the first socket;
  `.git/objects/info/alternates` is detected and warned about, because it usually
  names an absolute host path that does not exist in the guest.
- **Never half-seeded**: the marker `.cang/seed-complete` is written **after** the
  copy, and a unit test pins that ordering in the script text.
- **Refusals before work**: the fits check is host-side (a walk with the same
  excludes, against the image's size, 85 % cap), so a tree that cannot fit never
  boots a VM; the seeded and non-empty refusals are guest-side, because only the
  mounted tree can answer them.
- **Trigger**: explicit `cang workspace init`. Nothing seeds implicitly, so a
  `--workspace-disk` launch never spends a boot copying gigabytes before your
  command runs.

## Arms (all on the shipped binary)

**(1) too big, refused before boot** — 256 MiB image against this repo:

```
cang: the tree to seed is 634.5 MiB and the image is 256.0 MiB; a seed may fill at
most 85% of the image (the rest is what the first build needs) - create a larger
image with --workspace-size, or --workspace-disk a new path
```

**(2) the real seed** — this repository into a 16 GiB image:

```
cang: seeded 50471 entries into the workspace image
image bytes: 17179869184
```

**(3) a second seed is refused**:

```
cang: this workspace image is already seeded (/workspace/.cang/seed-complete); a
seed is a one-time act, so pass a different image instead
```

**(4) the seeded image is a working checkout** — booted with `--workspace-disk`:

```
tree: .cang .config .git .githooks .github .gitignore .gitmodules .smoke
      .zvec-grep AGENTS.md Cargo.lock Cargo.toml GLOSSARY.md Justfile README.md
      cargo chrome crates deny.toml deps docs flake.lock flake.nix mpvextra nix
      probe result result-1 result-2 result-3 result-cang-prebuilt-release
      result-container result-zvec scripts tools
sentinel: seeded-from=/mnt/seed entries=50471
git status --porcelain:  M crates/cang/src/cli/mod.rs
                         M crates/cang/src/runtime/maintenance/mod.rs
                         A crates/cang/src/runtime/maintenance/workspace_seed.rs
                         M deps/rutabaga_gfx
                         ?? .cang/
git log --oneline -1: af6fef9f (HEAD -> main) docs(wayfinder): ticket 27 shipped ...
cargo check -p cang-guest-relay: Finished `dev` profile in 1m 26s
```

The `git status` output is exactly the host's own working tree at the moment of
seeding (my three edited files plus the dirty submodule and the new `.cang/`), which
is the property that matters: the copy reproduced the checkout, not just its files.

## What it does not claim

- A whole-workspace `cargo check` inside the guest fails on native dependencies
  (`virglrenderer.pc`): cang builds in `nix develop`, on either side of the VM. The
  seeded crate that has no native deps does build there.
- `--offline` resolution fails inside the guest because the cargo home is the
  launch's registry mount; the resolved-index case needs the devshell too.
- No sync direction exists: a seed is one-time, by design (contract: the image is
  authoritative, and the host sees it through the relay or a server).
