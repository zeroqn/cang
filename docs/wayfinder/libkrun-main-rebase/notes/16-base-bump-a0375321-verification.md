# Base bump: libkrun fork rebased onto upstream `a0375321`

**Status (2026-11-02): next base verified locally, not yet pushed.**
`deps/libkrun` (branch `cang`) now sits on upstream `main`
`a0375321bb1e6e51312e022af53e9e7668d3a07e` (2026-10-29) instead of
`a1d8616765da03980f3be1bb8714bfdd3842f33c` (2026-10-16) - 26 new upstream
commits under the fork's 35. Local tip is `2e836ce88ca04ca029b5b801e012eb4b25b3032e`
(32 commits on the new base). Nothing is pushed: `origin/cang` is still
`e8a771592ec78e5041f8d3abe44a37572a73a977`, so `flake.lock`'s `libkrun-src`
input still pins the old rev, and the Nix path was verified with
`--override-input libkrun-src "git+file://$PWD/deps/libkrun"`.

## What the new base brought, and what it replaced

Upstream merged PR 865, so the fork's three blob-device cherry-picks
(`0a1092c1`, `34e22453`, `936c79e8`) are the *same patches* (identical
`git patch-id`) as upstream `130572f5`, `54d4cd5c`, `3181982d` and were dropped
in the rebase. What survives is the fork's one-line default flip
(`parallel_reads = true` in `BlockDevice::new`), which is all CANG.md needs to
keep describing; the claim that PR 865 is "carried and still unmerged" is gone.

Upstream also took the `zstd 0.14` bump, so `d1c58a99` became a no-op and was
*not* dropped as an empty commit: its hunk resolved to upstream's `0.14.3`
string and the rest of the commit was the lock refresh, which upstream's own
`72b65678` had already done.

## Non-obvious conflict resolutions

- **`fs` enums moved.** Upstream moved `CachePolicy` / `PermissionSemantics`
  (and `submount_attr_flags`) out of `linux/passthrough.rs` into `fs/mod.rs`.
  The fork's commits still added them to `passthrough.rs`, so those hunks are
  dropped and only the fork's *new* items stay there: `submount_attr_flags`
  (upstream inlines the check; our readdirplus path calls the helper from two
  places) and `negative_entry`.
- **`Fs::new` config.** Upstream builds `passthrough::Config { root_dir,
  ..Default::default() }` and then calls `set_permission_semantics(semantics)`,
  which sets `semantics`/`attr_timeout`/`xattr` itself. The fork's explicit
  `attr_timeout`/`entry_timeout` locals and struct fields are therefore
  redundant and were dropped; the resulting timeouts are the same (5 s, 0 s
  under `LinuxSimplified`).
- **`DeviceRequirements`** gained upstream's `replaces_legacy_rtc` next to the
  fork's `zero_copy_shm`; both are kept, and `build_microvm` now attaches the
  RISC-V PL031 RTC before the (profiled) `device_manager.attach_all`.
- **`rutabaga_gfx` / `imago`.** The fork pins `zeroqn/rutabaga_gfx`
  (`d8479a1e`, the `VIRGL_RENDERER_USE_VIDEO` bit); upstream's `imago 0.2.6`
  bump is taken over the fork's `0.2.4` (nothing in the fork pins imago).
- **Generated surface.** `include/libkrun.h` and
  `bindings/libkrun-via-cdylib-weak/ffier-krun.json` were taken from upstream
  during the rebase and regenerated with
  `make BLK=1 NET=1 GPU=1 INPUT=1 TIMESYNC=1 FFI=1 gen-libkrun-bindings`, which
  re-added exactly the five fork fs setters next to upstream's new
  permission-semantics and cache-policy setters (pure insertions, no deletions).

## Verification (all live, on `/nix/store/zgkiyf272ch05wpm5j3vma3f22pxbbrg-cang-0.16.2`)

The package under test is built from the rebased tree: the build log resolves
`deps/libkrun?ref=refs/heads/cang&rev=2e836ce8...`. The control is the released
`.#cang-prebuilt` (`v0.16.2`, old base, same cang code) - so every A/B below
isolates libkrun.

