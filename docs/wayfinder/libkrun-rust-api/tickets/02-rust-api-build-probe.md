---
label: wayfinder:prototype
title: Prototype - libkrun's Rust API as a cargo path dependency
status: closed
blocked_by: []
claimed_by: pi session (2026-09-28)
---

## Question

Before designing the Nix work, is libkrun's Rust API actually compilable, in this
environment, as a path dependency of a foreign crate with the full feature set
cang needs (and without `ffi`)?

## Resolution

**Yes, cheaply.** A throwaway crate outside the repo depending on
`libkrun = { path = "<repo>/deps/libkrun/src/libkrun", features = ["blk","net","gpu","input","timesync"] }`
built in **41s with zero warnings** (rustc 1.95.0), with only:

```sh
source <bindgenHook>/nix-support/setup-hook && populateBindgenEnv
export PKG_CONFIG_PATH=<virglrenderer-1.3.0>/lib/pkgconfig:<mesa-libgbm>/lib/pkgconfig
```

Recipe, the two mistakes made on the way (dependency key must be `libkrun`, not
`krun`; `cargo fetch` wants a lock file present) and what it does *not* prove (a
real VM graph, the init blob, Nix-sandbox vendoring) are in
`../notes/02-rust-api-build-probe.md`, raw log in
`../notes/02-raw-probe-build.log`.

Tickets 03/04 are this probe's Nix and init-blob counterparts.
