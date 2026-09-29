---
label: wayfinder:task
title: Reproduce the addon failure inside a real cang guest before the mechanism is chosen
status: open
blocked_by: []
claimed_by: ""
---

## Question

Unit of work, not a decision: boot a real cang microVM from the current tree and
show, with fresh evidence, what `require('sharp')` and
`require('onnxruntime-node')` do inside it - the failure the mechanism ticket is
being decided on. Ticket 01 established the failure by reproducing the guest's
loader condition on the host (bun + the guest's `pkgs.mimalloc` preload, `/etc`
masked); this ticket is the same claim made on the real target, plus the container
image build that every later verification needs anyway.

Scope:

- Isolated config/state and a hermetic container storage directory, house recipe
  from `tools/chromium-cang-smoke/chromium-smoke.sh`; the 40G btrfs loop image at
  `/home/dev/cang/disk` is the place for both (the host home filesystem is small).
- Record: the exact launch, the image digest, the `pi`/`bun` identity in the
  guest, both modules' outcomes verbatim, and `LD_DEBUG=libs` evidence for how
  `libstdc++.so.6` was sought (and what the guest's `/etc/ld-nix.so.preload`
  contained at the time).
- Do not change the repository or the image for this ticket; it is a measurement.

Known risks to name in the answer if they bite: `nix build .#container` failed
earlier in the run (a `syn` compile failure while the host disk was full, and the
image wrapper checks fail if the store DB check misses references), and a cold
image build can take a long time.

Resolved when the in-guest outcome is recorded as evidence, whether or not it
matches ticket 01's prediction.
