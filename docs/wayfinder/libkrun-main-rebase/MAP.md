---
label: wayfinder:map
title: libkrun fork onto upstream main + PRs 865/840
---

## Destination

`deps/libkrun` (the `zeroqn/libkrun` fork, branch `cang`) rebased onto upstream
`main` - the 2.0.0 / **ABI 2** line - with upstream PRs **865** (virtio-blk
opt-in parallel reads) and **840** (guest-clock workarounds) folded in,
published as the permanent fork release **`v2.0.0-cang.1`**, pinned in
`nix/pins.nix` with the submodule pointer moved to the same commit, with cang
ported to libkrun's v2 C API and green against it, and the GPU smoke run on the
new pin.

**Status (2026-09-28): tickets 01-07 and 09-13 resolved. Ticket 08 is diagnosed
and fixed locally; it and the new ticket 14 (publish the fix, re-pin, re-run the
smoke) are the remaining work.**
The fork's `cang` branch is pushed (tip `d578e4e2`) and the permanent release
**`v2.0.0-cang.1`** is published and pinned. cang is ported to the ABI-2 object
API (ticket 09) and live-boots a guest - but **only against a locally built
libkrun**: the published asset is hollow. Its `libkrun.so.2.0.0` exports zero
`krun_*` symbols because the release workflow builds without `FFI=1` (ABI 2 put
the C entry points behind that feature) and only checks that the files exist;
`nix/dev` had the same gap and is fixed. That fix, the re-published asset and the
re-pin are ticket **13**, and it needs bob's push/dispatch. Ticket 10 re-added the
fork's own C extensions on the ABI-2 device/builder
(`krun_gpu_device_set_render_server_fd`, `krun_vmm_builder_set_profile_path`, the
fence-retirement/poll fixes) and folded the `FFI=1` fix into the release
workflow, which now asserts the exported symbols before packaging. The permanent
release **`v2.0.0-cang.2`** (tag on `18267332`) is published and pinned: its
`libkrun.so.2` exports 101 `krun_*` symbols where `v2.0.0-cang.1` exported none,
and `nix build .#cang` now boots a guest (ticket 13), which also answered ticket
05 - the pinned fw is fine, PID 1 is the injected init blob and the guest's
virtio devices and status files all come up. The fork is rebased (main + CI/docs +
PRs 865/840 + four fork commits) and `nix build ./nix/dev#cang-dev` is green.
Upstream `main` is a **ground-up C-ABI rewrite** (`a3d31822`, 2026-09-11;
`ABI_VERSION=2`, `libkrun.so.2`): 20 of cang's 22 bound `krun_*` symbols are
gone, init injection is caller-supplied via `libkrun_init.so`, and
`rutabaga_gfx` moved to crates.io - so cang's launcher is **ported to the v2
builder/object API** (tickets 09/10). PR **822 is out of this map** (bob,
2026-09-27): it is inert on the pinned kernel, its own commit would collide with
our `VIRTIO_GPU_F_FENCE_PASSING = 5`, and carrying it as intended would mean
vendoring a patched `rutabaga_gfx` for a capability nothing can exercise.
Frontier: ticket **14** - publish the ABI-2 GPU fixes as `v2.0.0-cang.3` and
re-pin, which is what closes ticket **08**. The smoke's verdict on the *pinned*
`v2.0.0-cang.2` is a real regression (venus degrades to 2D), it is attributed to
three device-side causes and fixed in the fork's `3d7af2c2`, and both smoke modes
pass against that library locally; only the publication is missing. "Not yet
specified" below still names the map's open design questions.

### The GPU regression in one paragraph

ABI 2's device code dropped two things the fork's vendored rutabaga used to do:
it never told virglrenderer which DRM render node to use (vrend winsys and VA-API
video got `-1`), and it advertises the DRM native-context capset, which - with
upstream rutabaga's always-on `ASYNC_FENCE_CB` - makes virglrenderer run
`drm_renderer_init`; when that probe fails (no virtio native context on a plain
amdgpu host) virglrenderer gives up on the whole backend and rutabaga falls back
to 2D, taking venus with it. Fixing the first and suppressing the DRM capset when
a render server is present also requires the device's `num_capsets` to come from
the same mask, otherwise the guest's capset enumeration times out instead.

## Notes

- Domain: the `zeroqn/libkrun` fork (`deps/libkrun`, branch `cang`); cang's host
  VM launcher (`crates/cang/src/runtime/vm/libkrun/{dynamic,api,launcher,tests}.rs`);
  `nix/pins.nix`, `nix/pkgs/libkrun.nix`, `nix/pkgs/cang-{prebuilt,rust}.nix`,
  `nix/dev/`; the fork's release workflows; the chromium/wayland GPU smokes in
  `tools/chromium-cang-smoke`.
