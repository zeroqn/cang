# 07 - fork VMM port: progress (2026-09-28)

Ticket `tickets/07-fork-vmm-port.md`, design per ticket 04. `cargo check -p cang`,
`cargo fmt --check` and the touched unit tests pass in the repo devshell.

## `deps/libkrun` (uncommitted at the time of writing)

- `src/devices/Cargo.toml`: both `rutabaga_gfx` dep sites are now
  `git = "https://github.com/magma-gpu/rutabaga_gfx", rev = "ec60ee11..."` (main,
  2026-09-25). `Cargo.lock` records `rutabaga_gfx` and its `magma-gpu` path dep
  as git sources.
- `src/utils/src/linux/udmabuf.rs` (new): `UdmabufDriver` (the PR's file, minus
  its debug `log::warn!`, plus `libc::sysconf(_SC_PAGESIZE)` instead of
  `nix::unistd::sysconf`, which is not available through nix 0.30 here).
- `src/devices/src/virtio/gpu/` (`device.rs`, `worker.rs`, `virtio_gpu.rs`): the
  `udmabuf_driver` field/parameter plumbing, and the `resource_create_blob` arm
  that replaces the `panic!("GUEST_HANDLE unimplemented")` with a real handle.
- **Feature bits**: the device advertises `CREATE_GUEST_HANDLE` (6) **and**
  `BLOB_CTX_ID_FIX` (7) only when the driver opened, so the guest kernel only
  stamps the flag (and only passes a `ctx_id`) when the host can serve it.
- **Failure is loud, not silent**: a udmabuf that cannot be created fails the
  blob request instead of creating a handle-less blob, because the guest has
  already switched to its zero-copy path (ticket 10's wrong-pixels mode).
- **Mis-route warning**: the device records each context's capset id and warns
  when a guest-handle blob's ctx is not the cross-domain one (rutabaga would
  route it to the default component and drop the handle).
- `src/libkrun/src/api/device_builders.rs`: `DeviceRequirements.zero_copy_shm`,
  `GpuDevice::set_zero_copy_shm` (the `/dev/udmabuf` probe lives here, so the
  advertised bits and the RAM backing cannot disagree).
- `src/libkrun/src/vmm/builder.rs`: `use_gpu_udmabuf` from the requirements
  drives file-backed guest RAM, with `MFD_ALLOW_SEALING` +
  `F_SEAL_GROW|F_SEAL_SHRINK` only on the udmabuf path (vhost-user keeps its
  unsealed memfds).

## `cang`

- `--zero-copy-shm` CLI flag (errors without `--gpu=drm`), plumbed through
  `RuntimeOptions` -> plan -> `LaunchConfig` -> the launch-config codec ->
  `crates/cang-libkrun` -> `GpuDevice::set_zero_copy_shm`. `README.md` documents
  it, including the balloon trade.
- Landlock: `/dev/udmabuf` is granted read-write only when `zero_copy_shm` is
  set, and `classify_fd` now classifies that descriptor as benign - without it a
  `--landlock=all --zero-copy-shm` run would refuse its own descriptor as an
  "unexpected retained fd" (the pre-enter hook runs after device setup).
- Seccomp needs no change: the packaged policy allows `memfd_create`, `fcntl`,
  `ioctl` (unfiltered), `ftruncate`, `mmap` and `madvise` already.

## Still open

- The two `fetchCargoVendor` hashes (`nix/pkgs/cang-rust.nix`,
  `nix/pkgs/libkrun-source.nix`) move with the rutabaga rev and have not been
  refreshed; they can only be computed once the fork changes are committed
  (nix copies tracked files only).
- Committing the fork changes, moving the submodule pointer, and the
  `bindings/ffier-krun.json` + `include/libkrun.h` regeneration
  (`make gen-libkrun-bindings`) for the new setter.
- cang's logging: libkrun logs through `log`, cang installs only
  `tracing_subscriber`, so the fork's warnings (the probe failure, the
  mis-route) are **invisible** today. Needs a decision.
