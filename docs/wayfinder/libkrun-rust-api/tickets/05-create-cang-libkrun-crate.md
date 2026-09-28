---
label: wayfinder:task
title: Create cang-libkrun and make the launcher's seam a Rust-API backend
status: closed
blocked_by: ["03-nix-cang-derivation-libkrun-source", "04-nix-toolchain-and-init-blob"]
claimed_by: pi session (2026-09-28)
---

## Question

Where does libkrun's Rust API get touched, and how does today's handle-shaped
launcher become a Rust-API consumer?

## Resolution

**The crate exists and `cang` drives libkrun's Rust API through it; the seam
(ticket 05's option 1) survived.**

Shape taken: keep `LibkrunApi` and the recording fake, add one production
implementation, so `launcher.rs` and its 1600 lines of fixtures did not have to
be rewritten around libkrun's ownership graph.

- `crates/cang-libkrun/{api,linked,display}.rs`: `api.rs` is the trait (moved
  verbatim from cang, visibility lifted to `pub`); `linked.rs` is
  `LinkedLibkrunApi`, the only implementation; `display.rs` carries the headless
  display vtable, which the Rust API still takes as a raw `krun_display_backend`
  pointer (copied and verified by `DisplayBackend::new`).
- **The backend is an arena.** `Slot` is an enum of the real libkrun values
  (`Payload`, `FsOverlay<'static>`, `FsDevice<'static>`, `BlockDevice`,
  `NetDevice`, `VsockDevice`, `ConsoleBuilder/Device<'static>`, `GpuDevice`,
  `MmioDeviceManager<'static>`, `VmmBuilder<'static>`, `Vmm<'static>`, the init
  `Builder` and the built `Config`). A `Handle` is its index; the
  consume-and-return builder methods take the value out and put the returned one
  back, and a consumed handle is an error instead of a silent use. Everything is
  instantiated at `'static`, which is sound here because the only borrowed things
  are the init blob (static) and raw fds the launcher holds for the VM's life -
  exactly the contract libkrun documents on those parameters.
- **`krun_init_blob`'s `Config` is leaked** (`Box::leak`): `Config::apply` wants
  `&'a self` and `&mut FsOverlay<'a>` at one lifetime and the config must outlive
  the VM anyway. Documented in the module header.
- `GpuDevice`'s narrowing handled with `VirglRendererFlags::from_bits_retain`, so
  cang's `DRM`/`USE_VIDEO` bits survive (see ticket 07 for the flag audit).
- Call sites updated: `LinkedLibkrunApi::new()` replaces
  `DynamicLibkrunApi::open_default()` in
  `runtime/session/supervisor/vm_child.rs` and
  `runtime/maintenance/container_store.rs`; `crate::runtime::vm::libkrun::mod.rs`
  now only holds the launcher and re-exports.
- **Workspace**: `crates/cang-libkrun` is a member; only it depends on libkrun.
  Root `Cargo.toml` gained `exclude = ["deps"]` (see ticket 03).

Verified: `cargo test --workspace` (cang 593 tests, guest-init 296, repository
45 - all pass), `cargo fmt --check`, `cargo clippy --all-targets --all-features --
-D warnings`, and the same suite inside `nix build .#cang`.