- Skills to consult: `cang-libkrun-family-fork-rebase-onto-release` (rebase
  procedure, conflict rule, CI/LIBDIR traps), `cang-fork-versioned-release-tags-and-pins`
  (permanent tags, attestations, the release pin gate),
  `cang-prebuilt-release-pin-naming` (asset naming),
  `cang-local-validation-gates` (what is checkable on this host),
  `cang-guest-gpu-chromium-diagnosis` (ticket 08).
- Tracker: **local markdown**, this directory. No issue tracker was provided for
  the session and `gh` is unauthenticated here, so the map is files under
  `docs/wayfinder/` and the only GitHub actions are pushes by bob and a
  `workflow_dispatch` by bob. Evidence files go in `notes/`.
- **Execution is in scope** for this map (bob, 2026-09-27): it ends with the
  rebased fork published, pinned and smoke-validated, not with a plan.
- Destination decisions taken while charting (bob, 2026-09-27):
  **865 and 840** are cherry-picked into `cang` (822 deferred, see *Out of
  scope*); the fork **accepts main's 2.0.0 identity**; the release route is
  **push -> CI rolling `cang-<sha>` -> bob dispatches the permanent tag -> pin**;
  validation must reach the **GPU smoke**; **cang is ported to libkrun's v2
  API** - no fork-side v1 compatibility shim (ticket 09); the **fork's GPU
  render-server-fd hook is ported onto main's `GpuDevice`** (ticket 10); the
  destination stays **one map** rather than splitting fork-release from
  cang-integration.
- Starting facts (2026-09-27): base of `cang` = `upstream/stable-1.19.x` tip
  `fb988873` (v1.19.5) + **16 fork commits**; submodule pointer `237ceac0` =
  tag `v1.19.5-cang.1` = the pin; local `cang` is 1 commit ahead of that pointer
  (CI-only, `28e79624`); upstream `main` = `a980e779`, `FULL_VERSION=2.0.0`,
  369 commits past the last common ancestor (`8018a20c`, v1.18.0).
- Triage finding (2026-09-27, `notes/04-fork-commit-triage.md`): of the fork's
  16 commits, **seven replay** onto `main` (CI + docs), **three drop as
  superseded** (upstream rustfmt; and the vaapi DRM work, which crates.io
  `rutabaga_gfx` 0.1.85 already ships - `get_drm_fd` with an `O_RDWR` render-node
  open), and **six re-derive** on the ABI-2 API (profiling, the render-server fd,
  the fence/blob-map/poll device fixes) as ticket 10. **No vendored
  `rutabaga_gfx` is needed**: the crate provides `set_server_descriptor`,
  `set_use_render_server`, `poll_descriptor` and `get_drm_fd`. The rebased branch
  is therefore upstream `main` + fork CI/docs until ticket 10 lands.
- Harness note: `rlm.spawn` children **do** have tools in this session (tickets
  01 and 02 were resolved by research children), unlike the
  `waypipe-gpu-smoke` map's experience.

## Decisions so far

- [Publish the GPU fixes as v2.0.0-cang.3 and re-pin](tickets/14-publish-gpu-fix.md):
  push the fork's `3d7af2c2`, tag `v2.0.0-cang.3`, re-pin, and re-run both smoke
  modes against the published artifact (this is what closes ticket 08).
- [GPU smoke the new pin](tickets/08-gpu-smoke-on-new-pin.md): run on the pinned
  `v2.0.0-cang.2` - FAIL with `renderer=no-webgl`, a rebase regression (the
  2026-09-26 baseline on the v1 pin shows the venus renderer). Fixed in the
  fork's device code; both modes PASS against that library; publication pending.
- [Does the pinned libkrunfw boot libkrun 2.0.0?](tickets/05-libkrunfw-compat-check.md):
  yes - live boot on the pinned fw/kernel (`6.12.109-hardened1`), PID 1 = the
  injected `init.krun` blob, guest status files under `/run/cang`, all configured
  virtio devices present, `/dev/zram0` swap up.
- [Rebuild the v2 release with the C ABI and re-pin](tickets/13-release-c-abi-repin.md):
  `v2.0.0-cang.2` published (CI run 36371495907) and pinned; the pinned
  `libkrun.so.2` exports 101 `krun_*` symbols, `libkrun_init.so.0` 33, and
  `nix build .#cang` boots.
