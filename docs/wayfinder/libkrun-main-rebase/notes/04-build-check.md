# 04 — Build check on the rebased branch

Session 2026-09-27. `nix build ./nix/dev#cang-dev` against the rebased submodule
worktree (`cang-main-rebase`).

## What ran

| step | result |
|---|---|
| vendoring main's deps (`fetchCargoVendor`, hash `sha256-SjThWtfmffo38w3ormnO+hSa4H6IugRz2wq4DvWX5Jg=`) | ok |
| `libkrunfw` 5.6.2-cang.1 (kernel 6.12.109) from local source | ok |
| `libkrun` (main's tree, our fork flags) | **failed** in `krun-init-blob`'s build script |
| `cang` | not reached |

## The failure

```
error: failed to run custom build command for `krun-init-blob v0.1.0-2.0.0-dev (/build/libkrun/init/init-blob)`
  thread 'main' panicked at init/init-blob/build.rs:97:13:
  musl target not available for krun-init; Run `rustup target add $(uname -m)-unknown-linux-musl` to obtain.
make: *** [Makefile:218: target/release/libkrun.so.2.0.0] Error 101
```

Upstream `main` builds the guest init as a **musl** binary
(`init/init-binary`), and `init/init-blob/build.rs` refuses to continue unless
the musl target's std is installed. Our `nix/dev` override builds libkrun with
the **host** rust toolchain, whose sysroot has only
`x86_64-unknown-linux-gnu` (verified: `ls $(rustc --print sysroot)/lib/rustlib/`
in the devshell lists no musl std).

This is a nix-side gap, not a rebase defect: nixpkgs' own `libkrun` package is
written for 1.17.4, which predates the separate init blob, so it has no precedent
for this. The **release path is unaffected** - the fork's
`.github/actions/setup-build-env/action.yml` already runs
`rustup target add "$(uname -m)-unknown-linux-musl"` on the runners.

## Therefore

- Ticket 04's rebase content (main + 7 replayed fork commits + 7 PR commits +
  1 adaptation commit) is **not** yet compile-verified.
- A local-source build path that can produce both `libkrun.so.2` and a musl
  `libkrun_init.so` is needed for tickets 09/10 (and for their live-boot
  evidence); the devshell rust cannot supply it alone, while `pkgsStatic`'s rust
  (used by `cang-musl`) can.
- Options considered: (a) split the build - host libkrun by `make` plus a
  `pkgsStatic`-built `krun-init-blob` installed alongside; (b) build the host
  library only and accept compile-only local verification, leaving all live
  booting to the published prebuilt; (c) keep iterating without a nix dev-build
  path (manual cargo/make in the devshell).

## Outcome (2026-09-27): green

Bob chose the split build (ticket 12). `init/init-blob/build.rs` honours
`KRUN_INIT_BINARY_PATH`, so `nix/dev` now builds `init/init-binary` for musl with
`pkgsStatic`'s rust (with the `timesync` feature) and points the ordinary
Makefile flow at it; the blob's build script embeds that binary instead of
cross-building it. With `ffier`'s rustfmt dependency and main's real feature set
(`withTimesync`, no `withSound`) also fixed:

```
nix build ./nix/dev#cang-dev   ->  exit 0
/nix/store/...-libkrun-2.0.0-cang/lib64 -> lib/
  libkrun.so -> libkrun.so.2 -> libkrun.so.2.0.0
  libkrun_init.so -> libkrun_init.so.0 -> libkrun_init.so.0.1.0
dev: include/libkrun{,_display,_init,_input}.h, lib/pkgconfig/libkrun{,_init}.pc
cang lib/cang -> libkrun.so.2* and libkrunfw.so.5* from the same local build
```

That proves the rebased tree compiles and links. It does **not** prove cang can
boot: cang still binds the v1 C ABI (`krun_set_log_level` and friends) at
runtime, which is ticket 09's port.
