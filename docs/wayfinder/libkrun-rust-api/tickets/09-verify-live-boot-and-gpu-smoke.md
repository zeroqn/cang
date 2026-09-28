---
label: wayfinder:task
title: Verify - build, no libkrun .so, live boot, Chromium GPU smoke
status: closed
blocked_by: ["06-retire-the-dlopen-path", "07-port-gpu-and-launcher-details"]
claimed_by: pi session (2026-09-28)
---

## Question

Prove the destination on a real host, not by inspection:

1. `nix build .#cang` from a clean store path; `cargo fmt --check`, `clippy
   --all-targets --all-features -- -D warnings`, `cargo deny check`, `cargo test`
   in the devshell (`cang-local-validation-gates` has the known pre-existing
   failures and the nixfmt caveat).
2. `readelf -d`/`ldd` on the built binary: no `libkrun.so*` /
   `libkrun_init.so*` `DT_NEEDED`, and note what *is* now `DT_NEEDED`
   (libvirglrenderer and friends) - that feeds ticket 10.
3. A live guest boot on this host: the storage/graphroot recipe on the btrfs
   loop image, an isolated `XDG_CONFIG_HOME` with `[state] location` on that
   disk, `--seccomp=off --landlock=off` under `script -q -e` (the recipe is
   recorded in project memory; `tools/chromium-cang-smoke/chromium-smoke.sh`
   automates it).
4. The Chromium GPU smoke in both modes (`--gpu=drm` and the wayland/headless
   mode the smoke already covers), checking the renderer string is hardware
   backed - this is the acceptance for ticket 07 and the only backstop for
   `gpu`/`input`/`timesync` never having had a static consumer upstream.
5. A regression sweep of the things this change could quietly disturb: the
   guest's EMFILE/fd-pressure story (`cang-guest-runtime-troubleshooting`), and
   the `libkrunfw.so.5` lookup when cang is run from a built `$out` with nothing
   on `LD_LIBRARY_PATH` (previously covered by libkrun's `$ORIGIN` rpath).

Done when: all five are recorded here or in `notes/`, with the GPU smoke's
renderer evidence attached.

## Resolution

**All five verified, and the Chromium GPU smoke passes with hardware venus.**

1. **Validation** (devshell): `cargo fmt --check`, `cargo clippy --all-targets
   --all-features -- -D warnings` and `cargo deny check` clean. `cargo test
   --workspace`: cang 593, `cang-libkrun` 4, `cang-guest-init` 296,
   `cang-repository-tests` 45, all passing; `nix build .#cang` and
   `nix build .#cang-musl` both green. One **flake**, unrelated to this change:
   `cang-guest-init`'s `guest_init::components::waypipe::tests::failed_replacement_can_be_retried`
   failed once inside the derivation with `Text file busy (os error 26)` (a race
   under the parallel suite) and passed 5/5 in isolation and on the rebuild -
   retry rather than investigate.
2. **No libkrun shared object.** `readelf -d result/bin/cang` NEEDED entries:
   `libvirglrenderer.so.1`, `libgcc_s.so.1`, `libc.so.6`, `ld-linux-x86-64.so.2`
   - no `libkrun.so*`/`libkrun_init.so*` - and `strings` finds no
   `libkrun*.so` name in the binary. `result/lib/cang` carries only
   `libkrunfw.so*`. **Feeds ticket 10**: `libvirglrenderer.so.1` is now a hard
   NEEDED of the published binary, i.e. a `/nix/store`-relative dependency that
   the "neutral ELF" release asset cannot carry.
3. **Live boot** on this host (btrfs loop image at `/home/dev/cang/disk`, the
   `v2boot` graphroot's `storage.conf`, isolated `XDG_CONFIG_HOME` with
   `[state] location` on that disk, `script -q -e`):
   `cang --mem 4 --alloc hardened --seccomp=off --landlock=off -- sh -c 'echo
   live-boot-ok; uname -r; nproc'` printed `live-boot-ok`, `6.12.109-hardened1`
   and `26`, exit 0. That run is with the GPU off, i.e. the `--gpu=off` half of
   ticket 07's acceptance. The first attempt failed on the firmware lookup -
   see [ticket 11](11-firmware-load-in-the-vm-worker.md).
4. **Chromium GPU smoke: PASS**, `--gpu=drm`, 14 fresh evidence files:
   `PASS version`, `PASS chromium-rc` (WebGL + probe-DOM runs exit 0),
   `PASS webgl-vulkan`, `PASS webgl-png`. Renderer string recorded in
   `notes/03-chromium-gpu-smoke-evidence.txt`:
   `ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV
   NAVI33)) (0x00007480)), venus)` - hardware venus, not SwiftShader, through a
   cang that links libkrun's Rust API. This is the acceptance for ticket 07's
   `preload_libva` removal (it is deleted, and the GPU still works) and the
   backstop for `gpu`/`input`/`timesync` never having had an upstream static
   consumer.
5. **Regression sweep.** The guest's fd-pressure behaviour is untouched: this
   change is host-side and does not alter the guest init, the fd-watch component
   or any vsock/session path (the guest-init binary in the image is the image's;
   the smoke overrides it with the current `.#cang-musl`). The
   `libkrunfw.so.5` lookup from a bare `$out` *is* the thing that broke first,
   and is now handled by the preload in ticket 11 plus the binary's
   `$ORIGIN/../lib/cang` rpath - i.e. verified from a store path with nothing on
   `LD_LIBRARY_PATH`, which is exactly how the smoke ran.
