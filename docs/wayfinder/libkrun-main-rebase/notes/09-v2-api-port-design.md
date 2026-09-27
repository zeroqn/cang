# 09 — Scoping the port of cang's launcher to libkrun's v2 API

Wayfinder ticket `09-port-cang-to-v2-api`, map
`docs/wayfinder/libkrun-main-rebase`. Session 2026-09-27: the mapping gathered
before implementation, so the port itself can be mechanical.

## The v2 surface (main `a980e779`)

`include/libkrun.h` declares **111 `krun_*` functions**, object-oriented, plus
the separate `include/libkrun_init.h` with **31 `krun_init_*` functions**. The
groups cang needs:

| group | functions |
|---|---|
| vmm (23) | `krun_vmm_builder_new`, `_vcpus`, `_ram_mib`, `_payload`, `_devices`, `_nested_virt`, `_acpi`, `_split_irqchip`, `_shutdown_support`, `_add_serial_console`, `_set_kernel_console`, `_add_smbios_oem_string`, `_build`, `_destroy`; `krun_vmm_handle`, `_destroy`, `_pause`, `_resume`, `_shutdown`; `krun_vmm_run`, `krun_vmm_destroy`, `krun_vmm_error_code`, `krun_vmm_error_message` |
| payload (8) | `krun_payload_load_krunfw`, `_load_external`, `_load_firmware`, `_nitro_enclave`, `_append_cmdline`, `_cmdline`, `_destroy` |
| fs (10) | `krun_fs_device_new`, `_new_read_only`, `_new_null`, `_set_overlay`, `_set_dax_window_size`, `_destroy`; `krun_fs_overlay_*` |
| console (7) | `krun_console_device_builder`, `krun_console_builder_add_tty_port`, `_add_inout_port`, `_add_default_console`, `_build`, `_destroy`, `krun_console_device_destroy` |
| block (5) | `krun_block_device_new`, `_set_read_only`, `_set_sync_mode`, `_set_direct_io`, `_destroy` |
| net (6) | `krun_net_device_new_unixstream_fd`/`_path`, `_new_unixgram_fd`/`_path`, `_new_tap`, `_destroy` |
| vsock (4) | `krun_vsock_device_new`, `_add_unix_port`, `_add_port_forward`, `_destroy` |
| gpu (3) | `krun_gpu_device_new`, `_shm_size`, `_destroy` |
| input (3), balloon (2), rng (2) | `krun_input_device_new{,_from_fd}`, `_destroy`; `krun_balloon_device_new`, `_destroy`; `krun_rng_device_new`, `_destroy` |
| mmio (3) | `krun_mmio_device_manager_new`, `_add`, `_destroy` |
| helpers | `krun_init_log` (now `KrunResult (..., KrunError*)`), `krun_check_nested_virt` (**bool**), `krun_result_name{,_cstr}`, `krun_error_code`/`_message`/`_destroy`/`_result`, `krun_str_free`, `krun_free_object_array` |

`KrunInitConfig` comes from `libkrun_init.so`: `krun_init_config_builder`,
`krun_init_builder_{workdir,env,env_var,arg,args,mount,rlimit,rlimits,dhcp,set_root_disk_remount,from_oci_json}`, `krun_init_builder_build`,
`krun_init_config_apply_in` (the one cang must call), plus its own result/error/
string helpers.

## cang's current sequence, mapped

From `crates/cang/src/runtime/vm/libkrun/launcher.rs` (the only caller of the
`LibkrunApi` trait):

| cang today (v1) | v2 counterpart |
|---|---|
| `init_log(level)` | `krun_init_log(...)` (signature changed) |
| `create_ctx` / `free_ctx` | `krun_vmm_builder_new` / `krun_vmm_builder_destroy` |
| `set_vm_config(vcpus, ram_mib)` | `krun_vmm_builder_vcpus` + `_ram_mib` |
| `set_nested_virt` / `check_nested_virt` | `krun_vmm_builder_nested_virt` / `krun_check_nested_virt` (bool) |
| `set_root(task_rootfs)` | fs/root handling in the init config (`krun_fs_device_new`, `krun_init_builder_set_root_disk_remount`) - needs care: this is the one call with no obvious 1:1 |
| `add_disk(id, path, ro)` | `krun_block_device_new` (+ `set_read_only`) |
| `add_net_unixstream(fd, flags)` | `krun_net_device_new_unixstream_fd` |
| `add_vsock_port2(port, path, listen)` | `krun_vsock_device_new` + `add_unix_port` |
| `set_port_map(entries)` | `krun_vsock_device_add_port_forward` |
| `disable_implicit_console` / `add_virtio_console_default(0,1,2)` / `set_console_output(path)` | `krun_console_device_builder` (+ `add_default_console`) and `krun_vmm_builder_set_kernel_console` for the kernel console |
| `set_workdir`, `set_exec(path, argv, env)`, `set_rlimits` | the init config: `krun_init_builder_workdir`, `_args`/`_arg`, `_env`/`_env_var`, `_rlimits`, then `krun_init_config_apply_in(vmm_handle, overlay, payload)` |
| `set_kernel_cmdline_append(fragment)` | `krun_payload_append_cmdline` |
| `start_enter(ctx)` | `krun_vmm_builder_build` -> `krun_vmm_run` (handle for `pause`/`shutdown`) |
| **fork-only** `set_gpu_options3(flags, shm, render_fd)` | `krun_gpu_device_new` + `_shm_size` cover two thirds; the render-server fd has no main equivalent - ticket 10 re-adds it |
| **fork-only** `set_profile_path(path)` | no main equivalent - ticket 10 |

## Work breakdown

1. `dynamic.rs`: bind the v2 symbols cang needs (about 40 of the 111) plus the
   `libkrun_init.so` set; loader names gain `libkrun.so.2` and `libkrun_init.so`;
   adopt the `(KrunResult, KrunError*)` convention (non-zero result + error
   object -> `anyhow` context).
2. `api.rs`: reshape the `LibkrunApi` trait around objects (builder/config/
   device handles) rather than ctx ids; keep it testable through the existing
   replay fixtures.
3. `launcher.rs`: build payload -> devices -> init config -> vmm; the ordering
   constraints (payload before builder, init applied before `run`) are the risky
   part and want a live boot per step.
4. `tests.rs`: update the 1270-line fixture suite to the new call shape without
   weakening what it asserts.
5. Decide and record **which init runs** (see the ticket): the default
   `libkrun_init.so` blob (built with `timesync` in the dev build) or cang's own.
6. Live boot: `cang --mem 4 --seccomp=off --landlock=off -- <cmd>` against the
   local `nix/dev` build (green as of ticket 12).

## Open question for the port

`set_root(task_rootfs)` has no 1:1 v2 call: main models the guest root as
firmware/fs/init-config concerns rather than a single host path. Deciding how
cang's task rootfs (a directory cang controls) maps onto fs devices + the init
config's root handling is the first thing to settle in the implementation
session.
