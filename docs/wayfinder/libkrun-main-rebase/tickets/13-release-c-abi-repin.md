---
label: wayfinder:task
title: Rebuild the v2 release with the C ABI and re-pin
status: closed
blocked_by: ["09-port-cang-to-v2-api"]
claimed_by: pi session (2026-09-28)
---

## Question

The published `v2.0.0-cang.1` cannot be used by cang: its `libkrun.so.2.0.0`
exports **no** `krun_*` symbol, because ABI 2 put the C entry points behind the
`ffi` cargo feature and the fork's `Makefile` only enables it for `FFI=1`.
`.github/workflows/publish-cang-release.yml` builds with

```
make BLK=1 NET=1 GPU=1 INPUT=1 TIMESYNC=1
```

and only asserts that `libkrun.so*`/`libkrun_init.so*` *exist* in the asset
(`test -f …`), so the release shipped with a hollow host library while the init
blob (`INIT_BLOB_FEATURE_FLAGS` always adds `ffi`) was fine. Evidence and the
symbol counts are in `notes/09-v2-api-port.md`.

Fix the pipeline and re-publish:

1. `make … FFI=1` in `publish-cang-release.yml`, plus an asset assertion that the
   produced `libkrun.so.2` actually exports the ABI (e.g. `nm -D --defined-only`
   lists `krun_init_log`, `krun_vmm_builder_new`, `krun_vmm_run`). The same
   assertion belongs in `nix/dev`'s or the container's checks so this cannot come
   back silently.
2. Decide whether the corrected release also carries ticket 10's re-added
   extensions (`krun_gpu_device_set_render_server_fd`,
   `krun_vmm_builder_set_profile_path`) - one `v2.0.0-cang.2` for both is cheaper
   than two releases, and ticket 08 needs the render-server fd anyway.
3. Publish (`v<base>-cang.<n>` tag push, or bob's `workflow_dispatch`),
   re-pin `nix/pins.nix`, move the `deps/libkrun` submodule pointer to the same
   commit, and re-verify the asset's symbol table and `nix build .#cang`.
4. Keep ticket 08 honest: the GPU smoke must run against the re-pinned release,
   not the local build.

## Deliverable

A release whose `libkrun.so.2` exports the ABI (symbol-table evidence), `nix/pins.nix`
and the submodule pointer naming it, and `nix build .#cang` producing a cang that
gets past `krun_init_log` resolution.


## Resolution (2026-09-28, pi session)

**`v2.0.0-cang.2` is published, pinned and live-booted.**

1. The fork's release workflow now builds with `FFI=1` and asserts that
   `libkrun.so.2` exports `krun_init_log`,
   `krun_vmm_builder_new`/`_build`/`run`, `krun_gpu_device_new` and that
   `libkrun_init.so.0` exports its builder/apply pair; `binutils` was added to the
   setup action. It carries ticket 10's re-added extensions, so one release
   covered both.
2. The branch (`cang` @ `18267332`) and the permanent tag **`v2.0.0-cang.2`** were
   pushed. CI run
   [36371495907](https://github.com/zeroqn/libkrun/actions/runs/36371495907)
   succeeded and published both assets plus the init blob, pc files and headers.
3. Re-pinned with `./scripts/update-libkrun.sh --tag v2.0.0-cang.2` (both Linux
   systems in one run). Measured on the *pinned* artifact:
   `libkrun.so.2.0.0` exports **101** `krun_*` symbols including
   `krun_gpu_device_set_render_server_fd` and `krun_vmm_builder_set_profile_path`,
   and `libkrun_init.so.0.1.0` exports 33 `krun_init_*` - against 0 and 31 for
   `v2.0.0-cang.1`.
4. `nix build .#cang` links the pinned library and **boots**: a `--mem 4
   --seccomp=off --landlock=off` run reaches the guest with
   `uname -r` 6.12.109-hardened1, `/dev/hvc0`, `/dev/vsock`, the pinned fw and the
   guest identity set up (see ticket 05's resolution, which is the same run).

Frontier after this: **ticket 08** (the GPU smoke), which needs this pin.
