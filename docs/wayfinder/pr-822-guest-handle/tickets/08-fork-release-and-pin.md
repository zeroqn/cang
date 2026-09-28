---
label: wayfinder:task
title: Publish the libkrun release and repin the fork
status: open
blocked_by: ["07-fork-vmm-port"]
---

## Question

Bob pushes the branch and tag (the workflow triggers on the tag), CI builds with
`FFI=1` and asserts the exported symbols, then:

- confirm the published `libkrun.so.2` still exports the expected `krun_*` set
  and `libkrun_init.so.0` is present (a hollow asset bit the fork once),
- move the `deps/libkrun` submodule pointer to the tagged commit,
- update `nix/pins.nix` via the fork's update script and refresh the
  `fetchCargoVendor` hash in the same commit,
- confirm `nix build .#cang` boots.

## Deliverable

The release published and pinned, with the symbol assertion and a boot as
evidence recorded here.
