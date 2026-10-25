---
label: wayfinder:grilling
title: Should the agentbox image ship cang's own build inputs so cang can be built inside a cang guest?
status: open
blocked_by: []
claimed_by: ""
---

## Question

bob's workload suggestion was: build **our own cang code** inside a cang guest.
The harness now does exactly that (`--workload cang`: a copy of the repo at HEAD,
`cargo build --offline -p cang`), and it works on the host in 55.9 s - but in the
guest it stops at the first build script: the image's `/nix` closure has clang,
libclang, gcc, rustc, cargo, pkg-config, mesa-libgbm, libdrm, yet **no
virglrenderer**, which `deps/rutabaga_gfx/build.rs` requires (via
`cang-libkrun` -> libkrun -> rutabaga_gfx).

Decide:

- **Ship the inputs in the image.** Add `pkgs.virglrenderer` (and anything else
  the build turns out to want) to `cToolchainImagePackages` in
  `nix/image/layers.nix:281-285`. Small diff, rebuilds the C-toolchain layer, and
  it buys more than the benchmark: "build cang inside a cang guest" becomes
  possible at all, which is the development experience bob asked about. Costs: an
  image rebuild + `podman load` + the usual live verification, and a slightly
  larger image.
- **Restrict the in-guest workload** to crates the image can already build
  (`cang-guest-init`, `cang-exec-protocol`, `cang-attach-protocol`) and keep the
  full-tree workload host-only. No image change, but the guest arm then measures a
  much smaller crate graph and says little about real cang development.
- **Mount the host's whole `/nix` in the guest** so any store path is visible.
  That is cang-side plumbing (a mount-layout change), which this map's scope
  excludes as a lever - listed only so the option is on the record.

Whatever is chosen, the harness records the same three arms and the same fields
for this workload.
