---
label: wayfinder:task
title: Verify - build, no libkrun .so, live boot, Chromium GPU smoke
status: open
blocked_by: ["06-retire-the-dlopen-path", "07-port-gpu-and-launcher-details"]
claimed_by:
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