| check | result |
|---|---|
| `make BLK=1 NET=1 GPU=1 INPUT=1 TIMESYNC=1 FFI=1` (libkrun) | green, no warnings |
| `cargo fmt --all --check` | clean |
| `nix build .#cang .#cang-musl .#container` (rebased input) | green; cang's own test/doc-test hooks pass |
| virtio-fs perf, `tools/rust-compile-bench` synthetic (2401 crates), 5 alternating pairs | pre `61.769/55.037/69.991/70.966/72.461 s` (mean 66.04 s, median 69.99 s) vs new `55.405/56.636/70.370/66.089/72.944 s` (mean 64.29 s, median 66.09 s): mean paired delta **-2.6 %**, per-pair `-10.3 %, +2.9 %, +0.5 %, -6.9 %, +0.7 %`, `fs_worker` 22-28 % of a core in both - **no regression**, the spread is host noise (absolute level drifts 55-73 s between batches) |
| two runs discarded | the `23:07` window (85.4 s rebased, 72.2 s control) ran under a 1 Hz log-copying watcher and is excluded, not averaged in |
| fs opcode histogram at 300 001 requests (both arms) | `FLUSH 2`, `lookup ... 0 negative`, `LOOKUP 39.09 %` (vs `FLUSH 2`, `0 negative`, `39.22 %` on the control) - the no-flush and negative-entry levers survived |
| `tools/chromium-cang-smoke --waypipe` | PASS - venus `ANGLE (AMD, Vulkan 1.4.343 (Virtio-GPU Venus (RADV NAVI33)), venus)`, host frame 100 928 pattern px, renderer overlay 4 613 px, control run 0 px |
| `tools/chromium-cang-smoke --waypipe --software-renderer` | PASS - llvmpipe presented, frame 117 344 px, renderer overlay 3 995 px |
| `tools/virglrenderer-cang-smoke --skip-chromium` | PASS - H.264 VLD + EncSlice advertised, 60 frames, decode `y:46.633758 u:44.227307 v:44.408519`, encode/roundtrip `y:46.537574 u:43.627419 v:42.909682` - identical to the 7.2.10 baseline, so the PR-889-class decode blanking is absent |
| boot + `--fs` CLI surface on the rebased binary | three launches (default, `--fs=no-flush:off --fs=negative-entry:off`, `--fs=negative-entry:1000`) all rc 0 on guest `7.2.10-hardened1`; share reads correct, missing-name `stat` still 1, guest writes byte-exact on the host |

Guest kernel in every run is the unchanged libkrunfw pin
(`7.2.10-hardened1`), so these results isolate the VMM, not the firmware.

## Host data cleaned in the same pass

Two stale smoke roots from the previous campaign (`/home/dev/disp/disk/vsmoke-79228a72`,
`vsmoke-full-79228a72`, fork tip `79228a72`) and 18 stale cang workspaces under
`/home/dev/disp/cang` were removed (their PASS/PSNR lines were copied to
`/home/dev/cang-bench/prev-vsmoke-79228a72-summary.txt` first). The new smokes'
graphroots were dropped after their evidence was copied to
`/home/dev/disp/disk/evidence-rebase/`; the container image cache
(`/home/dev/disp/cang/microvm`) is untouched. Disk-headroom remains tight on the
btrfs output filesystem (~18 GiB), which the smoke preflight requires 12 GiB of.

## Open

1. Push `cang` (force, with lease) and re-point `v2.0.0-cang.13` / cut
   `v2.0.0-cang.14`; then `nix flake update libkrun-src` and check the locked rev
   equals `git submodule status deps/libkrun` (maintenance.md "Updating the
   libkrun fork", steps 1-2).
2. `krunInitCargoDeps` (`nix/pkgs/libkrun-source.nix`) did **not** move:
   `init/init-binary/Cargo.lock` is unchanged in this rebase. `cargoDeps`
   (`nix/pkgs/cang-rust.nix`) did move with `imago 0.2.5 -> 0.2.6` +
   `parking_lot`/`lock_api`: `sha256-PPvNXtFP64R117I+o5z5BVlqt13BxXrfvNijyCwhHYg=`.
3. The `v2.0.0-cang.*` tags inside the rebased range (`v2.0.0-cang.10`..`.13`)
   now point at commits that are no longer in the branch; they need re-pointing
   (or leaving as historical record) when the push happens.
