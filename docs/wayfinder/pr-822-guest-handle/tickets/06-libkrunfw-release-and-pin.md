---
label: wayfinder:task
title: Publish the libkrunfw release and repin per system
status: closed
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

## Resolution (2026-09-29, pi) - released and pinned

Both forks are released and both pins are in, verified against the published
assets.

- **`zeroqn/libkrunfw` `v5.6.2-cang.2`** - the fork's `cang` branch was rebased so
  the kernel commit sits on the fork's own CI commit (`8707239`, "scope the
  release concurrency group per ref") rather than beside it; the rebased tip is
  `6b38b17`, byte-identical in content to the `3fdbb59` that was boot-tested.
  Pushing the tag made the fork's `publish-cang-release.yml` build and publish
  the release with `libkrunfw-x86_64-kvm-lto.tgz`, `libkrunfw-x86_64-lto.tgz`,
  `libkrunfw-x86_64.tgz`, `libkrunfw-aarch64.tgz` and `libkrunfw-riscv64.tgz`
  (run green, `draft: false`, `prerelease: true` as the fork's versioned releases
  are). `nix/pins.nix` re-pinned per system with
  `update-libkrunfw.sh --tag v5.6.2-cang.2` (`50ba147`) and
  `nix build .#libkrunfw` validates the SRI.
- **`zeroqn/libkrun` `v2.0.0-cang.4`** - pushed `cang` to `63f3737f` and an
  annotated tag on it; the fork's CI published `libkrun-x86_64-linux-full.tgz`
  and `libkrun-aarch64-linux-full.tgz`. cang has nothing to pin here: it links
  the fork's Rust API by path, so the submodule pointer is the pin.
- **cang `v0.10.0`** - `ecbfe8f` bumps the workspace version to 0.10.0 (and
  `cargoDeps.hash`, which rides the vendored lock's member versions), `a9e9e66`
  pins `cangPrebuiltRelease` to `v0.10.0` /
  `cang-v0.10.0-x86_64-unknown-linux-gnu` with the SRI computed from
  `nix build .#cang-ci-sccache` normalized as the workflow does. The tag is on
  the pin commit; the main push's release run finished before the tag went out
  (the v0.9.1 race), and the tag run published the versioned release plus the
  `ghcr.io/zeroqn/cang:v0.10.0` image. Published asset sha256
  `8ddc3b889224a90a63f799c2c1f7b007952861f03156aba4089c5c590f8a3c8e` matches the
  local normalized build byte for byte, and `nix build .#cang-prebuilt` plus
  `cang --version` (`cang 0.10.0`) confirm the pin resolves to the release.
- **The released pair boots.** Running the packaged prebuilt
  (`.../cang-0.10.0-prebuilt-v0.10.0/bin/cang`, whose `lib/cang` carries the
  pinned `libkrunfw-v5.6.2-cang.2`, no firmware override) with
  `--gpu=drm --zero-copy-shm`: guest `uname -r` `6.12.109-hardened1`, guest
  `/dev/udmabuf`, negotiated virtio-gpu bits 0-4 plus **6/7**,
  `VIRTGPU_PARAM_CREATE_GUEST_HANDLE` = **1**. So the released cang binary, the
  released firmware and the pin all agree end to end.
