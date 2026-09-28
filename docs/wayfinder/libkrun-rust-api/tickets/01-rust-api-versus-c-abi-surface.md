---
label: wayfinder:research
title: libkrun's Rust API surface versus the C ABI cang binds
status: closed
blocked_by: []
claimed_by: pi session (2026-09-28)
---

## Question

Does libkrun's Rust API cover every call cang's `LibkrunApi` makes, how does the
guest init get injected without `libkrun_init.so`, and does dropping the C ABI
cost anything? Answered while charting as a research ticket.

## Resolution

**Yes to coverage, and the C ABI costs nothing worth keeping.**

Detail in `../notes/01-rust-api-surface.md`. In short:

- The whole `krun_*` surface is *generated from* the Rust API by `ffier`
  (`make gen-libkrun-bindings`), so the C ABI has no stability the Rust API
  lacks; for a consumer that compiles the source, it is pure indirection.
- Every `LibkrunApi` method has a Rust counterpart, including the fork-only ones
  (`GpuDevice::set_render_server_fd`, `VmmBuilder::set_profile_path`).
  `DisplayBackend::new` still takes the raw vtable pointer, so cang's headless
  backend survives verbatim. `VirglRendererFlags` is narrower than cang's raw u32
  (no `DRM`/`USE_VIDEO`) - `from_bits_retain` or a fork addition (ticket 07).
- Init injection without `libkrun_init.so` is `krun-init-blob`'s existing
  `direct` feature: `Config::apply(&mut krun::FsOverlay, &mut krun::Payload)`,
  next to the `ffi-client` variant that `dlsym`s its two libkrun calls.
- Upstream itself ships a statically linked Rust-API consumer: libkrun's test
  suite has `static-linking` (= `krun` + `krun-init-blob/direct`) and
  `dynamic-linking` features over the same test bodies. Its static feature set
  enables only `blk` + `net`, so `gpu`/`input`/`timesync` are cang-first - the
  GPU smoke (ticket 09) is the backstop.
- One wrinkle to fix in the fork (ticket 03): `krun-init-blob/Cargo.toml`
  declares `ffier` non-optionally although all its uses are `#[cfg(feature =
  "ffi")]`, which drags a git dependency into cang's graph for nothing.
