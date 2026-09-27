# 09 — Porting cang's launcher to libkrun's ABI-2 object API

Ticket `09-port-cang-to-v2-api`, map `docs/wayfinder/libkrun-main-rebase`.
Implementation session 2026-09-27/28, against `deps/libkrun` tip `d578e4e2`
(fork `v2.0.0-cang.1`, upstream `main` + PRs 865/840).

## What changed

- `crates/cang/src/runtime/vm/libkrun/api.rs`: `LibkrunApi` reshaped from the
  flat v1 `krun_set_*`/ctx-id surface to the ABI-2 object model. Handles are an
  opaque `usize`. Methods whose C entry point takes `KrunX**` (libkrun's
  "consume self, return self" builder pattern) **return the handle** the library
  left behind: `krun_vmm_builder_*` may rebox the object, so the caller has to
  follow it.
- `crates/cang/src/runtime/vm/libkrun/dynamic.rs`: two `dlopen` handles
  (`libkrun.so.2` and `libkrun_init.so.0`), the ~40 v2 symbols cang needs, the
  `(KrunResult, KrunError*)` error model with a `KrunPushStr` vtable that formats
  the error object into an `anyhow` error, the no-op headless display backend,
  and the two optional fork symbols. The unconditional `krun_set_log_level`
  resolution is gone.
- `crates/cang/src/runtime/vm/libkrun/launcher.rs`: the v2 call graph, its
  ordering constraints, and the console/init/device mapping below.
- `crates/cang/src/runtime/vm/libkrun/tests.rs`: the recording fake rewritten to
  the v2 call shape. Every v1 assertion has a v2 counterpart; the handful that
  have no ABI-2 equivalent are listed under *Tests replaced*.
- `crates/cang/src/runtime/publish.rs`: `tsi_port_map` became
  `tsi_port_forwards`, emitting ABI 2's `guest:host` order.

## v1 → v2 mapping (implemented)

| v1 | v2 |
|---|---|
| `krun_init_log(fd, level, style, opts)` | same signature, now returning `(KrunResult, KrunError*)` |
| `krun_create_ctx` / `krun_free_ctx` | `krun_vmm_builder_new` / `krun_vmm_builder_destroy` (only before `build`) |
| `krun_set_vm_config(vcpus, ram)` | `krun_vmm_builder_vcpus` + `_ram_mib` |
| `krun_check_nested_virt` → int | `krun_check_nested_virt` → `bool` |
| `krun_set_nested_virt(true)` | `krun_vmm_builder_nested_virt(true)` |
| `krun_set_gpu_options3(flags, shm, fd)` | `krun_gpu_device_new(flags, backend)` + `krun_gpu_device_shm_size` + the fork's `krun_gpu_device_set_render_server_fd` (**ticket 10**) |
| `krun_set_root(path)` | `krun_fs_device_new("/dev/root", path)` + `krun_fs_device_set_overlay` |
| `krun_add_disk(id, path, ro)` | `krun_block_device_new(id, path, RAW)` + `set_read_only` |
| `krun_add_net_unixstream(fd, mac, features, DHCP)` | `krun_net_device_new_unixstream_fd("net0", fd, mac, features, 0)` + `krun_init_builder_dhcp(true)` |
| `krun_add_vsock_port2(port, path, listen)` | `krun_vsock_device_new(cid, tsi)` + `add_unix_port` |
| `krun_set_port_map([host:guest])` | `krun_vsock_device_add_port_forward("guest:host")` |
| `krun_disable_implicit_console` / `krun_set_console_output` | no equivalent (there is no implicit console) - see *Console* |
| `krun_add_virtio_console_default(0,1,2)` | `krun_console_device_builder` + `add_default_console(0,1,2)` + `build` |
| `krun_set_workdir` / `set_exec` / `set_rlimits` | the init config: `krun_init_builder_{args,env,workdir,rlimits}` |
| `krun_set_kernel_cmdline_append(fragment)` | `krun_payload_append_cmdline(fragment)` |
| `krun_start_enter(ctx)` | `krun_vmm_builder_build` then `krun_vmm_run` (never returns alive) |
| (implicit init) | `krun_init_config_builder` → `krun_init_config_apply_in(config, libkrun_handle, overlay, payload)` |
| fork `krun_set_profile_path` | the fork's `krun_vmm_builder_set_profile_path` (**ticket 10**, optional today) |

## Ordering constraints found while porting