- [Re-add the fork's C extensions on main's device code](tickets/10-fork-c-extensions-on-main.md):
  `krun_gpu_device_set_render_server_fd` (owned fd -> `RutabagaBuilder::set_server_descriptor`),
  `krun_vmm_builder_set_profile_path` + `vmm::profile::KrunProfiler`, the
  fence-retirement/poison-safe-lock/two-tier-poll device fixes, the regenerated
  header + schema, and the release workflow's `FFI=1` build with a symbol
  assertion. Verified with a local `FFI=1` build (101 `krun_*` symbols), clippy on
  the CI feature set, a `--gpu=drm` boot and a `--profile` artifact.
- [Port cang's launcher to libkrun's v2 API](tickets/09-port-cang-to-v2-api.md):
  the ABI-2 object model in `api.rs`/`dynamic.rs`/`launcher.rs` (`krun_init_config_apply_in`
  supplies the init from the shipped `libkrun_init.so`, DHCP moved into the init
  config, the managed kernel console became the leading console device, TSI
  forwards are `guest:host`, a headless display backend is passed to
  `krun_gpu_device_new`), the v2 call-shape fixtures, and a live boot. Finding:
  the pinned release exports no C ABI (see ticket 13).
- [Rebuild the v2 release with the C ABI and re-pin](tickets/13-release-c-abi-repin.md):
  `FFI=1` in the release workflow plus a symbol-table assertion, one
  `v2.0.0-cang.2` carrying ticket 10, then re-pin.
