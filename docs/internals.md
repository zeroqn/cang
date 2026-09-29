# Internals

Host-side lookup order, prepared-root grafting, and the guest entry contract.

## libkrun and host-tool lookup

cang binds libkrun through **libkrun's Rust API**, not its C ABI: the
`cang-libkrun` crate depends on the checkout at `deps/libkrun` by path (libkrun's
Rust API is not a stable ABI, so the rlib has to be built by cang's own rustc;
a Nix build gets that checkout from the `libkrun-src` input via
`nix/pkgs/workspace-src.nix`, an in-tree `cargo` build from the submodule)
and is linked into the binary, as is `krun-init-blob` - the guest init blob plus
its config builder, which installs the guest's PID 1. Nothing under the name
`libkrun.so` is loaded at runtime and there is no library override; only
`libkrunfw.so.5` (the firmware) is opened by soname, by libkrun itself, and the
Nix package places it in `$out/lib/cang` with an `$ORIGIN/../lib/cang` rpath on
the binary. Host tool lookup follows the same wrapper-free pattern: per-tool overrides
(`CANG_BUILDAH`, `CANG_BTRFS`, `CANG_MKFS_BTRFS`, `CANG_BLKID`,
`CANG_PASTA`, `CANG_PASST`) win first, then `CANG_HELPER_BINARY_DIR`, then
`$out/libexec/cang-helpers`, then `PATH` for source/debug runs. The helper
prepares a crun-style root export inside that same rootless namespace, and attaches that single prepared
root plus the writable persistent container-store disk. The prepared root is a
bind-mounted view of the task rootfs with the workspace, tool state, Cargo,
sccache, and host-prepared `/nix` overlay directories grafted into their final
guest paths before `krun_set_root`. Cang
intentionally does not register one `krun_add_virtiofs3` device per developer
path; keeping those binds inside the root export avoids the legacy x86
IRQ/device exhaustion that can otherwise occur before libkrun's implicit vsock
device is registered.

## Guest entry contract

`cang-guest-init enter` reads only `CANG_*` guest contract variables, validates
that the prepared-root paths already exist, ensures `/tmp` is a tmpfs with
`rw,exec,mode=1777`, verifies `/dev/net/tun` is the expected character device
`10:200`, makes it mode `0666`, probes it with `TUNSETIFF`, verifies the
host-prepared `/nix` overlay in host-overlay mode, prepares the selected
raw-disk container-store backend, exports the shell environment, and runs `fish -l` by
default. For deterministic smoke tests, `cang -- <command>` preserves the same
guest bootstrap path but replaces the final guest command with the explicit argv
after `--`.

## Validation history

Phase 4 completion was validated with targeted `cang` and `cang-guest-init`
unit tests plus a focused local-image libkrun smoke test. The smoke used a local
`localhost/cang:latest` image and verified Buildah-backed btrfs rootfs
materialization, persistent disk preparation, launch-config handoff, and a
successful libkrun guest-init entry. Full public-image publication and broader
guest-bootstrap hardening are follow-on work.
