---
label: wayfinder:task
title: Rebase cang onto main and fold in the three PRs
status: open
blocked_by: ["01-upstream-main-delta-for-cang", "03-pr-822-gating-decision"]
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
3. Cherry-pick 822 / 865 / 840 in the order ticket 03 decided, preserving
   authorship and upstream commit messages (plus the drop-me trailer).
4. Verify: `git merge-base --is-ancestor <frozen-main> cang`; `git log --oneline
   <frozen-main>..cang` shows only fork + PR commits; `nix build ./nix/dev#cang-dev`
   (the submodule-aware dev flake) is green.

Do **not** force-push here - that is ticket 06. The rebased branch stays local
until it builds.

## Deliverable

The rebased `cang` ref, the frozen main SHA, the per-fork-commit triage table,
and the `cang-dev` build result.
