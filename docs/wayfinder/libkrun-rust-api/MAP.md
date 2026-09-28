---
label: wayfinder:map
title: Bind libkrun through its Rust API and retire libkrun.so
---

## Destination

cang binds libkrun through **libkrun's Rust API** - the `libkrun` crate (lib
name `krun`) and `krun-init-blob` with its `direct` feature, taken as **cargo
path dependencies on `deps/libkrun`** and compiled by cang's own rustc inside
cang's cargo build - and ships **no `libkrun.so.2` and no
`libkrun_init.so.0`**: the `dlopen`/`dlsym` layer, `CANG_LIBKRUN_LIBRARY`, the
load-order planning, the symbol-presence checks and the prebuilt-libkrun pin
are gone. The effort ends with `nix build .#cang` green, a live guest boot, and
the Chromium GPU smoke passing in both modes against the Rust-API build.

`libkrunfw.so.5` stays: libkrun's own `Payload::load_krunfw` opens it by soname
(`src/libkrun/src/api/payload.rs`), so this removes the *libkrun* shared objects,
not every shared object. What replaces libkrun's `$ORIGIN`-based firmware lookup
is cang's own rpath/`LD_LIBRARY_PATH`, which `cang-prebuilt` already sets.

**Status (2026-09-28): charted.** Nothing implemented yet in the repo. Two
tickets are already closed by work done while charting (01, 02); 03 and 04 are
the frontier.

## Notes

