---
label: wayfinder:task
title: Port the VMM half onto the ABI-2 fork against the pinned rutabaga rev
status: closed
blocked_by: ["01-rutabaga-delta", "03-port-matrix-on-abi2", "04-carried-design"]
claimed_by: pi session (2026-09-29)
---

## Question

Implement ticket 04's decision on `deps/libkrun`:

1. Swap crates.io `rutabaga_gfx 0.1.85` for the **rev-pinned git dependency** and
   carry whatever delta ticket 01 says is still needed, in the shape ticket 04
   chose.
2. Apply the payload per ticket 03's matrix: the `UdmabufDriver` wrapper in
   `src/utils/src/linux/`, the feature advertisement and driver plumbing in
   `src/devices/src/virtio/gpu/`, and the memfd-backed guest RAM in
   `src/libkrun/src/vmm/builder.rs` - resolved against the fork's own GPU
   extensions, not taken wholesale.
3. Exclude or adapt `230f2c55`; keep the constant values ticket 04 fixed.
4. Refresh `Cargo.lock` and the `fetchCargoVendor` hash in
   `nix/pkgs/libkrun-source.nix`, and keep `nix build ./nix/dev#cang-dev` green.
5. Wire the gate from cang: the `--zero-copy-shm` companion flag (error if given
   without `--gpu=drm`), through `crates/cang-libkrun` into the fork's
   `DeviceRequirements` bool - the same bool that gates the bits and the RAM
   backing. Default off. When the stack cannot serve it, log one clear reason and
   keep the copy path (do not fail the run).
6. Add the fork-side guard: warn (with the `ctx_id`) when a
   `CREATE_GUEST_HANDLE` blob would reach a component that ignores handles, and
   keep serving.
7. Document the flag in `README.md` (user-visible behaviour) and record that the
   balloon is inert on fast-path runs.
8. Run `cargo fmt --check`, clippy with `-D warnings`, and the crate tests in the
   devshell.

**No fork release is involved:** cang compiles `deps/libkrun` with cang's own
rustc, so the change lands as fork commits + a submodule pointer move, and both
`cargoDeps` (`nix/pkgs/cang-rust.nix`) and `libkrunCargoDeps`
(`nix/pkgs/libkrun-source.nix`), plus both `Cargo.lock`s, refresh in the same
commit. `nix build .#cang` is the end-to-end check.

## Deliverable

The fork commits, the moved submodule pointer, the refreshed vendored-crate
hashes and both lockfiles, and `notes/07-fork-vmm-port.md` with the build
evidence.

## Resolution (2026-09-29, pi)

**Landed and green.** All eight items are in the fork and in cang; the detailed
file-by-file record and the build evidence are in
[`notes/07-fork-vmm-port.md`](../notes/07-fork-vmm-port.md).

- `deps/libkrun` `a1a772a0` (the port, on magma-gpu/rutabaga_gfx#81's `ctx_id`
  route - PR 822's three component-routing hunks and `230f2c55` are not carried,
  bits stay 6 = `CREATE_GUEST_HANDLE` and 7 = `BLOB_CTX_ID_FIX` beside our 5 =
  `FENCE_PASSING`) and `5d9cb075` (lock refresh + regenerated
  `ffier-krun.json` / `libkrun.h`). `127313d` moves the cang submodule pointer
  past both.
- cang: `f62b3ca` adds `--zero-copy-shm` (errors without `--gpu=drm`, off by
  default) through `crates/cang-libkrun` into `DeviceRequirements` - the same bool
  that gates the bits and the RAM backing - plus the Landlock `/dev/udmabuf`
  grant and its `classify_fd` classification; `761c6b9` bridges libkrun's `log`
  records into cang's tracing subscriber, without which the fork's
  "cannot serve it, running without it" warning and the mis-route warning were
  dropped. `README.md` documents the flag and the balloon trade.
- Both vendored-crate hashes refreshed in `6a5e14a`; `nix build .#cang` is green
  (`/nix/store/p78385yi099hddafyddxdwp8k64wd3ll-cang-0.9.1`), with
  `buildRustPackage`'s lock-consistency check running against both vendor dirs.
- Gates in the devshell: `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo deny check` (advisories/bans/licenses/
  sources ok) and `cargo test` all clean.

One trap worth carrying forward: the two `fetchCargoVendor` calls are both named
`cargo-deps-vendor`, and a fixed-output derivation's path comes from the name and
the specified hash alone, so a shared placeholder hash points both at one store
path and the second is never content-checked - the run then fails later, in
`buildRustPackage`, with a "Cargo.lock is not the same" message that reads like a
stale hash. Harvest the two hashes with two distinct placeholders.

Next: ticket 05's boot test, then tickets 06 and 09.
