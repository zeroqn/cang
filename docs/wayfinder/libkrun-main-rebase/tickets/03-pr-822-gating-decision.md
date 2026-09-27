---
label: wayfinder:grilling
title: Decide how PR 822 is carried in the fork
status: closed
blocked_by: ["02-pr-822-inert-check"]
claimed_by: bob + pi session (2026-09-27)
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

## Resolution

**Bob, 2026-09-27: defer 822 out of this map entirely.** The rebase carries
**865 and 840 only**. 822 comes back in the effort that gives libkrunfw
`CONFIG_UDMABUF=y` plus the non-upstream virtio-gpu param-10/blob-flag-8 kernel
patch (out of scope here, see the map), or when upstream merges it.

Why this beat the research's mechanical recommendation:

- **Nothing can exercise it** on the pinned stack (ticket 02), so the carry buys
  zero capability today; the one non-cosmetic property it adds - swapping an
  unreachable `panic!` for `ErrUnspec` - is upstream's to land, and the flag that
  would reach that path is rejected by the pinned kernel before the device sees
  it.
- **The price is a vendored crate.** Carrying it as intended means vendoring a
  patched `rutabaga_gfx` because crates.io `0.1.85` has no guest-blob-handle
  support, i.e. maintaining a patched third-party crate across every future
  rebase of a crate upstream just externalised.
- **The PR is a moving draft** whose kernel API is not upstreamed; porting it
  onto ABI-2 now risks doing it twice.

Consequences recorded in the map: the destination drops 822; ticket 04
cherry-picks 865 and 840 only; the prerequisites for 822's return (including the
`VIRTIO_GPU_F_FENCE_PASSING = 5` collision that any future carry must avoid) live
in *Out of scope* and in `notes/02-pr-822-inertness.md`.
