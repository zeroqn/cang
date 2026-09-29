---
label: wayfinder:task
title: Make a regression in the wrapper wiring fail loudly
status: open
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: ""
---

## Question

The cheap guard the destination promises: something in the repo that fails when
the ticket-05 wiring is undone - the wrapper losing its `LD_LIBRARY_PATH`
prefix, or the runtime directory losing `libstdc++.so.6`.

Pick the cheapest check that actually catches both, following the repo's
existing patterns:

- `nix/image/checks.nix` wrapper contracts assert the image carries the wiring
  (see `wrapperContracts`, and the absence-check recipe for layer contents);
- a repository test under `crates/cang-repository-tests/` can assert the
  derivation's shape (`nix/pkgs/pi-coding-agent.nix` installs a wrapper that
  names the runtime directory, and the directory derivation links
  `libstdc++.so.6`);
- the probe from ticket 06 is the end-to-end complement, not a substitute: it is
  slow and needs a guest.

Requirement: build the check *first* and confirm it fails against the unwired
tree, then confirm it passes once ticket 05 lands.
