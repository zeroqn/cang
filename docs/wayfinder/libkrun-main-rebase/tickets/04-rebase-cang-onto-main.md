---
label: wayfinder:task
title: Rebase cang onto main and fold in the three PRs
status: open
blocked_by: ["03-pr-822-gating-decision"]
claimed_by: pi session (2026-09-27)
---

## Question

Perform the rebase in `deps/libkrun` and leave the result buildable:

1. Freeze the base: pick the upstream `main` commit to rebase onto and record
   its full SHA (upstream is moving; the map's pins must name one commit).
2. Rebase: `git rebase --onto <frozen-main> <v1.19.5 commit> cang`, replaying
   the 16 fork commits. Triage each one - *keep*, *drop as superseded*, or
   *adapt* - and record the verdict per commit. Conflict rule from the
   `cang-libkrun-family-fork-rebase-onto-release` skill: **take upstream's
   structure and re-apply the fork's addition**; never take the fork side
   wholesale. Expect conflicts in the GPU files, `src/libkrun/src/lib.rs`,
   `src/vmm/src/builder.rs`, `src/vmm/src/lib.rs`.
3. Cherry-pick **865 and 840** (822 is out of scope - bob, 2026-09-27),
   preserving authorship and upstream commit messages plus a
   `(cherry picked from upstream PR #N …)` trailer so each is droppable. Mind
   that both PRs are post-rewrite (ticket 11 established that 840's head was
   force-pushed 2026-09-14/15, after `a3d31822`), so neither needs adaptation -
   see the resolution note below for the committed sequences.
3b. Re-establish the fork's own C extensions on main's structure - that is
   ticket 10, which runs alongside this one; do not leave the rebase "green but
   missing `krun_set_gpu_options3`" without saying so.
4. Verify: `git merge-base --is-ancestor <frozen-main> cang`; `git log --oneline
   <frozen-main>..cang` shows only fork + PR commits; `nix build ./nix/dev#cang-dev`
   (the submodule-aware dev flake) is green.

Do **not** force-push here - that is ticket 06. The rebased branch stays local
until it builds.

## Deliverable

The rebased `cang` ref, the frozen main SHA, the per-fork-commit triage table,
and the `cang-dev` build result.

## Resolution (2026-09-27, pi session)

Rebased and folded in; the build check is the last open item.

**Branch:** local `cang-main-rebase` = the frozen base
`a980e7795b86c8151e867b32a43af4cb984364fa` (`main`, 2.0.0 / ABI 2) plus
**fifteen commits**: the seven replayed fork commits (all CI + `CANG.md`), the
seven cherry-picked commits of upstream PRs 865 and 840, and one adaptation
commit (`docs+ci: adapt the fork's docs and prebuilt build to the ABI-2 base`).
The pre-rebase tip is preserved as `cang-pre-main-rebase` (`28e79624`); nothing
was pushed, and the submodule pointer is deliberately still the pinned
`237ceac0` (ticket 07 moves it).

**Triage:** `../notes/04-fork-commit-triage.md` - seven replayed, three dropped
as superseded (upstream rustfmt; the vaapi DRM work, which crates.io
`rutabaga_gfx` 0.1.85 already ships), six handed to ticket 10 for re-derivation
on the ABI-2 API (profiling, the render-server fd, the GPU device fixes). The
rebased branch therefore contains **no fork Rust code** except `CANG.md` and the
CI workflows, which is expected: every line of the fork's C-side delta extends an
API that main replaced.

**Build check: FAILED, for a nix-side reason - ticket 12 owns it.** With the
`fetchCargoVendor` fix in place the build reaches main's libkrun and dies in
`krun-init-blob`'s build script, which requires the musl rust std that
`nix/dev`'s host toolchain does not have (`../notes/04-build-check.md`). So the
rebased content is not yet compile-verified, and this ticket stays open until
ticket 12 gives it a build path.

**Build enablement found on the way:** `nix/dev` could vendor main's dependency
set only after switching the fork-source override from `importCargoLock` to
`rustPlatform.fetchCargoVendor`, because upstream's lock pulls `ffier` from git
at two revisions under one name-version.