1. `krun_fs_device_set_overlay` **moves** the overlay into the device, so
   `krun_init_config_apply_in` (which adds the init blob and its config as
   overlay files) must run before it. The rootfs device is therefore created
   after the init config, unlike v1's "root first" order.
2. `krun_mmio_device_manager_add` **takes ownership** of the device, so a device
   must be fully configured before it is added. The vsock device's ports and
   forwards are registered before the add (the v1 code added vsock ports after
   the equivalent of the add).
3. `krun_vmm_builder_*` and `krun_init_builder_*` take a pointer to the handle
   and may rebox; each call's returned handle must feed the next one. The fake
   returns `builder + 1` from every builder call and the sequence fixture asserts
   the chain, so a launcher that ignores the new handle fails the test.
4. `krun_vmm_builder_build` consumes the builder and `krun_vmm_run` consumes the
   VMM, so "free the context on setup failure" only applies before the build.
5. The kernel console is `hvc0` = the *first* console device, so a managed launch
   adds the kernel-console device before the default console device.
6. The `krun_init_config_apply_in` config object must stay alive for the VM's
   lifetime; cang never destroys it (the helper process `_exit`s from the VMM).

## Decisions

- **Which init runs: libkrun's own blob.** ABI 2 removed implicit init
  injection; `libkrun_init.so` supplies the blob, and cang only supplies the
  config (argv, env, workdir, rlimits, DHCP). cang's `cang-guest-init` stays what
  it always was - the *entrypoint* the blob execs, not PID 1. The prebuilt
  workflow builds the blob with `timesync` (PR 840's guest half), so cang does
  not need to implement the time-sync request itself.
- **Guest environment.** The v1 flow ran the C init with `KRUN_CONFIG=/
  .cang_config.json` and let *it* apply that file's `Env` to the entrypoint. ABI
  2's init config owns the process environment, so cang passes
  `config.env + config.guest_config_env` to `krun_init_builder_env`; the argv it
  passes is `[exec_path, ...argv]` because the blob execs `argv[0]`. cang still
  writes `/.cang_config.json` (unchanged).
- **DHCP.** ABI 2's `krun_net_device_new_unixstream_fd` ignores its `flags`
  argument entirely, so v1's `NET_FLAG_DHCP_CLIENT` and its `EINVAL` retry are
  gone; passt mode sets `krun_init_builder_dhcp(true)` instead.
- **Console.** `krun_disable_implicit_console`/`krun_set_console_output` have no
  ABI-2 counterpart because there is no implicit console. A managed launch
  reproduces the v1 fork's *two* console devices: the leading one is hvc0 with
  port 0's output bound to the opened `guest-kernel-console.log` (so the kernel's
  OOM/panic account still lands in its own file), the second is the default
  console (worker stdio as `krun-stdin`/`krun-stdout`/`krun-stderr` ports). A
  normal launch builds only the default console, which is hvc0.
- **TSI publish.** `krun_vsock_device_add_port_forward` is `guest:host`, the
  reverse of the v1 port-map entry, so the translation lives in
  `publish::tsi_port_forwards`.
- **Vsock.** One device, cid 3, `HIJACK_INET` in TSI mode and no TSI features in
  passt mode (v1's implicit configuration). It is created when the mode or a
  host channel needs it.
- **GPU device.** `krun_gpu_device_new` requires a display-backend handle, and
  libkrun rejects a backend that does not advertise `BASIC_FRAMEBUFFER` with all
  four methods. cang renders headless, so it passes a backend that advertises the
  feature and refuses every scanout request (`KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED`)
  rather than a null handle. The *render-server fd* has no ABI-2 entry point at
  all; `dynamic.rs` binds the fork's `krun_gpu_device_set_render_server_fd` as an
  optional symbol and fails with a clear message when it is absent, so
  `--gpu=drm`/`--wayland` fail loudly until ticket 10 instead of silently
  launching without venus.
- **Symbol strictness.** Every symbol other than the two ticket-10 extensions is
  required, so a libkrun without them fails at `open()` with a named symbol
  rather than at launch. `krun_check_nested_virt` is no longer optional (it is
  part of ABI 2), and the launcher still treats a `false` or failing check as a
  non-fatal diagnostic.

## Tests replaced (documented, not weakened)

- `rlimit_setup_failure_*`: ABI 2's rlimits call is a `void` builder method with
  no failure path. The rlimit entry's value is still asserted (reaching
  `krun_init_builder_rlimits`), and the failure-classification/cleanup discipline
  is covered by the fs-device, port-forward and nested-virt cases.
