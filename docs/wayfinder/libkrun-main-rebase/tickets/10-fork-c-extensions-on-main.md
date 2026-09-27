---
label: wayfinder:task
title: Re-add the fork's C extensions on main's device code
status: open
blocked_by: ["04-rebase-cang-onto-main"]
claimed_by: unclaimed
---

## Question

The fork's value beyond upstream is its own C-API surface, and main has no
equivalent for the two most important pieces. During the rebase (ticket 04),
re-establish them on main's structure:

1. **`krun_set_gpu_options3`** (render-server fd / `--gpu=drm`, `--wayland`):
   re-implement on main's `GpuDevice`/display path. Route decided by bob
   (2026-09-27): **port the hook** rather than drop GPU mode. Note main has no
   in-tree `rutabaga_gfx` (crates.io `0.1.85`), so the fork's patches to it have
   no path - find out what of that delta main/crates.io already carries before
   re-adding anything.
2. **`krun_set_profile_path`**: no main equivalent; re-add or replace.
3. **The rest of the fork's extension surface** - nested-virt gate, console
   variants, vsock/port-map, `krun_set_kernel_cmdline_append`'s v1 shape
   (main has `krun_payload_append_cmdline`), and the CI-visible behaviour the
   repo's Rust tests and smokes rely on. Verify each still holds after main's
   reorganisation (`src/vmm` now inside `src/libkrun/src/vmm`, new `src/polly`,
   `src/whp`).
4. Keep the fork's features buildable from the fork's own CI feature list
   (`BLK NET GPU INPUT`, main's `vhost-user`/`timesync`/`ffi` added, `SND`
   removed as a silent no-op).

## What the 2026-09-27 triage already established

From `notes/04-fork-commit-triage.md`:

- **No vendored `rutabaga_gfx` is needed.** crates.io 0.1.85 already provides
  `RutabagaBuilder::set_server_descriptor(Option<OwnedDescriptor>)` (`set_use_render_server`
  too; `build()` now takes no arguments), `poll_descriptor()` (wrapping
  `virgl_renderer_get_poll_fd`), and `get_drm_fd` with the `O_RDWR` render-node
  open. The two vaapi commits (`1c68255f`, `f0e2028d`) are therefore dropped as
  superseded rather than ported.
- **Main moved GPU configuration out of `VmResources`**: there are no `gpu_*`
  fields left there, and the builder takes `gpu_shm_size` from the device
  requirements (`src/libkrun/src/vmm/builder.rs:723`). The v1
  `krun_set_gpu_options3` entry point is gone, so the capability must be
  expressed as a v2 GPU-device configuration/setter.
- Commits to re-derive: `ac615be3` + `329f14db` (profiling), `dacdbac4`
  (render-server fd), `7f77a0ac` (fence retirement), `2a583f8a`, `7c20aa6f`
  (device fixes). `deps/libkrun/CANG.md` was updated to describe this state and
  must stay true as the work lands.

## Deliverable

The fork's feature set restored on main's code, with the file-level mapping
(old entry point -> new implementation) recorded in `notes/`, plus a build of
the fork proving the symbols exist in the produced `libkrun.so.2`.

## Prerequisite found while porting (ticket 09, 2026-09-28)

The fork's release pipeline builds the host library without the `ffi` feature
(`make BLK=1 NET=1 GPU=1 INPUT=1 TIMESYNC=1`), so neither `v2.0.0-cang.1` nor
`nix/dev`'s local build produced a `libkrun.so.2` that exports *any* `krun_*`
symbol. Any "the symbol exists in the produced `libkrun.so.2`" verification here
has to build with `FFI=1` and check the symbol table, not just the file. The
cang-side port already calls the two extension entry points by name when present:

- `krun_gpu_device_set_render_server_fd(KrunGpuDevice*, int, KrunError*) -> KrunResult`
- `krun_vmm_builder_set_profile_path(KrunVmmBuilder*, KrunStr, KrunError*) -> KrunResult`

`crates/cang/src/runtime/vm/libkrun/dynamic.rs` binds both as optional symbols, so
re-adding them under those names needs no further cang change. The fork's release
workflow fix and the re-pin are ticket 13.
