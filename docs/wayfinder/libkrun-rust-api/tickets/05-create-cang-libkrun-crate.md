---
label: wayfinder:task
title: Create cang-libkrun and make the launcher's seam a Rust-API backend
status: open
blocked_by: ["03-nix-cang-derivation-libkrun-source", "04-nix-toolchain-and-init-blob"]
claimed_by:
---

## Question

Where does libkrun's Rust API get touched, and how does today's
handle-shaped launcher become a Rust-API consumer?

Shape agreed while charting: a new workspace crate (e.g.
`crates/cang-libkrun`) owns everything that touches `krun::` /
`krun_init_blob::`; `cang` depends on it and keeps policy (launch config ->
devices/env/rlimits). Only that crate gains libkrun in its dependency list, so
the guest-init/attach crates and `cang-musl` stay unaware of it.

The concrete problem: `LibkrunApi` (`api.rs`) + `DirectLibkrunLauncher`
(`launcher.rs`) are shaped like the C ABI - `Handle = usize`, "the library may
hand back a different pointer", sequential calls, explicit
`vmm_builder_destroy` on error - and 1600 lines of tests in `tests.rs` drive
them through a recording fake. The Rust API is ownership-shaped: `VmmBuilder<'a>`
consumes and returns `Self`, `Payload`/`FsDevice<'a>`/`ConsoleDevice<'a>` are
moved into the graph, the render-server fd is an `OwnedFd`, `Config` must outlive
the VM, and `Vmm::run(self)` never returns. Two candidate shapes:

1. Keep `LibkrunApi` as the seam and add a `LinkedLibkrunApi` holding the real
   values (slab keyed by index, `'static` lifetime erasure) so the existing
   fixtures and `launcher.rs` survive the swap.
2. Restructure the launcher to own the value graph directly and move the test
   seam (`codebase-design` vocabulary) - fewer unsafe lifetime games, but the
   C-ABI-shaped fixtures get rewritten.

Decide with `codebase-design`; the destination only requires that the mapping
into the VM graph stays testable without a hypervisor.

Done when: the crate builds against the path dep, `cargo test` for the launcher
suite passes with the Rust-API backend in place, and `cang` no longer constructs
a `LibkrunApi` itself.
