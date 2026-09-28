---
label: wayfinder:task
title: Port the VMM half onto the ABI-2 fork against the pinned rutabaga rev
status: open
blocked_by: ["01-rutabaga-delta", "03-port-matrix-on-abi2", "04-carried-design"]
claimed_by: (unclaimed)
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
5. Run `cargo fmt --check`, clippy with `-D warnings`, and the crate tests in the
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
