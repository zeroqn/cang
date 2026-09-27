---
label: wayfinder:task
title: Rebuild the v2 release with the C ABI and re-pin
status: open
blocked_by: ["09-port-cang-to-v2-api"]
claimed_by: unclaimed
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