- Domain: `crates/cang/src/runtime/vm/libkrun/{api,dynamic,launcher,tests}.rs`
  (today's C-ABI binding), `nix/pkgs/{libkrun,cang-rust,cang-prebuilt}.nix`,
  `nix/dev/flake.nix` (the local-source libkrun override), `flake.nix` (the
  `libkrun-loadable` check and the package wiring), `nix/image/{layers,checks}.nix`,
  `crates/cang-repository-tests/tests/repository.rs` (asserts the `.so` symlink
  loops), `scripts/update-libkrun.sh`, `nix/pins.nix`, the fork `deps/libkrun`
  (branch `cang`, upstream `main` = the 2.0.0 / ABI-2 line), and the guest-side
  `tools/chromium-cang-smoke`.
- Skills to consult: `cang-libkrun-rust-api-static-link-feasibility` (the size
  and shape of this move), `cang-libkrun-v1-to-v2-api-port-mapping` (how the
  launcher got to ABI 2), `cang-local-validation-gates`, `cang-guest-gpu-chromium-diagnosis`
  (ticket 09), `cang-repo-rename-and-removal` (the docs/skill refresh pattern),
  `codebase-design` (the `cang-libkrun` seam, ticket 05),
  `domain-modeling` (the ADR this effort should end with).
- Tracker: **local markdown**, this directory (`MAP.md`, `tickets/`, `notes/`).
  `gh` is unauthenticated here, so pushes to `zeroqn/libkrun` / `zeroqn/cang` are
  bob's. Evidence files go in `notes/`; the two-rule from the previous map holds:
  claim a ticket by writing `claimed_by` before working it.
- **Execution is in scope** (bob, 2026-09-28): this map ends with a working
  cang, not with a plan.
- Destination decisions taken while charting (bob, 2026-09-28):
  - **Rust API, not a static C ABI.** If cang compiles libkrun's source with its
    own rustc anyway (which same-toolchain correctness requires), the C ABI buys
    nothing: it *is* a projection of the Rust API (`make gen-libkrun-bindings`),
    and the static-archive route costs fork work (crate-types plus a link-time
    client for the init blob, which today `dlsym`s its two libkrun calls).
    Evidence: `notes/01-raw-staticlib-abi-forensics.txt`,
    `notes/01-rust-api-surface.md`.
  - **Path dependency on `deps/libkrun`, not a git rev** (bob): a git rev still
    pulls the whole source for compilation, and libkrun is `2.0.0-dev`, so the
    submodule stays the place to hack the fork.
  - **No v2.0.0 gate.** "Wait for libkrun 2.0.0" was bob's earlier lean; with the
    C-ABI-is-generated evidence it protects nothing here. The fork is tracking
    upstream `main`, so a pin bump may need compile fixes - accepted. The gate's
    one surviving form is "keep today's `dlopen` until 2.0.0", which is *not*
    what this map does.
  - **Keep `LibkrunApi` as the seam** (with the recording fake in `tests.rs`) so
    `launcher.rs`'s fixtures survive; whether it stays afterwards is ticket 05's
    question, not a destination decision.
- Starting facts (2026-09-28):
  - The Rust API builds as a path dep with `blk,net,gpu,input,timesync` in ~41s
    and zero warnings, with `bindgenHook`'s env plus `virglrenderer`/`mesa-libgbm`
    `.pc` files: `notes/02-rust-api-build-probe.md`.
  - `nix/dev/flake.nix` already contains half the Nix plumbing: `self.submodules
    = true`, `libkrunCargoDeps = fetchCargoVendor { src = deps/libkrun; ... }`,
    a `pkgs.pkgsStatic`-built `krunInitBinary` fed to libkrun's build through
    `KRUN_INIT_BINARY_PATH`, and a `pkgs.libkrun.override` with `FFI=1` and the
    five features. That work was the previous map's ticket 12; tickets 03/04
    reuse it rather than invent it.
  - The header comment there records the vendoring constraint that decides
    ticket 03: *upstream main's lock vendors `ffier` twice at one name-version,
    which `importCargoLock` cannot express, so the fork's source is vendored with
    `fetchCargoVendor`*.
  - Binding sites to retire live in `dynamic.rs` (1200 lines: `dlopen`, the
    symbol table, `planned_*_load_order`, `preload_libva`, the error vtable) and
    are asserted by `flake.nix`'s `libkrun-loadable` check, the image layers'
    libkrun symlinks, `nix/pins.nix`'s `libkrunRelease` pin, and README/docs
    (`README.md:29-34`, `docs/internals.md:7-9`).

## Decisions so far

- [Research: libkrun's Rust API surface vs the C ABI](tickets/01-rust-api-versus-c-abi-surface.md):
  the Rust API covers every call cang makes (`VirglRendererFlags` is narrower -
  DRM/USE_VIDEO need `from_bits_retain` - and `DisplayBackend::new` still takes a
  raw vtable, so cang's headless backend survives); `krun-init-blob/direct` is
  the init-injection path; upstream's own tests compile their suite both
  static-linked and cdylib-loaded.
- [Prototype: build libkrun's Rust API as a path dependency](tickets/02-rust-api-build-probe.md):
  it builds (41s, no warnings) with `blk,net,gpu,input,timesync` given the
  nixpkgs bindgen hook env and `virglrenderer` + `mesa-libgbm` `.pc` files.

## Not yet specified

- **Whether `nix/dev`'s local-source libkrun override survives.** Once cang
  compiles libkrun itself, the dev sub-flake and the main derivation both build
  the same source with the same rustc; the override may collapse into a plain
  source pointer. Sharpens in ticket 03.
- **The cache story for the enlarged graph.** Every libkrun commit changes the
  vendor hash; how that interacts with the CI sccache cache key
  (`publish_release.yml`'s `hashFiles('Cargo.lock', 'flake.lock', 'nix/pins.nix')`)
  and with `cang-ci-sccache` is unexplored. Sharpens after 03.
- **Whether `LibkrunApi` should survive the port.** The trait exists to make the
  C-ABI seam testable; a Rust-API backend may make a deeper `cang-libkrun` module
  the better seam (`codebase-design`). Sharpens in 05/06.
- **Supply-chain policy for ~150 new crates** (licenses, advisories, and the
  `ffier` git dependency that `krun-init-blob` carries unnecessarily). Sharpens
  in 08.
- **What the release asset becomes.** `publish_release.yml` publishes a neutral
  ELF with `rpath` stripped and no `/nix/store` references; with libkrun static
  the ELF gains `DT_NEEDED` entries (libvirglrenderer and friends), which may
  change what "neutral" can mean. Sharpens in 10.
- **The next fork base.** If upstream cuts 2.0.0 while this runs, the Rust API
  may move; the pin bump is then a compile fix. That base decision belongs to the
  fork map, not here.

## Out of scope

- **A static C ABI archive (`staticlib`) instead of the Rust API.** Bob's first
  lean (2026-09-28), superseded the same day by the C-ABI-is-a-projection
  argument plus the cross-toolchain forensics: a Rust staticlib defines
  `rust_eh_personality` as a strong unmangled symbol, so a foreign-toolchain
  archive collides or needs `--allow-multiple-definition` and ships two stds,
  while a same-toolchain archive gains nothing over the Rust API. Evidence:
  `notes/01-raw-staticlib-abi-forensics.txt`.
- **Keeping a `dlopen` fallback / `CANG_LIBKRUN_LIBRARY`.** Not a decision to
  revisit: a Rust-API binding cannot be `dlopen`ed. The override's loss is a
  consequence of the destination, recorded here so nobody re-litigates it.
- **Replacing `libkrunfw.so.5` too.** libkrun's payload code opens it by soname;
  removing that is a libkrun change, not a cang one.
- **PR 822** (virtio-gpu zero-copy SHM): inert on the pinned kernel, ruled out by
  the previous map and not reopened here.
- **Wiring libkrun's `timesync` / 865's parallel reads into cang's CLI**: the
  fork carries them; exposing them is a follow-on effort.
- macOS/Windows variants of libkrun; the fork stays Linux-only in cang.
- The fork's own rebase/pull mechanics (`libkrun-main-rebase`); this map consumes
  whatever base that effort pins.
