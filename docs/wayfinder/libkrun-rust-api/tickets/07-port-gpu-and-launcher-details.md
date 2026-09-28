---
label: wayfinder:task
title: Port the GPU/launcher details, and re-verify the libva preload
status: open
blocked_by: ["05-create-cang-libkrun-crate"]
claimed_by:
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
