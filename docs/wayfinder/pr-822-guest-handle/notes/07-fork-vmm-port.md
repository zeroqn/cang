# 07 - fork VMM port: progress (2026-09-29)

Ticket `tickets/07-fork-vmm-port.md`, design per ticket 04. The VMM half is
committed in `deps/libkrun`, the submodule pointer is moved, both
`fetchCargoVendor` hashes are refreshed, and `nix build .#cang` is green
(`/nix/store/p78385yi099hddafyddxdwp8k64wd3ll-cang-0.9.1`).

## `deps/libkrun` (committed)

- `a1a772a0` **gpu: carry PR 822's udmabuf zero-copy SHM path behind a gate** -
  the whole VMM port, per ticket 04's design, replacing PR 822's three
  component-routing hunks with magma-gpu/rutabaga_gfx#81's `ctx_id` route:
  - `src/devices/Cargo.toml`: `rutabaga_gfx` moves from crates.io `0.1.85` to
    `git = "https://github.com/magma-gpu/rutabaga_gfx", rev = "ec60ee11..."`.
  - `src/utils/src/linux/udmabuf.rs` (new): `UdmabufDriver`, with
    `libc::sysconf(_SC_PAGESIZE)` instead of `nix::unistd::sysconf` (that axis
    of nix 0.30 is feature-gated here and does not compile).
  - `src/devices/src/virtio/gpu/` (`device.rs`, `mod.rs`, `worker.rs`,
    `virtio_gpu.rs`): `udmabuf_driver` plumbing and a real
    `resource_create_blob` guest-handle arm.
  - **Feature bits** `CREATE_GUEST_HANDLE` (6) **and** `BLOB_CTX_ID_FIX` (7)
    are advertised only when the driver opened, so the guest kernel only stamps
    the flag (and passes `ctx_id`) when the host can serve it. Bit 5 stays
    `FENCE_PASSING`; upstream's `BLOB_ALIGNMENT = 5` is not imported.
  - **Failure is loud**: a udmabuf that cannot be created fails the blob request
    instead of creating a handle-less blob (the guest has already switched to
    its zero-copy path).
  - **Mis-route warning**: a guest-handle blob whose ctx is not the
    cross-domain one is warned about rather than silently losing its handle.
  - `src/libkrun/src/api/device_builders.rs`: `DeviceRequirements.zero_copy_shm`
    and `GpuDevice::set_zero_copy_shm`, where the `/dev/udmabuf` probe lives, so
    the advertised bits and the RAM backing cannot disagree.
  - `src/libkrun/src/vmm/builder.rs`: the requirements bool drives file-backed
    guest RAM with `MFD_ALLOW_SEALING` + `F_SEAL_GROW|F_SEAL_SHRINK` only on the
    udmabuf path (vhost-user keeps its unsealed memfds).
- `5d9cb075` **chore: refresh the lock for the new utils deps and regenerate the
  ffier bindings** - `krun-utils` gained `thiserror`/`vm-memory` and the C
  header/schema needed `krun_gpu_device_set_zero_copy_shm`. Tip: `5d9cb075`.

## `cang`

- `f62b3ca` **cang: add `--zero-copy-shm` and move the libkrun/libkrunfw
  pointers** - the flag (errors without `--gpu=drm`) through `RuntimeOptions` ->
  plan -> `LaunchConfig` -> the launch-config codec -> `crates/cang-libkrun` ->
  `GpuDevice::set_zero_copy_shm`; `README.md` documents it including the balloon
  trade. Landlock grants `/dev/udmabuf` read-write only when `zero_copy_shm` is
  set, and `classify_fd` classifies that descriptor as benign (otherwise a
  `--landlock=all --zero-copy-shm` run refuses its own descriptor: the pre-enter
  hook runs after device setup). Seccomp needs no change - the packaged policy
  already allows `memfd_create`, `fcntl`, unfiltered `ioctl`, `ftruncate`,
  `mmap` and `madvise`. This commit also carries the `deps/libkrun` and
  `deps/libkrunfw` pointer moves.
- `761c6b9` **cang: bridge libkrun log records into cang tracing** - later
  **reverted** (see the live-boot finding below): libkrun's own `krun_init_log`
  installs an env_logger with `try_init()` and treats "logger already set" as
  fatal, so the `tracing-log` `LogTracer` made every boot die. The problem it
  aimed at is fixed instead by `84b9b9d` - libkrun's own log level is floored at
  `warn`, so the fork's warnings (`--zero-copy-shm` falling back to the copy
  path, a failed GPU backend, a mis-routed handle) always reach stderr while
  cang's tracing still honours `--log-level`.
