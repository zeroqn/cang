---
label: wayfinder:research
title: Upstream-main delta for cang's libkrun integration
status: closed
blocked_by: []
claimed_by: pi research child-1 (2026-09-27)
---

## Question

Rebasing `cang` from the stable-1.19.x tip (`fb988873`, v1.19.5) onto upstream
`main` (`a980e779`, `FULL_VERSION=2.0.0`) crosses ~369 commits. What does that
change *on the surface cang actually touches*?

Enumerate, with file:line evidence from the rebased tree:

1. **The C ABI cang binds.** cang loads libkrun dynamically and resolves the
   symbols declared in `crates/cang/src/runtime/vm/libkrun/api.rs` /
   `dynamic.rs`: `krun_create_ctx`, `krun_free_ctx`, `krun_init_log`,
   `krun_set_log_level`, `krun_set_vm_config`, `krun_set_gpu_options3`,
   `krun_check_nested_virt`, `krun_set_nested_virt`, `krun_set_root`,
   `krun_add_disk`, `krun_disable_implicit_console`, `krun_set_console_output`,
   `krun_add_virtio_console_default`, `krun_add_net_unixstream`,
   `krun_add_vsock_port2`, `krun_set_port_map`, `krun_set_workdir`,
   `krun_set_exec`, `krun_set_rlimits`, `krun_set_profile_path`,
   `krun_set_kernel_cmdline_append`, `krun_start_enter`. Which of these changed
   signature, semantics, or disappeared between 1.19.5 and main? Which new
   `krun_*` entry points exist that cang might now want (not to wire - just to
   record)?
2. **Soname / install layout.** cang's loader probes `libkrun.so.1` then
   `libkrun.so` (`DEFAULT_LIBKRUN_NAMES`). `main`'s Makefile declares
   `FULL_VERSION=2.0.0`; what soname, symlinks and `LIBDIR_Linux` does main
   actually install, and what does the fork's existing asset (`libkrun-<arch>-linux-full.tgz`)
   end up containing? Does `nix/pkgs/libkrun.nix` still find what it expects?
3. **Build shape.** Which cargo features does the fork's CI and the in-tree
   `nix/dev` build enable (`gpu`, `init-blob`, `net`, `blk`, `input`, `virgl_resource_map2`,
   `krun_display`), and did main add/rename/remove any of them?
4. **init/kernel protocol.** What does main expect of its kernel side
   (`libkrun_init.so`, kernel cmdline, virtio feature advertisement) compared
   with 1.19.5 - i.e. the input to ticket 05?

## Deliverable

`notes/01-upstream-main-delta.md`: the delta as a table (symbol -> unchanged /
changed / gone / new, with the main-side definition), the soname/asset/layout
facts, the feature-flag delta, and a concrete list of cang-side edits the pin
will need. Raw command output kept in the same directory.

## Resolution

**Resolved by a tool-capable research child (2026-09-27).** Deliverable:
`../notes/01-upstream-main-delta.md` (31 KB) plus six raw evidence files in the
same directory (`01-raw-commit-log.txt`, `01-raw-main-api-removal.txt`,
`01-raw-main-ffi-exports.txt`, `01-raw-fork-asset-elf.txt`,
`01-raw-soname-makefile-cargo.txt`, `01-raw-init-kernel-protocol.txt`).
Independently spot-verified by the charting session.

**The headline changes the map: main is not a superset, it is a rewrite.**
`a3d31822` *"lib: remove old C API"* (2026-09-11, 23 files, -6691/+276) deletes
the old `krun_*` surface and the `krun-sys` bindgen crate; `911aa81b` replaces
`include/libkrun.h` with a generated ABI-2 header. Verified in the tree:
`krun_set_log_level`, `krun_create_ctx`, `krun_set_gpu_options3` and
`krun_set_profile_path` have **zero** occurrences in main's `src/libkrun/src/lib.rs`
and `include/libkrun.h`.

