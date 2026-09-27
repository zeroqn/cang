---
label: wayfinder:task
title: Port cang's launcher to libkrun's v2 API
status: closed
blocked_by: ["04-rebase-cang-onto-main"]
claimed_by: pi session (2026-09-27)
---

## Scoping

`../notes/09-v2-api-port-design.md` has the v2 surface (111 `krun_*` functions
plus 31 `krun_init_*`), the call-by-call mapping of `launcher.rs`'s current
sequence, the work breakdown, and the one call with no 1:1 counterpart
(`set_root`).

## Question

Rewrite cang's libkrun integration against the ABI-2 builder/object API, so
`DynamicLibkrunApi::open()` succeeds and a guest boots. Route decided by bob
(2026-09-27): **port cang, do not add a fork-side v1 shim.**

Scope, from `notes/01-upstream-main-delta.md`:

1. `crates/cang/src/runtime/vm/libkrun/dynamic.rs` - rebind to the v2 entry
   points (`krun_vmm_builder_new/_vcpus/_ram_mib/_payload/_devices/_nested_virt/_build`,
   `krun_fs_device_new`, `krun_block_device_new`, `krun_net_device_new_unixstream_fd`,
   `krun_vsock_device_new` + `add_unix_port`/`add_port_forward`, `krun_console_*`,
   `krun_gpu_device_new`, `krun_payload_append_cmdline`, `krun_vmm_run`); add
   `libkrun.so.2` to `DEFAULT_LIBKRUN_NAMES`; drop the unconditional
   `krun_set_log_level` resolution that currently aborts `open()`; adopt the
   `KrunResult` + `KrunError*` error model.
2. **Init injection** - call `krun_init_config_apply_in(handle, overlay, payload)`
   from `libkrun_init.so` (main removed implicit `init=/init.krun`), which is what
   now supplies exec/env/workdir/rlimits. Bind the new library and make its
   absence a clear, early error.
3. `crates/cang/src/runtime/vm/libkrun/api.rs` - the `LibkrunApi` trait reshaped
   to the object model (device construction replaces the flat `krun_set_*`
   calls): decide per method whether it stays, splits, or disappears.
4. `launcher.rs` - build the payload/devices/init graph in the order the v2 API
   requires; `port_map` becomes vsock port-forward entries.
5. `tests.rs` - the 1270-line replay fixtures are the behaviour oracle: update
   them to the new call shape without weakening what they assert.
6. Anything `--gpu=drm`/`--wayland` needs from cang's side to call the fork's
   re-added GPU entry point (ticket 10) - coordinate, do not duplicate.

Decide and record **which init runs**: under ABI 2 the guest init is the
separate `libkrun_init.so`, and the prebuilt must ship it (ticket 07). If cang
keeps libkrun's default init blob, that blob is built with `--features ffi`
plus `timesync` when the workflow passes `TIMESYNC=1` (upstream PR 840's guest
half, now carried). If cang supplies its own init instead, it has to implement
the time-sync request itself - `cang-guest-init` does not do that today.

Evidence to keep: the diff, the fixture-suite output, and a live boot of a
trivial guest command (`cang --mem 4 --seccomp=off --landlock=off -- ...` under
`script`, per `cang-local-validation-gates`).

## Deliverable

cang builds and boots on the rebased fork: `cargo test`, `nix build .#cang
.#cang-musl`, fmt/clippy/deny clean, plus the recorded live boot. Report anything
that genuinely has no v2 equivalent (as a finding, not as a shim).


## Resolution (2026-09-28, pi session)

Ported, tested and live-booted. `notes/09-v2-api-port.md` has the call-by-call
mapping, the ordering constraints the ABI imposed on the launcher, and the
findings handed to ticket 10.

**Code.** `api.rs`'s `LibkrunApi` is now the ABI-2 object model
(`Handle = usize`; the `KrunX**` builder methods return the possibly-reboxed
handle). `dynamic.rs` loads `libkrun.so.2` **and** `libkrun_init.so`
(`libkrun_init` absence is a named early error), binds the reduced v2 symbol set,
adopts the `(KrunResult, KrunError*)` model with a `KrunPushStr` vtable that
formats the error object, drops the unconditional `krun_set_log_level`, and binds
the two ticket-10 fork symbols as optional. `launcher.rs` builds
overlay → payload → init config → devices → vmm and runs;
`krun_vmm_builder_build` + `krun_vmm_run` replace `krun_start_enter`.
`publish.rs` emits ABI 2's `guest:host` port-forward order.

**Decisions.** libkrun's own `libkrun_init.so` blob is the init (cang supplies
only the config, so `krun_init_config_apply_in` is what supplies exec/env/workdir/
rlimits, and the entrypoint argv is `[exec_path, ...argv]`). DHCP moved from the
net-device flag (ignored by ABI 2) to `krun_init_builder_dhcp`. The managed
kernel console is the leading console device (hvc0 → the console-log file) with
the default console as hvc1. `--gpu=drm` passes a headless display backend and
fails loudly until the fork re-adds the render-server fd setter (ticket 10).

**Verified.** `cargo test --workspace` green (597 `cang` lib tests, including the
rewritten v2 call-shape fixtures), `cargo clippy --all-targets --all-features --
-D warnings` clean, `cargo deny check` clean, `cargo fmt --check` clean. Live
boot on a btrfs-backed graphroot: the guest ran `bash -lc`, printed
`6.12.109-hardened1` for `uname -r`, saw `/dev/hvc*` and `/dev/vsock`, and came up
as `uid=1000(dev) gid=993(dev) groups=…,video,render`.

**Blocker found (own ticket):** the live boot ran against a **locally built**
libkrun, because the published `v2.0.0-cang.1` `libkrun.so.2.0.0` exports **zero**
`krun_*` symbols - the fork's release workflow builds without `FFI=1` and only
checks that the files exist. `nix/dev`'s local build had the same gap and is
fixed here; the release-side fix and re-pin are
[Rebuild the v2 release with the C ABI and re-pin](13-release-c-abi-repin.md).
