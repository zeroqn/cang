---
label: wayfinder:map
title: libkrun fork onto upstream main + PRs 822/865/840
---

## Destination

`deps/libkrun` (the `zeroqn/libkrun` fork, branch `cang`) rebased onto upstream
`main` - the 2.0.0 dev line - with upstream PRs **822** (virtio-gpu
`CREATE_GUEST_HANDLE`), **865** (virtio-blk opt-in parallel reads) and **840**
(guest-clock workarounds) cherry-picked in, published as the permanent fork
release **`v2.0.0-cang.1`**, pinned in `nix/pins.nix` with the submodule pointer
moved to the same commit, with cang green against it and the GPU smoke run on
the new pin.

**Status (2026-09-27): charted, 8 tickets, none resolved.** Frontier: the three
unblocked tickets (01 ABI delta, 02 PR 822 inertness, 05 libkrunfw
compatibility).

## Notes

- Domain: the `zeroqn/libkrun` fork (`deps/libkrun`, branch `cang`); cang's host
  VM launcher (`crates/cang/src/runtime/vm/libkrun/{dynamic,api,launcher}.rs`);
  `nix/pins.nix` + `nix/pkgs/libkrun.nix`; the fork's release workflows; the
  chromium/wayland GPU smokes in `tools/chromium-cang-smoke`.
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
  the three PRs are **cherry-picked into `cang`**, not carried as a scratch
  integration branch or a patch series; the fork **accepts main's 2.0.0
  identity** (`v2.0.0-cang.1`, and whatever soname/asset consequences follow);
  the release route is **push -> CI rolling `cang-<sha>` -> bob dispatches the
  permanent tag -> pin**; validation must reach the **GPU smoke**.
- Standing preference for this effort: prefer an honest, attributable failure
  over a green run that hides one. If a validation step cannot be run here, say
  so explicitly rather than asserting it.
- Starting facts (2026-09-27): base of `cang` = `upstream/stable-1.19.x` tip
  `fb988873` (v1.19.5) + **16 fork commits**; submodule pointer `237ceac0` =
  tag `v1.19.5-cang.1` = the pin; local `cang` is 1 commit ahead of that pointer
  (CI-only, `28e79624`); upstream `main` = `a980e779`, `FULL_VERSION=2.0.0`,
  ~369 commits past the last common ancestor (`8018a20c`, v1.18.0); all three
  PRs are still **open** - 822 is a **draft with mergeable=False**, 865 and 840
  merge cleanly.
- Harness note: `rlm.spawn` children in this session came back tool-less (the
  `waypipe-gpu-smoke` map hit the same), so the `research` tickets are worked by
  a tool-capable session rather than by fired subagents.

## Decisions so far

<!-- one line per closed ticket, gist plus link -->

*(none yet)*

## Not yet specified

- **Base sharpening:** if upstream cuts 2.0.0 (or opens a `stable-2.0.x`) while
  this effort runs, the base should become that tag rather than a frozen `main`
  commit. Sharpens once ticket 04 has fixed the main revision it rebased onto.
- **Retiring the cherry-picks:** when each of 822/865/840 merges upstream, how
  the local copy is retired (drop the commit at the next rebase, or carry it
  until the next release bump). Sharpens per PR as upstream moves.
- **The next update's base:** *this* rebase goes to `main` because the PRs live
  there; the standing policy after it (back to the stable line, as decided, or
  stay on main) is a later effort's decision, informed by what this one costs.
- **Optional-symbol drift:** whether cang's `LibkrunApi` optional bindings
  (`Option<fn>` fields) need new entries for main's new API surface. Sharpens
  once ticket 01 reports the ABI delta.
- **Fork CI adaptation:** whether the fork's `publish-cang-release.yml` (copied
  from an older upstream) needs more than the known `LIBDIR_Linux=lib64` fix to
  build main. Sharpens inside ticket 06 if the release run fails.

## Out of scope

- **Consuming libkrun through a Rust interface instead of the C ABI.** Bob's
  separate question (2026-09-27): cang today `dlopen`s `libkrun.so`
  (`CANG_LIBKRUN_LIBRARY`) and binds ~25 `krun_*` symbols behind the
  `LibkrunApi` trait; libkrun's crates are workspace-internal path deps
  (crates.io `libkrun` is a cdylib, `krun-sys` is only C bindings), so
  Rust-direct means vendoring the VMM crates into cang's build and dropping the
  prebuilt-pin pipeline. It gets **its own map/effort**, not a ticket here.
- **Making 822's zero-copy SHM path real end to end.** That needs the
  not-yet-upstreamed guest kernel virtio-gpu API (param 10) plus udmabuf on the
  guest side, i.e. a libkrunfw rebase. This map carries the host side only.
- **Exposing 865's parallel reads and 840's clock workarounds from cang**
  (CLI/config + FFI wiring). The fork carries them; using them is a follow-on
  effort.
- **Rebasing `deps/libkrunfw`.** Not this effort. If ticket 05 finds the pinned
  fw cannot boot libkrun 2.0.0, the destination is redrawn (a new effort), not
  silently widened here.
- macOS/Windows variants of libkrun; the fork stays Linux-only in cang.
