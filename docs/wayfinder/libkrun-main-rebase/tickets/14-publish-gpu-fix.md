---
map: libkrun-main-rebase
id: "14-publish-gpu-fix"
title: Publish the GPU fixes as v2.0.0-cang.3 and re-pin
status: open
blocked_by: ["08-gpu-smoke-against-abi2"]
claimed_by: unclaimed
---

# Publish the GPU fixes as v2.0.0-cang.3 and re-pin

## Intent

The ABI-2 GPU fixes from ticket 08 only exist as a local submodule commit
(`3d7af2c2`). The pinned `v2.0.0-cang.2` still degrades to 2D, so Chromium's
WebGL is dead on the pinned release. Make the fix the pin.

## Work

1. Push the fork branch (`cang` -> `3d7af2c2`) and publish the permanent tag
   **`v2.0.0-cang.3`** (the release workflow builds with `FFI=1` and asserts the
   exported ABI; the tag must be a new one because `.2` is already published at a
   different commit).
2. `./scripts/update-libkrun.sh --tag v2.0.0-cang.3` (both Linux systems in one
   run), then confirm on the *pinned* artifact that
   `krun_gpu_device_set_render_server_fd` is exported.
3. Re-run both smoke modes against the pinned build
   (`tools/chromium-cang-smoke/chromium-smoke.sh` GPU and `--waypipe`), which is
   what ticket 08 asks for, and record the evidence.
4. Refresh `docs/maintenance.md`'s release examples to `v2.0.0-cang.3` and close
   tickets 08/14.

## Notes

- The GPU fix is device-internal, so no header/schema regeneration is involved;
  `nix build .#cang` plus the smoke runs are the gate.
- Ticket 08's evidence already shows both modes passing against this library
  (identical renderer string to the pre-rebase baseline), so a re-run here is
  confirmation against the *published* artifact rather than new diagnosis.
