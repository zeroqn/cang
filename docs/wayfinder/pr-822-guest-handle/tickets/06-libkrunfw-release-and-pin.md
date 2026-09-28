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
