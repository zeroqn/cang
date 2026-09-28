---
label: wayfinder:task
title: GPU smoke the new pin
status: closed
blocked_by: ["07-pin-and-adapt-cang"]
claimed_by: pi session (2026-09-28)
---

## Question

Prove the rebased libkrun still does GPU/wayland work on this host, honestly:

1. Run `tools/chromium-cang-smoke` (GPU and wayland modes) against the new pin
   on the btrfs-backed live-VM setup. The PR-822 guest-handle fast path is **not**
   part of this map, so the smoke cannot and must not be expected to exercise it.
2. Attribute any failure: is it introduced by the rebase, by a cherry-picked PR,
   or pre-existing? When unsure, run the same smoke against the **old pin**
   (v1.19.5-cang.1) to separate the two, rather than assuming.
3. Record the result: the smoke's verdict, the exact revision pair exercised,
   and where the baseline lives.

Standing rule for this map: never weaken a smoke assertion to force a green run;
report the failure with its attribution instead.

## Deliverable

The smoke verdict per mode, the A/B attribution when something fails, and the
location of the recorded baseline/evidence.


## Resolution (2026-09-28, pi session) - run, attribution, fix; blocked on the release

**Ran** both modes (GPU and `--waypipe`) on the pinned `v2.0.0-cang.2` against the
same image and kernel as the pre-rebase baseline: **FAIL** - the guest reports
`renderer=no-webgl` (ANGLE `Internal Vulkan error (-3)` at `vk_renderer.cpp
initialize`), and the host log shows `failed to initialize drm renderer` ->
`falling back to 2d` -> the guest's capset enumeration timing out. The baseline
(`/home/dev/cang/disk/chromium-smoke/kernel-6.12.109e` and
`kernel-6.12.109-waypipe`, 2026-09-26, libkrun `1.19.5-cang` + the same
virglrenderer 1.3.0) passed with
`Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33))`, so this is a **rebase
regression**, not the host, the image, the guest mesa or the kernel. Assertions
were not touched.

**Attributed and fixed** in the fork's device code (submodule commit `3d7af2c2`):

1. the ABI-2 device registered no DRM render node for rutabaga's `get_drm_fd`, a
   job the fork's vendored rutabaga used to do itself, so virglrenderer's vrend
   winsys and VA-API video got `-1`;
2. upstream rutabaga always sets `ASYNC_FENCE_CB`, and virglrenderer's
   `ASYNC_FENCE_CB | DRM` branch calls `drm_renderer_init`, whose failure (a
   native amdgpu probe that cannot succeed without a virtio native context) is
   fatal for the whole backend - 2D fallback, venus gone;
3. the device's `num_capsets` and rutabaga's capset list had to be derived from
   one mask, or the guest asks for an index that does not exist and the device
   never answers.

**Verified locally** (unpublished library injected through
`CANG_LIBKRUN_LIBRARY`): both modes **PASS** with the same renderer string as the
pre-rebase baseline - GPU mode 4 PASS / verdict PASS (14 evidence files), waypipe
mode 9 PASS incl. `waypipe-transport`, `venus-presenting`, `frame-presented`,
`renderer-on-frame`, `control-no-frame` / verdict PASS (25 evidence files).
Details and the log excerpts are in
[notes/08-gpu-smoke.md](../notes/08-gpu-smoke.md).

**Closed against the published pin.** `v2.0.0-cang.3` (tag on the fork's
`3d7af2c2`, CI success) is pinned, `nix build .#cang` links it, and both smoke
modes **PASS against the pinned artifact**:

- GPU mode: 4 PASS, verdict PASS, 14 evidence files,
  `renderer=ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT
  (RADV NAVI33)) (0x00007480)), venus)` - byte-identical to the pre-rebase
  baseline's renderer string;
- `--waypipe`: 9 PASS (incl. `waypipe-transport`, `venus-presenting`,
  `frame-presented`, `renderer-on-frame`, `control-no-frame`), verdict PASS, 25
  evidence files.

Evidence lives in
`/home/dev/cang/disk/chromium-smoke/pinned-c3-gpu` and
`/home/dev/cang/disk/chromium-smoke/pinned-c3-waypipe`; revision pair: cang
`0.7.2` (pinned libkrun `v2.0.0-cang.3`, libkrunfw `v5.6.2-cang.1`) against the
unchanged `localhost/cang:latest` image.
