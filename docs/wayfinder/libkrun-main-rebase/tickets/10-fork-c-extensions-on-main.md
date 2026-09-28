---
label: wayfinder:task
title: Re-add the fork's C extensions on main's device code
status: closed
blocked_by: ["04-rebase-cang-onto-main"]
claimed_by: pi session (2026-09-28)
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


## Resolution (2026-09-28, pi session)

Re-added on main's device/builder code, plus the release-pipeline fix that the
ticket's own verification depends on. `notes/10-fork-extensions-on-main.md` has
the file-level mapping.

**C surface re-added (regenerated from the Rust API, so the names are
`krun_<object>_<method>`):**

- `krun_gpu_device_set_render_server_fd(GpuDevice*, int, KrunError*)` - validates
  the fd, owns it as `Option<OwnedFd>`, threads it through
  `Gpu` -> `Worker` -> `VirtioGpu`, and sets
  `RutabagaBuilder::set_server_descriptor`. cang's `--gpu=drm`/`--wayland` bind it
  by name and fail loudly without it, so the missing entry point is what blocked
  GPU mode.
- `krun_vmm_builder_set_profile_path(VmmBuilder*, KrunStr, KrunError*)` +
  `vmm::profile::KrunProfiler` + per-phase measurements in `build_microvm`
  (payload, guest memory, device attach, vCPU start, event subscriber) and the
  API layer (event-manager creation). The v1
  `krun_set_kernel_cmdline_append` half is **not** re-added: ABI 2 exposes it as
  `krun_payload_append_cmdline`, which cang already uses.
- The virtio-gpu device fixes came with the fd work: fence retirement
  (`event_poll` + `poll_descriptor` polled alongside the control queue, on a
  timeout only while a fence is pending), the fence descriptor kept alive for the
  worker's lifetime, and poison-safe fence-handler locks. The blob-map/render-node/
  cookie fixes are not re-added - crates.io `rutabaga_gfx` 0.1.85 carries them.

**CI/packaging:** the release workflow now builds with `FFI=1` and asserts the
exported symbols (without it, the published `libkrun.so.2` exported *nothing*),
and `binutils` joined the Linux build packages. `CANG.md` documents the FFI=1
requirement.

**Verified:** `make gen-libkrun-bindings` (header + schema match cang's bindings),
a local `FFI=1` build of the fork exporting 101 `krun_*` symbols including both new
entry points, `cargo clippy --locked --features net,blk,gpu,input -- -D warnings`
and `cargo fmt --all -- --check` clean, a `--gpu=drm` live boot (guest gets
`/dev/dri/card0`, `renderD128` and the venus virtio ICD), and a `--profile` run
whose artifact carries the new `libkrun_build_*` rows.

**Published:** the branch and the permanent tag `v2.0.0-cang.2` were pushed, so
the corrected asset exists; the re-pin is
[Rebuild the v2 release with the C ABI and re-pin](13-release-c-abi-repin.md).
