# 04 — Fork-commit triage onto upstream main

Wayfinder ticket `04-rebase-cang-onto-main`, map
`docs/wayfinder/libkrun-main-rebase`. Session 2026-09-27.

## Rebase parameters (frozen)

| what | value |
|---|---|
| upstream base (frozen `main`) | `a980e7795b86c8151e867b32a43af4cb984364fa` (`FULL_VERSION=2.0.0`, `ABI_VERSION=2`) |
| fork base before (old) | `fb988873` = `upstream/stable-1.19.x` tip = v1.19.5 |
| fork tip before (old) | `28e79624` (branch `cang`; submodule pin = `237ceac0` = tag `v1.19.5-cang.1`) |
| safety bookmark | local branch `cang-pre-main-rebase` -> `28e79624` |
| work branch | `cang-main-rebase`, created at the frozen base |
| fork commits to triage | 16 (`fb988873..cang`) |

## Triage

Verdicts: **replayed** (cherry-picked onto `main`, `-x` trailer added),
**dropped/superseded** (main or the external crates already do it),
**re-derive** (the fork's capability must be re-expressed on the ABI-2 API -
ticket 10's work; not a mechanical replay).

| # | fork commit | subject | verdict | why |
|---|---|---|---|---|
| 03 | `28aaf0d6` | ci: make loftd libkrun prebuilts reusable | **replayed** (`d4f04df7`) | conflict only in the apt list; resolved as upstream's `git` line **plus** the fork's `make gcc g++ clang lld pkg-config`, **minus** `libvirglrenderer-dev` (main deliberately builds virglrenderer from source in the next step) |
| 04 | `b7d37564` | ci: let sccache see real C compilers | **replayed** (`c3fd19f4`) | clean |
| 12 | `b7680460` | ci: publish cang-prefixed libkrun prebuilts | **replayed** (`82f3c257`) | clean; this is the `publish-loftd-release.yml` -> `publish-cang-release.yml` rename |
| 14 | `975363ba` | ci: publish permanent versioned releases and attest prebuilts | **replayed** (`154724a7`) | clean |
| 15 | `237ceac0` | ci: trigger permanent releases from version tags | **replayed** (`a164d045`) | clean |
| 16 | `28e79624` | ci: scope the release concurrency group per ref | **replayed** (`06d45fbf`) | clean |
| 13 | `8390691d` | docs: rename LOFTD.md to CANG.md | **replayed as an addition** (`abf8ca80`) | main has neither file, so the rename is a plain add; the doc's content was then updated for the ABI-2 base |
| 08 | `394802e3` | rustfmt: reorder imports | **dropped/superseded** | main's formatting already covers it |
| 06 | `1c68255f` | vaapi: get_drm_fd renderer callback + video diagnostics | **dropped/superseded** | crates.io `rutabaga_gfx` 0.1.85 ships it: `src/virgl_renderer.rs:287` `extern "C" fn get_drm_fd`, registered at `:397` (`get_drm_fd: Some(get_drm_fd)`) |
| 07 | `f0e2028d` | vaapi: open DRM render node O_RDWR for libva | **dropped/superseded** | same crate: the render node is opened `.read(true).write(true)` with `O_CLOEXEC|O_NONBLOCK|O_NOCTTY` (`src/virgl_renderer.rs:305-318`), gated by `is_valid_gpu_path` (`:82-93`) |
| 01 | `ac615be3` | feat(loftd): add opt-in libkrun profiling | **re-derive** (ticket 10) | touches `src/libkrun/src/lib.rs` + `src/vmm/src/{builder,lib,profile}.rs`; the v1 API is gone and `src/vmm` moved under `src/libkrun/src/vmm` |
| 02 | `329f14db` | feat(loftd): allow profile-only kernel diagnostics | **re-derive** (ticket 10) | v1 C API (`include/libkrun.h` + `lib.rs`) |
| 05 | `dacdbac4` | gpu: add krun_set_gpu_options3 render-server fd plumbing | **re-derive** (ticket 10) | main deleted the v1 entry point and moved GPU configuration out of `VmResources` (no `gpu_*` fields remain there; the builder now takes `gpu_shm_size` from device requirements, `builder.rs:723`). The *crate* half is already available: `RutabagaBuilder::set_server_descriptor(Option<OwnedDescriptor>)` (`rutabaga_core.rs:1413`), `set_use_render_server` (`:1380`), and `build()` now takes **no** arguments (`:1435`) |
| 09 | `7f77a0ac` | virtio/gpu: fence-retirement fix | **re-derive** (ticket 10) | needs `virtio_gpu.poll_descriptor()`, which the fork's *in-tree* rutabaga provided; the crate now provides `poll_descriptor()` (`src/virgl_renderer.rs:607`, wrapping `virgl_renderer_get_poll_fd`), so the re-derivation is small |
| 10 | `2a583f8a` | gpu: harden render-server fd handling, fix poll/fence/lock | **re-derive** (ticket 10) | spans `src/libkrun/src/lib.rs` (v1 API), the builder and the device |
| 11 | `7c20aa6f` | gpu: fix blob-map overflow, gate DRM render-node open, reclaim cookie, idle poll | **re-derive** (ticket 10) | device-side fixes stand, but the commit also rewrites `src/rutabaga_gfx/*`, which no longer exists in the tree |

## Result

`cang-main-rebase` = `a980e779` + the seven replayed commits. Nothing was pushed
and the old tip is preserved as `cang-pre-main-rebase`.

The replayed commits carry no fork Rust code: **every line of the fork's C-side
delta is currently absent from the rebased branch** and is ticket 10's work. That
is expected (main replaced the API those commits extend), and it means the
rebased branch is upstream `main` plus the fork's CI and docs until ticket 10
lands.

## Finding that changes ticket 10's shape

The fork's GPU delta is *not* blocked on a vendored `rutabaga_gfx`. The
primitives it needs are in crates.io 0.1.85:

- `RutabagaBuilder::set_server_descriptor(Option<OwnedDescriptor>)` and
  `set_use_render_server(bool)` - the render-server fd path (fork commit 05);
- `poll_descriptor()` -> `virgl_renderer_get_poll_fd()` - the fence-retirement
  fix (fork commit 09);
- `get_drm_fd` + the `O_RDWR` render-node open - the vaapi work (fork commits
  06/07), already upstream, so those two commits are dropped rather than ported.

What is genuinely fork-only on the ABI-2 tree is therefore: the *v2 API entry
points* (profile path, render-server fd), the device-side plumbing that reaches
`set_server_descriptor`, and the device fixes in commits 09/10/11.

## Adjacent findings for tickets 06/07 (from the replay + a build attempt)

- **`nix/dev/flake.nix` could not vendor main's dependency set with
  `importCargoLock`.** main's `Cargo.lock` pulls `ffier` from git twice, at tags
  `0.2.0rc1` (`9616083b...`) and `v0.2.0-rc2` (`4609b7a4...`), both name-version
  `ffier-0.2.0`. nixpkgs' `import-cargo-lock.nix` builds `namesGitShas` as a map
  keyed by `${name}-${version}` (verified in the pinned nixpkgs source), so the
  second revision has no expressible `outputHashes` key and evaluation fails with
  *"No hash was found while vendoring the git dependency ffier-0.2.0"* - for
  refs `9616083b` and `4609b7a4` the prefetch hashes are
  `sha256-bicvHReD9zX9N7iLY9JQXZKFtBU4X7IHKqXCzOKdFvI=` and
  `sha256-meSiGiejRrPGrMvVTeY5RKbB6iAiWg3OSiannST5qy4=`. Upstream tip still has
  both revisions, so pinning a newer `main` does not dodge it.
  **Fix applied:** the fork's source is vendored with
  `rustPlatform.fetchCargoVendor` (the same helper nixpkgs' own `libkrun`
  package uses), which runs `cargo vendor` and therefore tolerates the duplicate.
  The prebuilt/release path (`nix/pkgs/libkrun.nix`, which fetches a release
  tarball) is unaffected by this.
- **The replayed `publish-cang-release.yml` needs a feature-list and packaging
  update for main** (ticket 06): its build step runs
  `make BLK=1 NET=1 GPU=1 SND=1 INPUT=1`, but main removed the snd feature
  (`SND=1` is a silent no-op) and added `vhost-user`, `timesync`, `ffi`. main's
  Makefile builds the init blob in the same run
  (`cargo build --release -p krun-init-blob --features ffi`) and installs
  `libkrun_init.so.0.1.0` + symlinks, `libkrun_init.pc` and
  `include/libkrun_init.h`; the workflow's `lib64` + `include` tar therefore
  carries them, but its `test -f` asserts should be extended so a missing init
  blob fails CI instead of the guest.
- **The nix prebuilt packaging must ship `libkrun_init.so*`** (ticket 07): the
  glob-based fixup in `nix/pkgs/cang-{prebuilt,rust}.nix` / `nix/pkgs/libkrun.nix`
  matches `libkrun.so*` only.
- `nix/dev/flake.nix` also still passes `withSound = true` and pins
  `version = "1.19.5-cang-profile"`; both are stale for a 2.0.0 base (ticket 07).

## The two upstream PRs, replayed (ticket 11's matrix)

Replayed on `cang-main-rebase` in this order, all clean (no conflict, `-x`
trailers added), per `notes/11-pr865-840-matrix.md`:

| PR | commits (in order) | subject |
|---|---|---|
| 865 | `802c9e1e` -> `6b24d0d2` -> `08a8773a` | RwLock for disk image access; dispatch reads to a thread pool (fix #824); make parallel reads opt-in |
| 840 | `3f3062e3` -> `32eb92b5` -> `6d800f98` -> `2f37b0a3` | enable timesync for Linux; allow an explicit time-sync request from a guest; reduce `DELTA_SYNC` to 10 ms; build the init blob with timesync when `TIMESYNC=1` |

- The order inside 865 is load-bearing: `6b24d0d2` without `08a8773a` leaves
  parallel reads permanently **on** and changes cang's block behaviour.
- 865 stays inert for cang: its opt-in surface is a new
  `krun_block_device_set_parallel_reads(handle, bool)` entry point (default
  false, no cargo/env knob) that nothing in cang calls; only the `RwLock`
  refactor is live.
- 840's host half is unconditional; its guest half needs `TIMESYNC=1`, which the
  fork's prebuilt workflow did not pass. Added in the adaptation commit
  `d578e4e2`, which also asserts `libkrun_init.{so*,pc,h}` in the packaging step
  (main moved the guest init out of `libkrun.so`).
- Under ABI 2 the guest init is the separate `libkrun_init.so`: if cang keeps
  libkrun's default init the `TIMESYNC=1` blob is what runs, otherwise cang's own
  init must implement the time-sync request (`cang-guest-init` does not).
