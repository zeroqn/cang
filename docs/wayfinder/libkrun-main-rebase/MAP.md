---
label: wayfinder:map
title: libkrun fork onto upstream main + PRs 822/865/840
---

## Destination

`deps/libkrun` (the `zeroqn/libkrun` fork, branch `cang`) rebased onto upstream
`main` - the 2.0.0 / **ABI 2** line - with upstream PRs **822** (virtio-gpu
`CREATE_GUEST_HANDLE`), **865** (virtio-blk opt-in parallel reads) and **840**
(guest-clock workarounds) folded in, published as the permanent fork release
**`v2.0.0-cang.1`**, pinned in `nix/pins.nix` with the submodule pointer moved
to the same commit, with cang ported to libkrun's v2 C API and green against it,
and the GPU smoke run on the new pin.

**Status (2026-09-27): re-charted after ticket 01.** Ticket 01 closed with a
finding that redraws the route: upstream `main` is not a superset of
`stable-1.19.x` but a **ground-up C-ABI rewrite** (`a3d31822` *"lib: remove old
C API"*, 2026-09-11; `ABI_VERSION=2`, `libkrun.so.2`). 20 of cang's 22 bound
`krun_*` symbols are gone, 2 changed signature, implicit init injection was
removed, `rutabaga_gfx` left the tree for crates.io, and the workspace was
restructured (`src/vmm` now lives under `src/libkrun/src/vmm`). "Re-pin and fix
what breaks" is therefore an integration project: **cang's launcher is ported to
the v2 builder/object API** (bob's route decision, below). 9 tickets; 2
resolved-able frontier items (02 research), the rest blocked behind the rebase.

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
  the three PRs are **cherry-picked into `cang`**; the fork **accepts main's
  2.0.0 identity**; the release route is **push -> CI rolling `cang-<sha>` ->
  bob dispatches the permanent tag -> pin**; validation must reach the **GPU
  smoke**; and, after ticket 01, **cang is ported to libkrun's v2 API** - no
  fork-side v1 compatibility shim (ticket 09); the **fork's GPU render-server-fd
  hook is ported onto main's `GpuDevice`** (ticket 10); the destination stays
  **one map** rather than splitting fork-release from cang-integration.
- Starting facts (2026-09-27): base of `cang` = `upstream/stable-1.19.x` tip
  `fb988873` (v1.19.5) + **16 fork commits**; submodule pointer `237ceac0` =
  tag `v1.19.5-cang.1` = the pin; local `cang` is 1 commit ahead of that pointer
  (CI-only, `28e79624`); upstream `main` = `a980e779`, `FULL_VERSION=2.0.0`,
  369 commits past the last common ancestor (`8018a20c`, v1.18.0); all three
  PRs are open - 822 is a **draft with mergeable=False**, 865 and 840 merge
  cleanly. 822 (created 2026-08-27) and 840 (2026-09-05) **predate the
  2026-09-11 ABI rewrite**; 865 (2026-09-17) postdates it.
- Harness note: `rlm.spawn` children *do* have tools in this session (ticket 01
  was resolved by one), unlike the `waypipe-gpu-smoke` map's experience.

## Decisions so far

<!-- one line per closed ticket, gist plus link -->

- [Upstream-main delta for cang's libkrun integration](tickets/01-upstream-main-delta-for-cang.md): main is an ABI-2 rewrite, not a superset - 20 of cang's 22 bound `krun_*` symbols deleted, `krun_init_log`/`krun_check_nested_virt` changed, init injection now caller-supplied via `libkrun_init.so`, `rutabaga_gfx` moved to crates.io, soname `libkrun.so.2`; the fork's own extensions (`krun_set_gpu_options3`, `krun_set_profile_path`) have no main equivalent and must be re-added. Full detail in `notes/01-upstream-main-delta.md`.

## Not yet specified

- **Base sharpening:** if upstream cuts 2.0.0 (or opens a `stable-2.0.x`) while
  this effort runs, the base should become that tag rather than a frozen `main`
  commit. Sharpens once ticket 04 has fixed the revision it rebased onto.
- **Retiring the cherry-picks:** when each of 822/865/840 merges upstream, how
  the local copy is retired. Sharpens per PR; 822 and 840 are pre-rewrite, so
  their local form may be a v2 adaptation rather than the upstream diff.
- **How much v2 API churn to absorb:** `main` is a dev branch that just bumped
  `ABI_VERSION`; 822/865/840 were written against slightly different revisions
  of it. Sharpens as 02 and 04 land.
- **Survival of the fork's smaller extensions under main's restructuring:**
  nested-virt, the console/vsock/port-map surface and `cang`'s profile-path hook
  now sit on a reorganised workspace (`src/vmm` inside `src/libkrun`). Sharpens
  inside ticket 10.
- **The next update's base:** *this* rebase goes to `main` because the PRs live
  there; the standing policy after it (back to the stable line, as decided, or
  stay on main) is a later effort's decision, informed by what this one costs.
- **Fork CI adaptation:** whether the fork's `publish-cang-release.yml` (copied
  from an older upstream) needs more than the known `LIBDIR_Linux=lib64` fix and
  a feature-list update to build main. Sharpens inside ticket 06 if the release
  run fails.

## Out of scope

- **Consuming libkrun through a Rust interface instead of the C ABI.** Bob's
  separate question (2026-09-27): cang today `dlopen`s `libkrun.so` and binds
  `krun_*` symbols behind the `LibkrunApi` trait; Rust-direct means vendoring the
  VMM crates into cang's build and dropping the prebuilt-pin pipeline. It gets
  **its own map/effort**, not a ticket here.
- **A fork-side v1 compatibility shim.** Considered in the re-chart and rejected
  by bob (2026-09-27) in favour of porting cang to the v2 API (ticket 09). Do not
  re-open it inside this effort; if the port stalls on a symbol with no v2
  equivalent, that is a finding for ticket 09, not a licence to add the shim.
- **Making 822's zero-copy SHM path real end to end.** That needs the
  not-yet-upstreamed guest kernel virtio-gpu API (param 10) plus udmabuf on the
  guest side, i.e. a libkrunfw rebase. This map carries the host side only (and,
  per ticket 02, may carry it inert).
- **Exposing 865's parallel reads and 840's clock workarounds from cang**
  (CLI/config wiring). The fork carries them; using them is a follow-on effort.
- **Rebasing `deps/libkrunfw`.** Not this effort: ticket 01 showed the fw/kernel
  ABI is unchanged on main (`krunfw_get_kernel` signature, `KernelBundle`,
  `DEFAULT_KERNEL_CMDLINE`, virtio-mmio + MP tables all the same), so the pinned
  `v5.6.2-cang.1` stays. If a live boot later contradicts that, the destination
  is redrawn (a new effort), not silently widened here.
- macOS/Windows variants of libkrun (`src/hvf`, `src/whp`); the fork stays
  Linux-only in cang.
