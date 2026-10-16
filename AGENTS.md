# AGENTS.md

## Purpose

This repository contains `cang`, a Rust CLI that launches a direct-libkrun
microVM task environment from an OCI image with the current working directory
mounted at `/workspace`.

The repository also builds the guest bootstrap binary `cang-guest-init`, the
guest protocol crates it shares with the host, and the container image that
embeds them.

## Repository Layout

- `crates/cang/`: host-side `cang` CLI, libkrun runtime orchestration, OCI
  image ingestion, task rootfs/storage handling, Landlock/seccomp sandboxing,
  and host-side tests.
- `crates/cang-guest-init/`: in-guest `cang-guest-init` bootstrap binary,
  root/user setup, guest Podman preparation, status files, and tests.
- `crates/cang-attach-protocol/`: attach/detach protocol shared by host and
  guest.
- `crates/cang-exec-protocol/`: guest exec protocol shared by host and guest.
- `crates/cang-repository-tests/`: repository-wide invariant tests over Nix,
  workflow, and documentation content.
- `flake.nix`: development shell, Rust packages, and container image definition.
- `README.md`: user-facing overview, prerequisites, quick start, and pointers
  to the topic docs.
- `docs/`: topic documentation (`usage`, `graphics-audio`, `security`,
  `networking`, `images-and-storage`, `diagnostics`, `internals`, `build`,
  `maintenance`) plus `docs/adr/`, `docs/design/`, and `docs/wayfinder/`.
- `Cargo.toml` / `Cargo.lock`: Rust workspace metadata and dependency lockfile.

## Working Style

- Keep changes narrow and consistent with the host/guest crate ownership split.
- Write host-crate behavioral tests under `crates/cang/src/`. These tests
  exercise the host crate's own public or internal APIs (CLI, runtime, storage,
  sandboxing).
- Write guest-crate behavioral tests under `crates/cang-guest-init/src/`.
  These tests exercise in-guest bootstrap, root/user setup, guest Podman prep,
  or status logic.
- Write repository-invariant tests (Nix file content, workflow, documentation,
  or ADR prose assertions) under `crates/cang-repository-tests/`.
- Update `README.md` whenever user-visible behavior, requirements, or run
  commands change.
- Preserve any existing user changes in the worktree. Do not revert unrelated
  edits.

## Development Workflow

Use the Nix development shell so required tools are available:

```bash
nix develop
```

Common commands:

```bash
cargo build
cargo test
nix build .#cang
nix build .#container
```

Enable the repo's git hooks once per clone with
`git config core.hooksPath .githooks`. The `pre-commit` hook runs `nixfmt` over
the staged `.nix` files and stages the result, so a commit never carries
unformatted Nix; a file `nixfmt` cannot parse fails the commit, and
`git commit --no-verify` skips the hook.

To run the CLI from a built artifact:

```bash
./result/bin/cang --help
```

## Validation

For code changes, prefer this validation sequence:

```bash
nix develop --command cargo fmt --check
nix develop --command cargo clippy --all-targets --all-features -- -D warnings
nix develop --command cargo deny check
nix develop --command cargo test
```

Before committing code changes, make sure formatting, Clippy, cargo-deny, and
tests pass.

If behavior touches container runtime or FUSE integration, also verify manually:

- the container image starts successfully with `podman`
- `/workspace` is mounted as expected

## Safety Notes

- Do not remove or reset `.cang/` or cang state-root contents unless
  explicitly requested.
- Avoid destructive git operations unless explicitly requested.
- Treat Podman, FUSE, and host `/nix/store` assumptions as environment-dependent
  and verify them when changing runtime behavior.

## Communication

- When a user question is needed, address the user as `bob`.