- `127313d` **deps: move the libkrun pointer past the lock and bindings refresh.**

## Vendored-crate hashes (refreshed)

| file | attr | hash |
|---|---|---|
| `nix/pkgs/cang-rust.nix` | `cargoDeps` (src = the repo) | `sha256-CmzlNqPRBTDmTQp00s6WUP6nIJ4jW8IjdgKWqAu1yzE=` |
| `nix/pkgs/libkrun-source.nix` | `libkrunCargoDeps` (src = `deps/libkrun`) | `sha256-5Snz7O5nbcg0qVgLPhSzFdGUk5+pqy+Iavt0mE3FLaQ=` |

**Trap (cost two build cycles):** both `fetchCargoVendor` calls are named
`cargo-deps-vendor`, and a fixed-output derivation's output path is computed from
the *name plus the specified hash only* - not from `src`. Two of them sharing a
placeholder hash therefore share one output path, so the first content that
builds satisfies both and the second is never checked; the run then dies later in
`buildRustPackage` with "Cargo.lock is not the same in /build/cargo-deps-vendor",
which reads like a stale hash but is really a cross-file hash collision. Harvest
each hash with its *own* placeholder (two distinct fake strings, or one real and
one fake) so the two never meet at one store path.

## Build evidence

- `nix build .#cang` (both hashes refreshed): green, out path
  `/nix/store/p78385yi099hddafyddxdwp8k64wd3ll-cang-0.9.1`; the build ran
  `buildRustPackage`'s lock-consistency check against both vendor dirs
  (`krun-init-static` against `/build/libkrun/Cargo.lock`, `cang` against
  `/build/source/Cargo.lock`), so both hashes are content-correct, not just
  accepted.
- `cargo check -p cang`, `cargo fmt --check` and the touched unit tests pass in
  the repo devshell (earlier run, before the commits).

## Live-boot finding (2026-09-29)

The first live VM on this tree died before boot:
`cang sandboxed VM worker: libkrun setup failed: krun_init_log: internal error:
logger init: attempted to set a logger after the logging system was already
initialized`. `761c6b9` was the cause: cang's `tracing_log::LogTracer::init()`
owns the `log` logger, and libkrun's `env_logger::Builder::try_init()` in
`krun_init_log` then fails with `VmmError::Internal`, which cang's launcher turns
into a hard setup error. No unit test launches a VM, so only a boot caught it.

`84b9b9d` reverts the bridge and the `tracing-log` dependency and floors the
level cang passes to libkrun at `warn` (`LogLevel::libkrun_log_level`), since
libkrun's diagnostics never went through cang's subscriber in the first place -
they were merely level-gated by cang's default `--log-level off`. That commit
also refreshes `nix/pkgs/cang-rust.nix`'s `cargoDeps` (the lock loses
`tracing-log`/`valuable`), and `docs/diagnostics.md` records the floor.

## Validation gates (repo devshell, 2026-09-29)

- `nix build .#cang`: green (above).
- `cargo fmt --check`: clean.
- `cargo clippy --all-targets --all-features -- -D warnings`: clean (45 s,
  `Finished \`dev\` profile`).
- `cargo test`: green - `597 passed` (cang; +1 for the new
  `libkrun_log_level_is_floored_at_warn`), `39` + `5` + `296` + `4`
  (cang-libkrun / cang-guest-init / cang-repository-tests), `47` in the
  repository-invariant suite; 0 failed, 0 ignored.
- `cargo deny check`: advisories/bans/licenses/sources ok.

## Live boot (after the fix)

A cang guest boots on a firmware built from this tree (see
[`notes/05`](05-libkrunfw-kernel-support.md)): `uname -r` = `6.12.109-hardened1`,
guest `/dev/udmabuf`, virtio-gpu bits `0-4 + VERSION_1`, and with
`--zero-copy-shm` additionally bits 6/7 with `VIRTGPU_PARAM_CREATE_GUEST_HANDLE`
answering 1.

## Still open

- Ticket 05's boot test: a cang guest on the locally built libkrunfw firmware
  (needs a libkrunfw built from `deps/libkrunfw`'s new `3fdbb59`, not the pinned
  prebuilt).
- Ticket 06's release + per-system repin (bob tags; the LTO assets have to be
  built by CI or locally).
- Ticket 09's live fast-path verification, including the A/B `wl_shm` client
  that does not exist yet.
