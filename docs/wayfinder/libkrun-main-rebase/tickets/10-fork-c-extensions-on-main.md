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

## Deliverable

The fork's feature set restored on main's code, with the file-level mapping
(old entry point -> new implementation) recorded in `notes/`, plus a build of
the fork proving the symbols exist in the produced `libkrun.so.2`.
