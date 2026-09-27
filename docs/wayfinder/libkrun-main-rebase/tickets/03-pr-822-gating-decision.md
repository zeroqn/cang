---
label: wayfinder:grilling
title: Decide how PR 822 is carried in the fork
status: open
blocked_by: ["02-pr-822-inert-check"]
claimed_by: unclaimed
---

## Question

Given ticket 02's verdict, decide the shape of 822 in `cang`:

- **Unconditional cherry-pick** - the PR's commits replayed onto the rebased
  branch, advertises the feature unconditionally, and we accept that the guest
  cannot use it until the kernel side exists.
- **Fork-local gate** - cherry-pick it behind a cang-specific cargo
  feature / cfg (or keep the feature unadvertised unless an env or flag asks for
  it), so no cang build can reach it by accident, at the cost of a fork-only
  divergence to maintain until upstream lands it.
- **Drop it** (only if 02 shows it is unsafe): leave the host side out and
  revisit when the guest side exists.

Also settle: the commit shape (one commit per PR with a `(cherry picked from
upstream PR #822 …)` trailer so it is droppable, versus merging the PR head),
and the cherry-pick order relative to 865/840.

## Deliverable

The decision, recorded here, with the exact commit/feature shape ticket 04 must
produce.
