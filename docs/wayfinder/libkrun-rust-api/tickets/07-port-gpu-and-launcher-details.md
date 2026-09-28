---
label: wayfinder:task
title: Port the GPU/launcher details, and re-verify the libva preload
status: closed
blocked_by: ["05-create-cang-libkrun-crate"]
claimed_by: pi session (2026-09-28)
---

## Question

Close the gaps between cang's ABI-2 call sequence and the Rust-API shapes that
do not map 1:1:

1. **`VirglRendererFlags` is narrower than cang's raw u32.** cang sets
   `USE_EGL | THREAD_SYNC | VENUS | RENDER_SERVER | DRM | USE_VIDEO`
   (`VIRGLRENDERER_VENUS_FLAGS` in `launcher.rs`); the Rust bitflags has the
   first four only. Either `VirglRendererFlags::from_bits_retain(bits)` or add
   `DRM`/`USE_VIDEO` in the fork (the fork already carries GPU work from the
   previous map's ticket 10).
2. **Render-server fd ownership**: `CANG_RENDER_SERVER_FD` ->
   `GpuDevice::set_render_server_fd(RawFd)` which takes the fd into an `OwnedFd`.
   Check who closes it now, and that the fd stays owned for the VM's life.
3. **The managed kernel console file** currently must stay open until
   `vmm_builder_build` duplicates the fd; confirm the Rust path's ordering keeps
   that true (the launcher's `_console_log` guard).
4. **`nested_virt`, `set_profile_path`, `check_nested_virt`, `init_log`** and the
   `VmmError` -> `anyhow` mapping: port each and keep the existing log/level
   behaviour.
5. **`preload_libva`**: with libkrun static, `libvirglrenderer.so.1` becomes a
   `DT_NEEDED` of cang rather than of a `dlopen`ed `libkrun.so` with an
   `RTLD_NOW` symbol-resolution order, so the `libva-drm.so.2`-before-`libva.so.2`
   workaround may simply not be needed. Prove it (or keep it) with the ticket 09
   GPU smoke rather than by inspection.

Done when: `--gpu=drm` and `--gpu=off` both launch a guest through the Rust-API
backend, the venus render server is reached, and any leftover workaround has a
measured reason for existing.

## Resolution

**All five answered in code; the GPU *run* is ticket 09's (the smoke), not this
ticket's.**

1. **Flags**: `VirglRendererFlags::from_bits_retain(bits)` in
   `crates/cang-libkrun/src/linked.rs`, keeping cang's
   `VIRGLRENDERER_VENUS_FLAGS` (including `DRM` and `USE_VIDEO`) exactly as the C
   ABI passed it. No fork change - the whole point of the migration is fewer fork
   deltas, and a flag-bit change in the fork would have to be mirrored back.
2. **Render-server fd**: `GpuDevice::set_render_server_fd(i32)` converts the
   launcher's `CANG_RENDER_SERVER_FD` into an `OwnedFd` and the device holds it,
   so the `GpuDevice` in the arena owns the fd for the VM's life and drops it
   after the VM stops. The launcher keeps no second owner, and nothing closes it
   early - strictly better than the C ABI, where ownership was a comment.
3. **Managed kernel console file**: unchanged in shape. `configure_console` still
   returns the `File` and the launcher still holds it (`console_log`) past
   `MmioDeviceManager::add(... gpu/console ...)`; the Rust console device
   *duplicates* the fd at `add_inout_port` (`port_io::output_to_raw_fd_dup`), so
   the ordering the guard protects still holds. Note the Rust `add_inout_port`
   takes `BorrowedFd<'a>` with `'a` tied to the device manager's lifetime, which
   the backend satisfies by borrowing the still-open `File` for the call.
4. **The rest of the surface**: `nested_virt` (with `check_nested_virt` staying a
   diagnostic that never gates), `set_profile_path`, `init_log`, and the
   `VmmError` -> `anyhow` mapping all ported into `linked.rs`, preserving the
   existing log messages and levels; the launcher's fixtures still assert the
   call order, which is what proves the sequence did not drift.
5. **`preload_libva` is deleted, not kept.** With libkrun linked in,
   `libvirglrenderer.so.1` is a `DT_NEEDED` of cang resolved before `main`, so the
   old RTLD_NOW symbol-resolution ordering problem (libva-drm before libva) is
   structurally gone. The smoke is what closes this item out, and it lives in
   ticket 09 - if the venus path regresses, this is the paragraph to revisit.
