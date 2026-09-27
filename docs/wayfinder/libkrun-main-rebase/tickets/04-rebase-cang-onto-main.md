---
label: wayfinder:task
title: Rebase cang onto main and fold in the three PRs
status: open
blocked_by: ["03-pr-822-gating-decision"]
claimed_by: unclaimed
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
   that **840 (2026-09-05) predates main's 2026-09-11 ABI rewrite** while 865
   (2026-09-17) postdates it; record the adaptation each needed.
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
