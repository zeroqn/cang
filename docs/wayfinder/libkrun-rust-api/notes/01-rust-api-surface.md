# libkrun's Rust API vs the surface cang binds today

Evidence gathered 2026-09-28 from `deps/libkrun` (fork `cang`, based on upstream
`main` = the 2.0.0 / ABI-2 line) while charting this map.

## The Rust API is the source; the C ABI is a projection of it

- `deps/libkrun/src/libkrun/Cargo.toml` builds `crate-type = ["cdylib", "lib"]`,
  so the same crate is usable as an rlib. The whole `krun_*` C surface is
  generated from the Rust API by `ffier` behind the `ffi` feature
  (`src/libkrun/src/api/mod.rs`, and `make gen-libkrun-bindings` writes the
  schema + header). `deps/libkrun/CANG.md`: "The C names are regenerated from the
  Rust API ... so they are `krun_<object>_<method>`".
  **Consequence:** the C ABI carries no stability the Rust API does not. Binding
  a frozen generated schema buys a little churn tolerance for a *runtime*
  consumer; it buys nothing for a consumer that compiles the source.

## Upstream already exercises a statically linked Rust-API consumer

`deps/libkrun/tests/test_cases/Cargo.toml` has two mutually exclusive features
over the same test bodies:

- `static-linking = ["host", "krun", "krun/blk", "krun/net", "krun-init-blob"]`
- `dynamic-linking = ["host", "krun-cdylib", "krun-init-cdylib"]`

`tests/test_cases/src/common.rs` branches on them (e.g. `require_vm_symbols` is a
no-op in static mode). So `VmmBuilder` + `krun-init-blob` driven directly from
Rust is a CI-covered consumer of the same VM construction cang needs - and
`tests/test_cases/Cargo.toml` pulls `krun-init-blob` with `features = ["direct"]`.
Note the static feature set only enables `krun/blk` and `krun/net`: **gpu, input
and timesync have no upstream static consumer**, so cang is first there and the
GPU smoke is the backstop.

## Call-by-call coverage of cang's current `LibkrunApi`

`crates/cang/src/runtime/vm/libkrun/api.rs` is already a Rust-shaped mirror of
the ABI-2 C surface (`Handle = usize`, "the library may rebox the handle"), which
is why the mapping is close to 1:1:

| cang (`api.rs`) | Rust API (`deps/libkrun/src/libkrun/src/api/`) |
| --- | --- |
| `init_log(level)` | `logging::init_log(?, LogLevel, LogStyle, LogOptions)` |
| `check_nested_virt` | `vmm_builder::check_nested_virt()` |
| `payload_load_krunfw` | `Payload::load_krunfw()` (dlopens `libkrunfw.so.5` itself) |
| `payload_append_cmdline` | `Payload::append_cmdline(&str)` |
| `fs_overlay_new` | `FsOverlay::new()` |
| `init_*_builder_*` | `krun_init_blob::Config::builder()` + `.arg/.args/.env/.workdir/.rlimit(s)/.dhcp/.build()` |
| `init_config_apply_in` | `krun_init_blob::Config::apply(&mut FsOverlay, &mut Payload)` (`direct` feature) |
| `mmio_device_manager_new/add` | `MmioDeviceManager::new()` / `.add(impl AttachDevice)` |
| `fs_device_new` / `_set_overlay` | `FsDevice::new/_new_read_only/_new_null` + `.set_overlay(FsOverlay)` |
| `block_device_new` / `_set_read_only` | `BlockDevice::new(id, path, DiskFormat)` + `.set_read_only(bool)` |
| `net_device_new_unixstream_fd` | `NetDevice::new_unixstream_fd(...)` |
| `vsock_device_new` / `add_unix_port` / `add_port_forward` | `VsockDevice::new(cid, TsiFlags)` + `.add_unix_port/.add_port_forward` |
| `console_device_builder` / `add_default_console` / `add_inout_port` / `build` | `ConsoleDevice::builder()` + `.add_default_console/.add_inout_port/.build()` |
| `display_backend_new` | `DisplayBackend::new(*const c_void, usize)` - **still a raw vtable**, cang's headless vtable survives verbatim |
| `gpu_device_new` / `shm_size` | `GpuDevice::new(VirglRendererFlags, DisplayBackend)` + `.shm_size(bytes)` |
| `gpu_device_set_render_server_fd` (fork) | `GpuDevice::set_render_server_fd(RawFd)` (builds an `OwnedFd`) |
| `vmm_builder_*` / `vmm_builder_destroy` / `vmm_builder_build` / `vmm_run` | `VmmBuilder{new,vcpus,ram_mib,payload,devices,nested_virt,set_profile_path,build}` / `Vmm::run()` (consumes; never returns) |

Two shape differences that drive ticket 07:

1. **`VirglRendererFlags` is narrower than the raw u32 cang passes today.**
   Rust has `USE_EGL`, `THREAD_SYNC`, `VENUS`, `USE_ASYNC_FENCE_CB`,
   `RENDER_SERVER`; cang's `VIRGLRENDERER_VENUS_FLAGS`
   (`crates/cang/src/runtime/vm/libkrun/launcher.rs`) also sets `DRM` (1<<10) and
   `USE_VIDEO` (1<<11). bitflags' `from_bits_retain` covers it, or the fork adds
   the bits (the fork already carries fork-only GPU work - see
   `../libkrun-main-rebase/tickets/10-fork-c-extensions-on-main.md`).
2. **Ownership replaces handles.** `VmmBuilder<'a>` consumes and returns itself,
   `Payload` is moved into the builder, `FsDevice<'a>`/`ConsoleDevice<'a>` carry
   lifetimes and are handed to `MmioDeviceManager::add`, the render-server fd
   becomes an `OwnedFd`, and `krun_init_blob::Config` must outlive the VM. cang's
   launcher currently threads a `Handle` table and destroys on error, which is
   the C-ABI shape.

## Init injection without `libkrun_init.so`

`krun-init-blob` (`deps/libkrun/init/init-blob`) is the crate behind
`libkrun_init.so`; it embeds the musl guest init (`INIT_BINARY`,
`KRUN_INIT_BINARY_PATH`) and exports `INIT_PATH = "/init.krun"` and
`KERNEL_INIT_ARG = "init=/init.krun"`. Its `direct` feature is exactly the
statically linked path:

```rust
// init/init-blob/src/config.rs (feature = "direct")
pub fn apply<'a>(&'a self, overlay: &mut krun::FsOverlay<'a>, payload: &mut krun::Payload) -> Result<(), ApplyError>
```

The `ffi-client` variant instead looks its two libkrun symbols up with `dlsym`
(`krun_fs_overlay_add_file`, `krun_payload_append_cmdline`, via
`bindings/libkrun-via-cdylib-weak`), which is the coupling that breaks when both
sides are static - the reason the static-C-ABI route needed fork work and this
route does not.

One wrinkle for ticket 03: `krun-init-blob/Cargo.toml` declares `ffier` as a
**non-optional** dependency even though every use of it is behind
`#[cfg(feature = "ffi")]`, so a git dependency lands in cang's graph regardless.
`optional = true` + `ffi = ["ffi-client", "dep:ffier"]` in the fork removes it.
