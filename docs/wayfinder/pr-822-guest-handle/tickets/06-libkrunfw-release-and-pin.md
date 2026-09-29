---
label: wayfinder:task
title: Publish the libkrunfw release and repin per system
status: open
blocked_by: ["05-libkrunfw-kernel-support"]
---

## Question

Bob drives the GitHub side: tag the pushed `cang` branch (permanent versioned tag
or the rolling one, per `cang-fork-versioned-release-tags-and-pins`), let CI
publish the prebuilt assets, then:

- run `scripts/update-libkrunfw.sh --system x86_64-linux|aarch64-linux|riscv64-linux`
  per system (each run rewrites only that system's hash),
- move the `deps/libkrunfw` submodule pointer to the tagged commit,
- update `nix/pkgs/libkrunfw.nix` (`kernelVersion`, `kernelHardenedVersion`, both
  `fetchurl` hashes) and the OOM-console fixture in
  `crates/cang/src/runtime/session/supervisor/guest_death_tests.rs` if the
  version string moves.

Record the CI run id, the published asset list and the new pin.

## Deliverable

The release published, `nix/pins.nix` + submodule pointer + `nix/pkgs/libkrunfw.nix`
updated, and the evidence recorded here.

## Prep (2026-09-29, pi) - everything except the GitHub side

`deps/libkrunfw` `cang` is at `3fdbb59` (patches `0037-0039` + `CONFIG_UDMABUF=y`
in all six configs) and boot-tested locally on both the plain `x86_64-kvm` build
and the shipped `libkrunfw-x86_64-kvm-lto.tgz`; `deps/libkrun` `cang` is at
`63f3737f` (the port plus the ticket-12 coalescing fix). Both branches are local:
`git -C deps/libkrun log --oneline 6176c1f..cang`-style checks will show commits
GitHub does not have yet, so `nix build .#cang` from a clean tree fails until they
are pushed (use `path:$PWD` until then).

The steps, in order:

1. **bob pushes** both fork branches (`zeroqn/libkrun` and `zeroqn/libkrunfw`,
   branch `cang`).
2. **bob tags a permanent release** on the libkrunfw fork. The tag must match the
   repository test's shape, `v<major>.<minor>.<patch>-cang.<n>` - the next one
   after `v5.6.2-cang.1` is **`v5.6.2-cang.2`** (see
   `crates/cang-repository-tests/tests/repository.rs`,
   `versioned_fork_release_tag_shape_is_enforced`); a rolling `cang-<sha>` tag
   would trip the `publish_release.yml` pin gate for the next tagged cang release.
   The fork's CI publishes `libkrunfw-x86_64-kvm-lto.tgz`,
   `libkrunfw-x86_64-lto.tgz`, `libkrunfw-aarch64.tgz` and
   `libkrunfw-riscv64.tgz`; assets only exist after the CI run finishes.
3. **Re-pin per system**, one run each with the same tag (each run rewrites only
   its system's asset + hash):
   ```bash
   for system in x86_64-linux aarch64-linux riscv64-linux; do
     nix develop --command ./scripts/update-libkrunfw.sh --tag v5.6.2-cang.2 --system "$system"
   done
   ```
   The script needs `curl`, `jq` and `python3` and queries the GitHub Releases
   API (unauthenticated, so it can rate-limit).
4. **Move the submodule pointer** to the tagged commit (the tag points at
   `3fdbb59` unless the branch moved) and re-run the gates:
   `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
   `cargo deny check`, `cargo test`, `nix build path:$PWD#cang`.
5. **Only if a version string moves**: `nix/pkgs/libkrunfw.nix`
   (`kernelVersion`/`kernelHardenedVersion`) and the OOM-console fixture in
   `crates/cang/src/runtime/session/supervisor/guest_death_tests.rs`. This
   release keeps `linux-6.12.109` and `v6.12.109-hardened1`, so neither changes.

Record the CI run id, the published asset list and the new pin here when done.
