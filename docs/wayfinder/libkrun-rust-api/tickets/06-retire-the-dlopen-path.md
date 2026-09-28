---
label: wayfinder:task
title: Retire the dlopen path, the .so files and their checks
status: open
blocked_by: ["05-create-cang-libkrun-crate"]
claimed_by:
---

## Question

Delete everything that existed to load `libkrun.so.2` / `libkrun_init.so.0` at
runtime, once the Rust-API binding is in place. Known sites:

- `crates/cang/src/runtime/vm/libkrun/dynamic.rs`: `dlopen`/`dlsym`, the hand
  written symbol table and `KrunStr`/`KrunBytes`/error-vtable plumbing,
  `CANG_LIBKRUN_LIBRARY`, `planned_library_load_order` /
  `planned_libkrun_init_load_order_for_exe`, and the `preload_libva` RTLD_NOW
  workaround (last one belongs to ticket 07's GPU verification, which decides
  whether it becomes unnecessary rather than deleted blind).
- `nix/pkgs/cang-rust.nix` and `nix/pkgs/cang-prebuilt.nix`: the
  `$out/lib/cang/libkrun*.so*` symlink loops; `nix/pkgs/libkrun.nix`'s
  `$ORIGIN`/virglrenderer rpath patching (only meaningful for a dlopen'd
  library).
- `flake.nix`'s `libkrun-loadable` check; `nix/image/{layers,checks}.nix`'s
  libkrun install; `crates/cang-repository-tests/tests/repository.rs`'s
  assertions on the `.so` symlink loops; `scripts/update-libkrun.sh`.
- Docs: `README.md:29-34` (`CANG_LIBKRUN_LIBRARY`, the `lib/cang` lookup order)
  and `docs/internals.md:7-9`. Replace with how the firmware is still found
  (`libkrunfw.so.5` is dlopened *by libkrun* and resolves through cang's
  rpath/`LD_LIBRARY_PATH`).
- `crates/cang/src/runtime/vm/libkrun/mod.rs`'s test-only re-exports of
  load-order helpers.

Careful: keep the `libkrunfw` wiring (`nix/pkgs/libkrunfw.nix`, the `$out/lib/cang`
symlinks for `libkrunfw.so*`, the image's firmware) - only libkrun's own shared
objects go.

Done when: `rg -n 'dlopen|dlsym|CANG_LIBKRUN_LIBRARY|libkrun\.so|libkrun_init\.so'`
over `crates/`, `nix/`, `docs/` and `README.md` returns only what legitimately
remains (the firmware), the repository tests are updated, and the package output
contains no `libkrun.so*`/`libkrun_init.so*`.
