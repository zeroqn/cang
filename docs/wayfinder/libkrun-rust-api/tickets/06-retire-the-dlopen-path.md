---
label: wayfinder:task
title: Retire the dlopen path, the .so files and their checks
status: closed
blocked_by: ["05-create-cang-libkrun-crate"]
claimed_by: pi session (2026-09-28)
---

## Question

Delete everything that existed to load `libkrun.so.2` / `libkrun_init.so.0` at
runtime, once the Rust-API binding is in place.

## Resolution

**The binding's dlopen path is gone; the prebuilt `.so` artefacts that still have
C-ABI consumers are ticket 10's, not this ticket's.**

- Deleted: `crates/cang/src/runtime/vm/libkrun/{api,dynamic}.rs` - the 1200-line
  dlsym layer (symbol table, `KrunStr`/`KrunBytes`, the error vtable,
  `CANG_LIBKRUN_LIBRARY`, `preload_libva`, `planned_*_load_order`) - and the four
  tests that asserted the load order and symbol presence. Their remaining
  subject matter (the `CANG_LIBKRUN_COMPAT_NET_FEATURES` bit contract) kept its
  test.
- Deleted from `nix/pkgs/cang-rust.nix`: the `libkrun*.so*` symlink loop. The
  firmware symlink loop stays (libkrun opens `libkrunfw.so.5` by soname) and the
  binary gained an `$ORIGIN/../lib/cang` rpath in its place.
- `nix/pkgs/libkrun.nix`'s `$ORIGIN`/virglrenderer rpath patching: kept, because
  the prebuilt `.so` it patches is still published and still consumed by
  C-ABI users (ticket 10).
- Docs: `README.md` (the libkrun-at-runtime bullet + the source-build section),
  `docs/internals.md` (the lookup-order paragraph), `docs/build.md` (build
  outputs, and the submodule requirement), `docs/maintenance.md` (the FFI=1 pin's
  remaining consumers, plus a new `deps/libkrun` bump procedure).
- Repository tests: the cang-packager invariant test dropped the `.so` symlink
  requirement, gained the Rust-API build inputs (`bindgenHook`, `pkg-config`,
  `rustfmt`, `virglrenderer`, `libgbm`, `KRUN_INIT_BINARY_PATH`,
  `fetchCargoVendor`) and now *forbids* the two `.so` symlink fragments and
  `cargoLock = {`. It caught a stray `libkrun.so` mention in a comment, which is
  why the assertion matches the symlink fragments rather than the bare name.
- **Deferred to ticket 10, deliberately**: `flake.nix`'s `libkrun-loadable`
  check, the `libkrun` package export, and the image's `toolingImageLayer`
  libkrun entry. All three exist for the *prebuilt* C-ABI `.so` that the pinned
  release asset still ships; the image's nested-cang path may still depend on it.
  Removing them is a scope question about consumers, which is ticket 10's.

Verified: `readelf -d result/bin/cang` shows `NEEDED libvirglrenderer.so.1`,
`libgcc_s`, `libc`, `ld-linux` and **no** libkrun object; `strings` finds no
`libkrun*.so` name in the binary; `result/lib/cang` holds only the firmware;
`./result/bin/cang --help` runs.
