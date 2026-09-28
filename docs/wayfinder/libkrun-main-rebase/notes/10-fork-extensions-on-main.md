# 10 — Re-adding the fork's C extensions on main's device code

Ticket `10-fork-c-extensions-on-main`, map `docs/wayfinder/libkrun-main-rebase`.
Implementation session 2026-09-28, on `deps/libkrun` branch `cang`
(base `a980e779` + PRs 865/840 + the fork CI/docs commit `d578e4e2`).

## File-level mapping (v1 entry point -> ABI-2 implementation)

| v1 fork entry point / commit | ABI-2 implementation |
|---|---|
| `krun_set_gpu_options3(ctx, flags, shm, fd)` (`dacdbac4`, hardened by `2a583f8a`) | `GpuDevice::set_render_server_fd(fd)` in `src/libkrun/src/api/device_builders.rs` (generates `krun_gpu_device_set_render_server_fd(GpuDevice*, int, KrunError*)`), storing `Option<OwnedFd>`; `GpuDevice::attach` hands it to `devices::virtio::Gpu::new` |
| the fd's path through `Gpu` -> `Worker` -> `VirtioGpu` into `RutabagaBuilder::build(fence, descriptor)` | same shape, but the descriptor is now a builder setter: `RutabagaBuilder::set_server_descriptor(Some(descriptor))`, then `build()` (crates.io `rutabaga_gfx` 0.1.85; `RutabagaDescriptor::from_raw_descriptor`) |
| fence retirement (`7f77a0ac`) | `VirtioGpu::event_poll()` + `VirtioGpu::poll_descriptor()` (new) and the worker's poll loop in `src/devices/src/virtio/gpu/worker.rs` |
| poll/fence/lock hardening (`2a583f8a`) | the fence descriptor is kept alive for the worker's lifetime (`AsFd`), the fence handler recovers poisoned locks |
| idle poll + blob-map/cookie fixes (`7c20aa6f`) | `VirtioGpu::has_pending_fence()` + the two-tier poll timeout. The blob-map overflow helper, the DRM render-node gating, the cookie reclaim and the `O_RDWR` render-node open are **not** re-added: they live in `rutabaga_gfx`, which crates.io 0.1.85 already carries (triage note 04) |
| `krun_set_profile_path(ctx, path)` (`ac615be3`) | `VmmBuilder::set_profile_path(path)` (generates `krun_vmm_builder_set_profile_path(VmmBuilder*, KrunStr, KrunError*)`) + `src/libkrun/src/vmm/profile.rs` + `measure_builder_phase` calls in `vmm/builder.rs` and `api/vmm_builder.rs` |
| `krun_set_kernel_cmdline_append(ctx, fragment)` (`329f14db`) | **not re-added**: ABI 2 exposes `krun_payload_append_cmdline`, which cang already calls for its profile-only kernel logging flags |
| the rest of the v1 surface (nested-virt gate, console variants, vsock/port-map) | native ABI-2 API (`krun_vmm_builder_nested_virt`, the console builder, `krun_vsock_device_add_unix_port`/`_add_port_forward`), so nothing is re-added |

## The release-build gap that came with it

`v2.0.0-cang.1` shipped a `libkrun.so.2.0.0` that exports **no** `krun_*` symbol:
ABI 2 gates the whole C surface behind the `ffi` cargo feature, the release
workflow never passed `FFI=1`, and its packaging assertions only checked that
`libkrun.so*` / `libkrun_init.so*` exist. `nix/dev`'s local build had the same
hole (see `notes/09-v2-api-port.md` for the symbol counts). Fixed here:

- `make ... FFI=1` in `.github/workflows/publish-cang-release.yml`;
- an assertion that `libkrun.so.2` exports `krun_init_log`,
  `krun_vmm_builder_new`/`_build`/`run`, `krun_gpu_device_new` and that
  `libkrun_init.so.0` exports its builder/apply pair (`binutils` added to
  `setup-build-env`);
- `nix/dev/flake.nix`'s local libkrun build now passes `FFI=1` too (that change is
  in the cang repository, ticket 09's commit).

## Verification (local, before the release)

- `make gen-libkrun-bindings` regenerates `include/libkrun.h` +
  `bindings/libkrun-via-cdylib-weak/ffier-krun.json`; both new C signatures match
  what cang binds by name.
- `nix build` of the fork's libkrun with `FFI=1` (cang's `devShell`-less route:
  a throwaway flake pointing `libkrunSrc` at the live worktree) compiles
  `net,blk,gpu,input` and produces **101** exported `krun_*` symbols, including
  `krun_gpu_device_set_render_server_fd` and `krun_vmm_builder_set_profile_path`.
- `cargo clippy --locked --features net,blk,gpu,input -- -D warnings` clean (the
  same feature set CI lints), `cargo fmt --all -- --check` clean.
- Live boot of cang against that library: `--gpu=drm` reaches the guest with
  `/dev/dri/card0`, `/dev/dri/renderD128` and the venus virtio ICD
  (`/usr/lib/cang-mesa-runtime/share/vulkan/icd.d/virtio_icd.x86_64.json`), exit 0
  - which could not have succeeded before, because cang fails loudly when
  `krun_gpu_device_set_render_server_fd` is missing.
- `--profile` run: the launch-profile artifact
  (`<task>/vm-worker-host-profile.tsv`) contains the rows the fork's profiler
  emits, e.g. `libkrun_build_vm_enter`, `libkrun_build_vm_event_manager_create`,
  `libkrun_build_microvm_choose_payload`,
  `libkrun_build_microvm_create_guest_memory`,
  `libkrun_build_microvm_attach_devices`,
  `libkrun_build_microvm_start_vcpus`,
  `libkrun_build_microvm_register_event_subscriber`.

Full venus/WebGL evidence (Chromium) stays ticket 08, which must run against the
re-pinned release rather than this local build.
