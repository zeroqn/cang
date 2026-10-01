# cang links libkrun's Rust API instead of loading a shared library

Status: accepted

Cang binds libkrun through libkrun's **Rust API**: the `cang-libkrun` crate
depends on `deps/libkrun` (the `zeroqn/libkrun` fork submodule) by path, enables
the `blk`, `net`, `gpu`, `input` and `timesync` features, and uses
`krun-init-blob`'s `direct` feature for the guest init. libkrun is compiled by
cang's own rustc inside cang's build. No `libkrun.so` or `libkrun_init.so` is
loaded at run time, there is no `CANG_LIBKRUN_LIBRARY` override, and the pinned
prebuilt C-ABI library, its pin and its updater are gone.

## Context

The previous binding `dlopen`ed a pinned prebuilt `libkrun.so.2` plus
`libkrun_init.so.0` and called a hand-maintained table of `krun_*` symbols: ~1200
lines of `dlsym` in `crates/cang/src/runtime/vm/libkrun/dynamic.rs`, a load-order
planner, symbol-presence checks, and a `libkrun-loadable` Nix check. It existed
because libkrun 2.0 (ABI 2) moved its C surface behind a cargo feature and
because binding a shared library let cang point at a locally built libkrun.

Two facts changed the calculus. First, **the C ABI is generated *from* the Rust
API** (`make gen-libkrun-bindings`; the exported names are
`krun_<object>_<method>`), so binding the frozen C surface buys no stability the
Rust API does not have. Second, Rust has no stable ABI: any same-toolchain
requirement means cang compiles libkrun's source anyway, and once it does, a
*static* archive of the C ABI adds a cross-toolchain hazard (a Rust staticlib
defines `rust_eh_personality` as a strong unmangled symbol) while still needing
fork work for the init blob, whose C-ABI client reaches libkrun by `dlsym`.
libkrun's own test suite compiles the same test bodies against both a
statically linked `krun` and the cdylibs, so this is an upstream-supported
consumer shape.

## Decision

- Bind libkrun's Rust API from a dedicated `crates/cang-libkrun`, keeping
  cang's existing `LibkrunApi` trait (and its recording fake) as the seam: the
  launcher's launch policy, ordering and tests are unchanged.
- Take libkrun as a **path dependency on the checkout at `deps/libkrun`**, not a
  git revision: the source is compiled either way, and a fork hack belongs in a
  checkout. The libkrun version is the revision of that checkout.
  (Amended 2026-09: the revision reaches a Nix build as the `libkrun-src` flake
  input, which `nix/pkgs/workspace-src.nix` grafts into `deps/libkrun`, because a
  flake's own source cannot carry submodule contents and
  `inputs.self.submodules = true` only makes a downstream `github:` lock ref
  invalid. The crate stays a path dependency on `deps/libkrun`.)
- Open the firmware (`libkrunfw.so.5`) by **absolute path before libkrun looks
  for it**: the VM worker is exec'd through `unshare --keep-id` with a changed
  uid, which puts glibc in secure-execution mode, where `$ORIGIN` in `DT_RUNPATH`
  and `LD_LIBRARY_PATH` are ignored. The firmware stays a shared object; it is
  opened by cang, not by the loader.
- Retire the prebuilt C-ABI libkrun pipeline: `nix/pkgs/libkrun.nix`,
  `scripts/update-libkrun.sh`, the `libkrunRelease` pin, the `libkrun-loadable`
  check and the image's `libkrun` layer entry. The image keeps the *firmware*
  (`libkrunfw`) for a cang binary that runs inside the guest.

## Consequences

Source builds of cang need libkrun's build inputs: the bindgen hook
(`krun-display`/`krun-input`), `pkg-config` with `virglrenderer` and `gbm`
(`rutabaga_gfx`), `rustfmt` (ffier's generator) and a musl guest init blob. The
workspace `Cargo.lock` grows libkrun's dependency graph, so `cargo deny` covers
it and Nix vendors it (`fetchCargoVendor`, which
`importCargoLock` cannot express for the fork's lock). A libkrun bump moves the
`deps/libkrun` submodule pointer and the `libkrun-src` flake input together and
refreshes two vendor hashes plus `Cargo.lock` - see
[the maintenance procedure](../maintenance.md#updating-the-libkrun-fork).

The fork's `ffier` dependency is only for the C bindings its release pipeline
generates: `krun-init-blob` declares it optionally and the `ffi` feature enables
it, so cang's graph contains no `ffier` at all (2026-10, fork `0847b562`; before
that it was compiled here as an unused dependency). The GPU stack still needs a fork
outside crates.io - libkrun pins `rutabaga_gfx` to `zeroqn/rutabaga_gfx` because
cang's `VIRGL_RENDERER_USE_VIDEO` request needs a flag upstream does not expose -
but cang compiles it the same way it compiles libkrun: the `deps/rutabaga_gfx`
submodule, selected by a workspace `[patch]` and grafted from the
`rutabaga-gfx-src` input in a Nix build, so cang's lock records it as a path (see
[the maintenance procedure](../maintenance.md#updating-the-rutabaga_gfx-fork)).
The vendor helper stays.

The published release asset stays a bare neutral ELF (ADR 0005), but it now
carries `libvirglrenderer.so.1` as a `DT_NEEDED`, so a consumer that is not Nix
has to provide that library.

`deps` must stay excluded from the workspace (`exclude = ["deps"]`), or cargo
enrols libkrun's own crates as workspace members and runs their test targets.

Upstream's static-linking test configuration only enables `blk` and `net`, so
cang is the first static consumer of `gpu`, `input` and `timesync`: the Chromium
GPU smoke, not the test suite, is what proves those still work.

## Considered options

- **Static C ABI (`staticlib`).** Rejected: it keeps the generated C surface and
  the init blob's `dlsym` client (so fork work), and a foreign-toolchain archive
  collides on `rust_eh_personality`. It only wins if libkrun is *not* built by
  cang's toolchain, which Rust's ABI rules out.
- **A git-revision dependency on the fork.** Rejected: it still downloads and
  compiles the whole source, and it removes the local checkout that fork work
  needs while libkrun is pre-2.0. (Amended 2026-09: the `libkrun-src` input is a
  revision pin for Nix builds only - the crate still resolves libkrun through
  `deps/libkrun`, and `--override-input` points a build at fork work there.)
- **Keep `dlopen` until libkrun 2.0.0 is released.** Rejected: the C ABI is a
  projection of the Rust API, so the wait protects nothing, and it would keep the
  load-order/symbol machinery and the `.so` packaging alive.
- **Keep a runtime `dlopen` fallback.** Impossible: a Rust-API binding cannot be
  `dlopen`ed.