1. **ABI:** of cang's 22 bound symbols, **20 are gone** (no stubs survive; the
   brief `-ENOTSUP` stubs from `502116e7`/`4d2201e7` died in `a3d31822`) and
   **2 changed**: `KrunResult krun_init_log(int, u32, u32, u32, KrunError*)` and
   `bool krun_check_nested_virt(void)` (`include/libkrun.h:686,688`). The
   replacement is the v2 builder/object API (`krun_vmm_builder_new` +
   `..._vcpus/_ram_mib/_payload/_devices/_nested_virt/_build`, `krun_fs_device_new`,
   `krun_block_device_new`, `krun_net_device_new_unixstream_fd`,
   `krun_vsock_device_new` + `add_unix_port`, `krun_console_*`, `krun_gpu_device_new`,
   `krun_payload_append_cmdline`, `krun_vmm_run`); `krun_set_port_map` becomes
   `krun_vsock_device_add_port_forward`; the error model is
   `KrunResult` + `KrunError*` out-param instead of negative errno.
   cang currently fails at `DynamicLibkrunApi::open()` (`dynamic.rs:143-145`
   resolves `krun_set_log_level` unconditionally).
2. **Soname/layout:** `libkrun.so.2.0.0`, SONAME `libkrun.so.2`, symlinks `.2`
   and `.so`; `LIBDIR_Linux` still `lib64`, so `nix/pkgs/libkrun.nix`'s
   `lib64`->`lib` + `include` copy and `libkrun.so.*` fixup still fit. New:
   `libkrun_init.so.0.1.0`, `libkrun_init.pc`, `include/libkrun_init.h` - and
   `nix/pkgs/cang-{prebuilt,rust}.nix` glob `libkrun.so*`, so `libkrun_init.so*`
   would **not** be shipped today. cang's `DEFAULT_LIBKRUN_NAMES` lacks `.so.2`.
3. **Features:** the fork's CI and `nix/dev` use `BLK NET GPU SND INPUT`; main
   removed `snd` (`7e5c6c4b`; `SND=1` is now a silent no-op) and `efi`, made the
   init blob a **separate `krun-init-blob` crate built with `--features ffi`**
   (no longer a default feature), and added `vhost-user`, `timesync`, `ffi`.
4. **Kernel/firmware (input to ticket 05):** the fw ABI is **unchanged** -
   `krunfw_get_kernel(u64*, u64*, usize*) -> *mut c_char`, soname `libkrunfw.so.5`,
   `KernelBundle` and `DEFAULT_KERNEL_CMDLINE` byte-identical, virtio-mmio + MP
   tables still default (ACPI opt-in), no new `ACCESS_PLATFORM`/packed-ring
   requirement - so the pinned `v5.6.2-cang.1` is ABI-compatible. **But** main
   removed implicit init injection (`502116e7`): 1.19.5 appended `init=/init.krun`
   itself, main requires the caller to call
   `krun_init_config_apply_in(libkrun_handle, overlay, payload)` from
   `libkrun_init.so`, and exec/env/workdir/rlimits now only exist through that
   `Config`. cang boots its workload via `krun_set_exec`, so without this it has
   **no PID 1**. A live boot remains the only way to confirm the pairing.
5. **Also structural:** main has no in-tree `rutabaga_gfx` (crates.io `0.1.85`),
   so the fork's patches to it have no path; `src/vmm` now lives inside
   `src/libkrun/src/vmm`; `src/polly` and `src/whp` are new.

The resulting cang-side edit list (the map's tickets 09/10/07) covers
`api.rs` ABI rewrite, `dynamic.rs` rebinding + `libkrun.so.2` + binding
`libkrun_init`, `launcher.rs` restructure onto payload/devices/init, `tests.rs`
fixture replay, re-adding the fork's render-server-fd GPU entry point on main's
`GpuDevice`, and the nix/docs/test follow-through.