- `set_console_output(managed log)`: replaced by "the managed launch builds a
  leading console device whose port-0 output is an open fd and whose log file
  exists", which is strictly more than the v1 fixture checked (it recorded a path
  that libkrun's Linux path never actually used).
- passt-mode DHCP-flag retry: replaced by "passt mode asks the init config for
  DHCP, TSI mode does not".
- `nested_virt_symbol_presence_for_test`: the optional-check/required-set split
  does not exist in ABI 2, so it became the generic
  `required_symbol_presence_for_test`.

## Findings handed to ticket 10

- `krun_gpu_device_set_render_server_fd(KrunGpuDevice*, int, KrunError*)` must be
  re-added by the fork (cang already calls it when present).
- `krun_vmm_builder_set_profile_path(KrunVmmBuilder*, KrunStr, KrunError*)` must
  be re-added (cang treats it as optional, exactly as v1's optional
  `krun_set_profile_path` was).
- The profiling phase labels the v1 fork emitted (`libkrun_start_enter_*`) are
  gone with `krun_start_enter`; `crates/cang/src/runtime/session/profile.rs`'s
  fixtures are synthetic, but the fork's phase names will change.

## Live boot (2026-09-28)

```
$ cang --mem 4 --seccomp=off --landlock=off -- bash -lc '...'
LIVE_V2_OK
6.12.109-hardened1
/dev/hvc0 ... /dev/hvc7  /dev/vsock
uid=1000(dev) gid=993(dev) groups=993(dev),44(video),107(render)
exit 0
```

Host-side shape: `tools/chromium-cang-smoke`'s recipe - a btrfs loop image
(`/home/dev/cang/disk`), a fresh btrfs graphroot with `localhost/cang:latest`
loaded into it, `CONTAINERS_STORAGE_CONF`/`XDG_CONFIG_HOME`/`XDG_STATE_HOME`
pointed at that disk, `script -q -e -c … /dev/null`, `btrfs-snapshot` task-rootfs.

**The boot used a locally built libkrun, not the pinned v2.0.0-cang.1 - see the
next section.** The kernel (`uname -r` 6.12.109-hardened1), the venus-free
console/identity path, `/dev/hvc*` and `/dev/vsock` all came up, so
`krun_init_config_apply_in` (the injected init blob), the virtiofs root with the
overlay, the block devices, the non-TSI/TSI vsock device and the console device
graph are all wired correctly.

## Finding: the published v2.0.0-cang.1 has no C ABI

The port could not run against the pin. `cang` failed at

```
failed to resolve libkrun symbol krun_init_log:
.../lib/cang/libkrun.so.2: undefined symbol: krun_init_log
```

Root cause: ABI 2 put the C entry points behind the `ffi` cargo feature, and the
fork's `Makefile` only enables it for `FFI=1`. The fork's release workflow
(`.github/workflows/publish-cang-release.yml`) builds with

```
make BLK=1 NET=1 GPU=1 INPUT=1 TIMESYNC=1
```

- no `FFI=1` - and only asserts that `libkrun.so*`/`libkrun_init.so*` *exist* in
the asset, not that they export the ABI. Measured on the shipped asset:

| library | exported `krun_*` symbols |
|---|---|
| `v2.0.0-cang.1` `libkrun.so.2.0.0` | **0** (only `bz_internal_error`) |
| `v2.0.0-cang.1` `libkrun_init.so.0.1.0` | 31 (`INIT_BLOB_FEATURE_FLAGS` always adds `ffi`) |
| locally built libkrun (FFI=1) | 99 |

So the release is unusable by any C consumer; the init blob was fine, which is
why ticket 07's `libkrun_init.so*` check passed. This also means ticket 12's
"green" `nix/dev` build was a file-level check, not an ABI-level one.

Fixes required (fork + publish side, not cang side):

1. `make … FFI=1` in `publish-cang-release.yml`, and an asset assertion that the
   produced `libkrun.so.2` exports `krun_init_log`/`krun_vmm_builder_new` (a
   `nm -D --defined-only | grep -c '^.* T krun_'` guard would have caught this).
2. A corrected release (`v2.0.0-cang.N+1`) with a fresh pin and submodule move.
3. `nix/dev`'s local libkrun build needs `FFI=1` too; it is fixed in this commit
   (`makeFlags = old.makeFlags ++ [ "FFI=1" ]`), which is what made the live boot
   above possible.
