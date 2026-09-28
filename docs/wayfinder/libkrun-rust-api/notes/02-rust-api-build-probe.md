# Prototype: does libkrun's Rust API build here?

Ticket 02. Run 2026-09-28 on the dev host, **outside** the repo
(`$SCRATCH/rustapi-probe`), with the workspace's rustc 1.95.0.

## Probe

```toml
# Cargo.toml  (a throwaway crate; libkrun's own Cargo.lock was copied in first so
# the resolution starts from the fork's pinned versions)
[package]
name = "rustapi-probe"
version = "0.1.0"
edition = "2021"

[dependencies]
libkrun = { path = "<repo>/deps/libkrun/src/libkrun", features = ["blk", "net", "gpu", "input", "timesync"] }
```

```rust
// src/main.rs
fn main() { println!("check_nested_virt={}", krun::check_nested_virt()); }
```

Note the names: the **package** is `libkrun`, the **lib** is `krun`
(`[lib] name = "krun"`), so the dependency key is `libkrun` and the code says
`krun::`.

## Toolchain recipe (the whole environment)

```sh
source /nix/store/<...>-rust-bindgen-hook/nix-support/setup-hook
populateBindgenEnv   # exports LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS
export PKG_CONFIG_PATH=<virglrenderer-1.3.0>/lib/pkgconfig:<mesa-libgbm>/lib/pkgconfig
cargo build
```

`rustPlatform.bindgenHook` was fetched with `nix shell`; `nix shell` puts only its
wrapper on `PATH`, so the env vars have to come from sourcing its
`nix-support/setup-hook` (the same trick the real derivation gets for free by
putting `bindgenHook` in `nativeBuildInputs`). `virglrenderer` and `libgbm` come
from the shell too; their `.pc` files live in the single `out` output (there is
no `.dev` output to point at).

## Result

```
Compiling bindgen v0.72.1
Compiling rutabaga_gfx v0.1.85
Compiling krun-arch v0.1.0-2.0.0-dev (/home/dev/cang/cang/deps/libkrun/src/arch)
Compiling krun-devices v0.1.0-2.0.0-dev (/home/dev/cang/cang/deps/libkrun/src/devices)
Compiling rustapi-probe v0.1.0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 41.38s
```

Zero warnings, one linked 53 MB debug binary; raw log in
`02-raw-probe-build.log`. Failures before the recipe was complete, for the
record: `cargo fetch` needs the lock copied in, and the dependency key must be
`libkrun` (not `krun`).

## What it does and does not prove

- **Proves**: libkrun's Rust API compiles as a path dependency of a foreign crate
  with the full GPU/input/timesync feature set, from `deps/libkrun`, using only
  packages nixpkgs already has (`clang`/libclang, `pkg-config`, `virglrenderer`
  1.3.0 - which satisfies `rutabaga_gfx`'s `atleast_version("1.3.0")` probe -
  and `mesa-libgbm`). No fork change is needed to *build* it.
- **Does not prove**: a real `VmmBuilder`/`GpuDevice` graph, the init blob (it
  needs `KRUN_INIT_BINARY_PATH`; the devshell rustc has no musl std, though a
  musl rustc 1.95.0 is in the store and `nix/dev/flake.nix` already builds
  `krun-init` with `pkgs.pkgsStatic`), vendoring inside a Nix *derivation*, or
  anything about running a guest.