- [Pin v2.0.0-cang.1 and adapt cang](tickets/07-pin-and-adapt-cang.md): pin `v2.0.0-cang.1` with both asset hashes; the submodule pointer moves to the tag's commit `d578e4e2`; `DEFAULT_LIBKRUN_NAMES` gains `libkrun.so.2`; both packagers ship `libkrun_init.so*`; `libkrun.nix` gives the init blob `$ORIGIN` and drops the now-dead pipewire edge. Expected window: cang launches fail until ticket 09 ports the binding.
- [Publish the permanent v2.0.0-cang.1 fork release](tickets/06-publish-v2-release.md): pushed the rebased branch to the fork's `cang` (force-with-lease) and pushed the `v2.0.0-cang.1` tag with plain `git` - the workflow triggers on tags, so no `workflow_dispatch` auth was needed; CI published both assets with the init blob, pc files and headers included.
- [Make nix/dev build main's libkrun (musl guest init)](tickets/12-nix-dev-build-main-libkrun.md): split the build - `pkgsStatic`'s rust builds `init/init-binary` for musl (with `timesync`), and `KRUN_INIT_BINARY_PATH` makes the ordinary Makefile flow embed it, so no Makefile patching; plus `withTimesync`, no `withSound`, `version = 2.0.0-cang`, and `rustfmt` for ffier. `nix build ./nix/dev#cang-dev` is green.
- [Rebase cang onto main and fold in the three PRs](tickets/04-rebase-cang-onto-main.md): `cang-main-rebase` = `a980e779` + 7 replayed fork commits (CI/docs), 7 cherry-picked PR commits (865, 840) and 1 adaptation commit; the fork's Rust delta is absent by design (ticket 10), the pin/pointer are untouched (ticket 07), and the build check passes.

<!-- one line per closed ticket, gist plus link -->

- [Upstream-main delta for cang's libkrun integration](tickets/01-upstream-main-delta-for-cang.md): main is an ABI-2 rewrite, not a superset - 20 of cang's 22 bound `krun_*` symbols deleted, `krun_init_log`/`krun_check_nested_virt` changed, init injection now caller-supplied via `libkrun_init.so`, `rutabaga_gfx` moved to crates.io, soname `libkrun.so.2`; the fork's own extensions (`krun_set_gpu_options3`, `krun_set_profile_path`) have no main equivalent and must be re-added.
- [Is cherry-picked PR 822 inert without the guest side?](tickets/02-pr-822-inert-check.md): the *path* is inert (pinned kernel rejects blob flag 0x8 in `verify_blob()`, param 10 is `-EINVAL`, `CONFIG_UDMABUF` unset on every arch) but the *patch* is not - `230f2c55` renumbers `VIRTIO_GPU_F_CREATE_GUEST_HANDLE` 6->5, colliding with our kernel's `VIRTIO_GPU_F_FENCE_PASSING = 5`; and its rutabaga half cannot be cherry-picked because crates.io `rutabaga_gfx 0.1.85` dropped guest-blob handles.
- [Do 865 and 840 apply to main's post-rewrite tree?](tickets/11-pr865-840-matrix.md): both apply **cleanly as ordered sequences** (865: `802c9e1e`+`6b24d0d2`+`08a8773a`; 840: `3f3062e3`+`32eb92b5`+`6d800f98`+`2f37b0a3`) and are replayed on the rebased branch; 865's parallel reads are opt-in via a new `krun_block_device_set_parallel_reads` (stays off, only its `RwLock` refactor is live) and 840's guest half needs `TIMESYNC=1`, added to the fork's prebuilt workflow along with asserts on `libkrun_init.{so*,pc,h}`.
- [Decide how PR 822 is carried in the fork](tickets/03-pr-822-gating-decision.md): **deferred out of this map** - the rebase carries 865 and 840 only; 822 returns with the libkrunfw `CONFIG_UDMABUF=y` + param-10/flag-8 kernel work, or when upstream merges it.

## Not yet specified

- **Base sharpening:** if upstream cuts 2.0.0 (or opens a `stable-2.0.x`) while
  this effort runs, the base should become that tag rather than a frozen `main`
  commit. Sharpens once ticket 04 has fixed the revision it rebased onto.
- **Retiring the cherry-picks:** when 865 and 840 merge upstream, how the local
  copies are retired. Sharpens per PR.
- **How much v2 API churn to absorb:** `main` is a dev branch that just bumped
  `ABI_VERSION`; the two PRs were written against slightly different revisions of
  it. Sharpens as 04 and 11 land.
- **Survival of the fork's smaller extensions under main's restructuring:**
  nested-virt, the console/vsock/port-map surface and `cang`'s profile-path hook
  now sit on a reorganised workspace (`src/vmm` inside `src/libkrun`). Sharpens
  inside ticket 10.
- **Whether the device-level GPU fixes (fence retirement, blob-map overflow,
  idle poll, render-node gating) still apply to main's device code**, or whether
  main/`rutabaga_gfx` 0.1.85 already covers some of them. Sharpens inside ticket
  10; ticket 08's smoke is the backstop.
- **The next update's base:** *this* rebase goes to `main` because the PRs live
  there; the standing policy after it (back to the stable line, as decided, or
  stay on main) is a later effort's decision, informed by what this one costs.
- **How much the dev-build gap constrains verification:** if `nix/dev` builds
  only the host library (ticket 12's option 2), then every live-boot claim in
  tickets 09/10 has to come from a published prebuilt. Sharpens with ticket 12's
  answer.
- **Fork CI adaptation:** whether the fork's `publish-cang-release.yml` (copied
  from an older upstream) needs more than the known `LIBDIR_Linux=lib64` fix and
  a feature-list update to build main. Sharpens inside ticket 06 if the release
  run fails.

## Out of scope

- **PR 822's host side (the zero-copy SHM fast path).** Deferred by bob
  (2026-09-27, ticket 03). Its return conditions, all prerequisites for the path
  to run at all: libkrunfw must gain `CONFIG_UDMABUF=y` (currently unset on
  x86_64, aarch64 and riscv64) *and* the non-upstream virtio-gpu param-10 +
  blob-flag-8 kernel patch; the host needs `/dev/udmabuf` with the seal contract
  the patch expects; and the guest-blob-handle plumbing has to come from
  upstream `rutabaga_gfx` (crates.io `0.1.85` dropped it) or a vendored patched
  crate. **Any future carry must keep `VIRTIO_GPU_F_CREATE_GUEST_HANDLE = 6`**,
  not the PR's `230f2c55` value of 5, which our kernel uses for
  `VIRTIO_GPU_F_FENCE_PASSING`. Evidence: `notes/02-pr-822-inertness.md`.
- **Consuming libkrun through a Rust interface instead of the C ABI.** Bob's
  separate question (2026-09-27): cang today `dlopen`s `libkrun.so` and binds
  `krun_*` symbols behind the `LibkrunApi` trait; Rust-direct means vendoring the
  VMM crates into cang's build and dropping the prebuilt-pin pipeline. It gets
  **its own map/effort**, not a ticket here.
- **A fork-side v1 compatibility shim.** Considered in the re-chart and rejected
  by bob (2026-09-27) in favour of porting cang to the v2 API (ticket 09). Do not
  re-open it inside this effort; if the port stalls on a symbol with no v2
  equivalent, that is a finding for ticket 09, not a licence to add the shim.
- **Exposing 865's parallel reads and 840's clock workarounds from cang**
  (CLI/config wiring). The fork carries them; using them is a follow-on effort.
- **Rebasing `deps/libkrunfw`.** Not this effort: ticket 01 showed the fw/kernel
  ABI is unchanged on main (`krunfw_get_kernel` signature, `KernelBundle`,
  `DEFAULT_KERNEL_CMDLINE`, virtio-mmio + MP tables all the same), so the pinned
  `v5.6.2-cang.1` stays. If a live boot later contradicts that, the destination
  is redrawn (a new effort), not silently widened here.
- macOS/Windows variants of libkrun (`src/hvf`, `src/whp`); the fork stays
  Linux-only in cang.
