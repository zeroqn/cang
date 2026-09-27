# 01 — Upstream-main delta for cang's libkrun integration

Wayfinder ticket `01-upstream-main-delta-for-cang`, map
`docs/wayfinder/libkrun-main-rebase`.  Resolved 2026-09-27, read-only on
`deps/libkrun` and on cang's source.

## Headline

**Upstream `main` (`a980e779`, `FULL_VERSION=2.0.0`, `ABI_VERSION=2`) is not a
superset of `stable-1.19.x` — it is a ground-up rewrite of the entire C ABI.**
Commit `a3d31822` *"lib: remove old C API"* deletes ~2800 lines of `krun_*`
`extern "C"` functions plus the `krun-sys` bindgen crate, and `911aa81b` *"lib:
add ffier bridge and generate bindings"* replaces `include/libkrun.h` with a
generated v2 header.

Of the **22 `krun_*` symbols cang binds**
(`crates/cang/src/runtime/vm/libkrun/api.rs`, bound in `dynamic.rs`):

* **20 are gone** — not present in `main`'s header, not in its export schema,
  not even as the `-ENOTSUP` stubs that commits `502116e7` / `4d2201e7` had
  briefly kept (those were deleted by `a3d31822`, which lands *after* them —
  see `01-raw-commit-log.txt` lines 228/229/283).
* **2 exist but changed signature**: `krun_init_log`, `krun_check_nested_virt`.

Concretely, `DynamicLibkrunApi::open()` fails immediately on `main`:
it resolves `krun_set_log_level` unconditionally at
`crates/cang/src/runtime/vm/libkrun/dynamic.rs:143-145`, which no longer exists.
So this ticket is not "re-pin and adjust a symbol or two" — the destination map
(`v2.0.0-cang.1` + cang green) requires **rewriting cang's libkrun launcher
against the v2 object/builder API**, and it requires the fork to **re-add its
own C-API extensions** (`krun_set_gpu_options3` render-server fd,
`krun_set_profile_path`, `krun_set_kernel_cmdline_append`) on top of `main`,
because `main` has no equivalent of the first two.

Evidence files in this directory:

| file | contents |
|---|---|
| `01-raw-commit-log.txt` | full `fb988873..a980e779` log, rev-parse of all relevant refs |
| `01-raw-main-api-removal.txt` | messages + stats of the API-removal commits, `main`'s `lib.rs` size |
| `01-raw-main-ffi-exports.txt` | every `ffi_name` `main`'s `libkrun.so.2` exports, header decls, per-cang-symbol hits |
| `01-raw-fork-asset-elf.txt` | the pinned v1.19.5-cang.1 asset: tar members, `libkrun.pc`, `readelf -d`, 69 exported `krun_*` dynsyms |
| `01-raw-soname-makefile-cargo.txt` | Makefile soname/`LIBDIR`/install + Cargo feature deltas |
| `01-raw-init-kernel-protocol.txt` | init/kernel/firmware protocol evidence for ticket 05 |

Refs used: fork base `fb988873` (`upstream/stable-1.19.x` tip, v1.19.5);
fork branch `cang` = `28e79624` (submodule pin `HEAD` = `237ceac0` = tag
`v1.19.5-cang.1`); `upstream/main` = `a980e779`.  `git rev-list --count
fb988873..a980e779` = **369**.

---

## (a) Symbol-by-symbol delta for cang's bound C ABI

Verdicts are against `a980e779`.  "gone" = absent from `main`'s generated
header (`include/libkrun.h`) *and* from the ffier export schema
(`bindings/libkrun-via-cdylib-weak/ffier-krun.json`, the authoritative export
list).  cang's assumed C signature is from
`crates/cang/src/runtime/vm/libkrun/dynamic.rs` (type aliases, lines 35-58).

| # | cang binding | cang's assumed signature | verdict on `main` | main-side definition |
|---|---|---|---|---|
| 1 | `krun_create_ctx` | `fn() -> i32` | **gone** | replaced by builder: `KrunVmmBuilder krun_vmm_builder_new()` `include/libkrun.h:378`; Rust `VmmBuilder::new` `src/libkrun/src/api/vmm_builder.rs:42` |
| 2 | `krun_free_ctx` | `fn(u32) -> i32` | **gone** | `void krun_vmm_destroy(KrunVmm)` `include/libkrun.h:443`; per-device `*_destroy()` |
| 3 | `krun_init_log` | `fn(i32,u32,u32,u32) -> i32` | **changed** | `KrunResult krun_init_log(int target, uint32_t level, uint32_t style, uint32_t options, KrunError* err_out)` `include/libkrun.h:686`; Rust `src/libkrun/src/api/logging.rs:85` (and `:133` for the other cfg) |
| 4 | `krun_set_log_level` | `fn(u32) -> i32` | **gone** | removed by `df19d2a0` *"lib: remove deprecated krun_set_log_level"*; logging now goes through `krun_init_log` + `KrunVmmBuilder` |
| 5 | `krun_set_vm_config` | `fn(u32,u8,u32) -> i32` | **gone** | `krun_vmm_builder_vcpus(KrunVmmBuilder*, uint8_t, KrunError*)` `include/libkrun.h:380`; `krun_vmm_builder_ram_mib(...)` `:382`; Rust `api/vmm_builder.rs:46,54` |
| 6 | `krun_set_gpu_options3` | `fn(u32,u32,u64,i32) -> i32` | **gone** (fork-only; never upstream) | **no equivalent.** `KrunGpuDevice krun_gpu_device_new(uint32_t virgl_flags, KrunDisplayBackend backend)` `include/libkrun.h:609` has **no render-server fd parameter**; Rust `GpuDevice::new(virgl_flags, backend)` `api/device_builders.rs:1637`, `shm_size` `:1645`.  Fork-only on the 1.19 line: `fb988873` has no `krun_set_gpu_options3` either (see `01-raw-fork-asset-elf.txt`, 69 dynsyms). |
| 7 | `krun_check_nested_virt` | `fn() -> i32` | **changed** (return type) | `bool krun_check_nested_virt(void)` `include/libkrun.h:688`; Rust `pub fn check_nested_virt() -> bool` `api/vmm_builder.rs:289` |
| 8 | `krun_set_nested_virt` | `fn(u32,bool) -> i32` | **gone** | `void krun_vmm_builder_nested_virt(KrunVmmBuilder*, bool)` `include/libkrun.h:402`; Rust `api/vmm_builder.rs:100` |
| 9 | `krun_set_root` | `fn(u32,*const c_char) -> i32` | **gone** | removed by `d83aed5e` *"lib: remove deprecated krun_set_root"*; replaced by a virtiofs device: `KrunFsDevice krun_fs_device_new(KrunStr tag, KrunStr host_path, KrunError*)` `include/libkrun.h:216` (+ `_read_only` `:219`, `_new_null` `:228`), attached via `krun_mmio_device_manager_add` `:201`; Rust `api/device_builders.rs:450,474` |
| 10 | `krun_add_disk` | `fn(u32,*c_char,*c_char,bool) -> i32` | **gone** | `KrunBlockDevice krun_block_device_new(KrunStr id, KrunStr disk_image_path, uint32_t format, KrunError*)` `include/libkrun.h:506`; `krun_block_device_set_read_only` `:509`; Rust `api/device_builders.rs:1249,1265` |
| 11 | `krun_disable_implicit_console` | `fn(u32) -> i32` | **gone** | removed by `ce4146d1`; console is now explicit: `krun_console_device_builder()` `include/libkrun.h:253`, `krun_console_builder_add_default_console` `:303`; Rust `api/device_builders.rs:608` |
| 12 | `krun_set_console_output` | `fn(u32,*const c_char) -> i32` | **gone** | same commit `ce4146d1`; file-backed console is `krun_console_builder_add_tty_port(handle, name, tty_fd, ...)` `include/libkrun.h:276`, or `krun_vmm_builder_add_serial_console(builder, in_fd, out_fd, err)` `:400` |
| 13 | `krun_add_virtio_console_default` | `fn(u32,i32,i32,i32) -> i32` | **gone** (only a doc reference survives) | the name appears in `main` *only* in prose: `include/libkrun.h:291` ("Replicates the v1 … behaviour"), `api/device_builders.rs:706`. Replacement is `krun_console_builder_add_default_console` `include/libkrun.h:303` |
| 14 | `krun_add_net_unixstream` | `fn(u32,*c_char,i32,*mut u8,u32,u32) -> i32` | **gone** | `KrunNetDevice krun_net_device_new_unixstream_fd(KrunStr id, int fd, KrunBytes mac, uint32_t features, uint32_t flags, KrunError*)` `include/libkrun.h:545` (+ `_path` `:538`, `_unixgram_fd/_path` `:528/535`, `_tap` `:547`); Rust `api/device_builders.rs:1334` |
| 15 | `krun_add_vsock_port2` | `fn(u32,u32,*c_char,bool) -> i32` | **gone** | `KrunVsockDevice krun_vsock_device_new(uint64_t cid, uint32_t tsi_features, KrunError*)` `include/libkrun.h:487` + `void krun_vsock_device_add_unix_port(handle, uint32_t port, KrunStr path, bool listen)` `:493`; Rust `api/device_builders.rs:1172,1184,1206` |
| 16 | `krun_set_port_map` | `fn(u32,*const *const c_char) -> i32` | **gone** | replaced by the vsock device's host-port map: `KrunResult krun_vsock_device_add_port_forward(KrunVsockDevice, KrunStr mapping /* "guest:host" */, KrunError*)` `include/libkrun.h:490`; Rust `api/device_builders.rs:1197`.  TSI hijack flags moved to `krun_vsock_device_new` (`KRUN_TSI_FLAGS_HIJACK_INET`/`_UNIX` `include/libkrun.h:160-163`) |
| 17 | `krun_set_workdir` | `fn(u32,*const c_char) -> i32` | **gone** | moved out of libkrun into the init config: `krun_init_builder_workdir(KrunInitBuilder*, KrunStr)` `include/libkrun_init.h:154`; Rust `init/init-blob/src/config.rs:211` |
| 18 | `krun_set_exec` | `fn(u32,*c_char,*const *const c_char,*const *const c_char) -> i32` | **gone** | `krun_init_builder_arg(s)` `include/libkrun_init.h:142,145`; Rust `init/init-blob/src/config.rs:181,187` |
| 19 | `krun_set_rlimits` | `fn(u32,*const *const c_char) -> i32` | **gone** | `krun_init_builder_rlimit(s)` `include/libkrun_init.h:160,163`; Rust `init/init-blob/src/config.rs:227,233` |
| 20 | `krun_set_profile_path` | `fn(u32,*const c_char) -> i32` | **gone** (fork-only) | **no equivalent** on `main`. Fork-only on 1.19 too (not in `fb988873`'s header). |
| 21 | `krun_set_kernel_cmdline_append` | `fn(u32,*const c_char) -> i32` | **gone** (fork-only) | `void krun_payload_append_cmdline(KrunPayload, KrunStr extra)` `include/libkrun.h:344`; Rust `api/payload.rs:152` |
| 22 | `krun_start_enter` | `fn(u32) -> i32` | **gone** | `KrunVmmHandle krun_vmm_handle(KrunVmm, KrunError*)` `include/libkrun.h:439`; `void krun_vmm_run(KrunVmm)` `:441`; Rust `api/vmm_builder.rs:245,266` |

The whole removal is a single commit: `a3d31822` deletes `include/libkrun.h`
(-1416), `krun-sys/` (5 files) and 3220 lines from
`src/libkrun/src/lib.rs`; after `911aa81b` the file is regenerated by
`make gen-libkrun-bindings` from `bindings/libkrun-via-cdylib-weak/ffier-krun.json`.
`main`'s `src/libkrun/src/lib.rs` is now 112 lines with **zero `no_mangle`**
and **zero `extern "C"`** definitions — the C symbols are emitted by the
`ffier::generate_bridge!` macro (`src/libkrun/src/lib.rs:11-15`).

### New `krun_*` entry points cang might want (record only, per the ticket)

The full v2 export list is in `01-raw-main-ffi-exports.txt` (78 symbols).  The
ones that map onto something cang does today:

| cang need | main entry point | file:line |
|---|---|---|
| launch/power | `krun_vmm_builder_build`, `krun_vmm_run`, `krun_vmm_destroy`, `krun_vmm_handle`, `krun_vmm_handle_pause/resume/shutdown` | `include/libkrun.h:426,441,443,439,448,450,461` |
| guest init install (**new requirement**, see (d)) | `krun_init_config_builder`, `krun_init_builder_*`, `krun_init_config_apply`, `krun_init_config_apply_in` | `include/libkrun_init.h:99,139-178,113,125` |
| root virtiofs | `krun_fs_device_new*`, `krun_fs_device_set_overlay`, `krun_fs_overlay_new/add_dir/add_file`, `krun_fs_device_set_dax_window_size` | `include/libkrun.h:216-247,236,311-329,245` |
| disks | `krun_block_device_new`, `krun_block_device_set_read_only/direct_io/sync_mode` | `include/libkrun.h:506-522` |
| network (passt fd) | `krun_net_device_new_unixstream_fd/_path`, `_unixgram_fd/_path`, `_tap` | `include/libkrun.h:528-549` |
| vsock attach/exec/waypipe | `krun_vsock_device_new`, `krun_vsock_device_add_unix_port`, `krun_vsock_device_add_port_forward` | `include/libkrun.h:487-495` |
| publish (TSI) | `krun_vsock_device_add_port_forward` + TSI flags on `krun_vsock_device_new` | `include/libkrun.h:487,490` |
| console capture | `krun_console_device_builder`, `krun_console_builder_add_tty_port/_inout_port/_default_console`, `krun_vmm_builder_add_serial_console`, `krun_vmm_builder_set_kernel_console` | `include/libkrun.h:253,276,283,303,400,388` |
| self-made kernel cmdline | `krun_payload_append_cmdline`, `krun_payload_cmdline` | `include/libkrun.h:344,342` |
| GPU | `krun_gpu_device_new`, `krun_gpu_device_shm_size`, `krun_display_backend_*`, `krun_display_info_builder_*` | `include/libkrun.h:609,611,600-605,554-565` |
| nested virt | `krun_check_nested_virt`, `krun_vmm_builder_nested_virt` | `include/libkrun.h:688,402` |
| not needed but new | `krun_vmm_builder_acpi`, `krun_vmm_builder_split_irqchip`, `krun_vmm_builder_shutdown_support`, `krun_vmm_builder_add_smbios_oem_string`, `krun_vhost_user_device_new`, `krun_balloon_device_new`, `krun_rng_device_new`, `krun_input_device_new*`, `krun_payload_load_external/_firmware` | `include/libkrun.h:413,404,424,415,581,468,475,629/637,338/340` |
| error/result | `krun_error_code`, `krun_error_message`, `krun_error_result`, `krun_vmm_error_code`, `krun_vmm_error_message`, `krun_result_name`, `krun_str_free`, `krun_free_object_array` | `include/libkrun.h:671,673,675,679,681,690,111,116` |

### The error model changed (affects every call site)

1.x: `int32_t`, negative errno.  2.0: `KrunResult` = `uint64_t` with
`KRUN_RESULT_SUCCESS 0` (`include/libkrun.h:40-41`) plus an opaque
`KrunError* err_out`; many setters return `void` and cannot fail at all
(`krun_mmio_device_manager_add` `:201`, `krun_fs_device_set_overlay` `:236`,
`krun_vmm_builder_payload/devices` `:384,386`).  cang's
`check_setup(name, rc)` / `check_start(name, rc)` (`launcher.rs:463-478`,
`if rc < 0`) and the `Result<i32>` shape of `LibkrunApi` must be rewritten,
and `DynamicLibkrunApi`'s `init_log` fallback to `krun_set_log_level`
(`dynamic.rs:326-341`) has no fallback any more.

---

## (b) Soname, LIBDIR and install layout

**main** (`Makefile` @ `a980e779`):

```
ABI_VERSION=2                FULL_VERSION=2.0.0
KRUN_BINARY_Linux = libkrun.so.2.0.0
KRUN_SONAME_Linux = libkrun.so.2
KRUN_BASE_Linux   = libkrun.so
LIBDIR_Linux      = lib64                 (unchanged)
KRUN_INIT_ABI_VERSION=0      KRUN_INIT_FULL_VERSION=0.1.0
KRUN_INIT_BINARY_Linux = libkrun_init.so.0.1.0
KRUN_INIT_SONAME_Linux = libkrun_init.so.0
KRUN_INIT_BASE_Linux   = libkrun_init.so
```

`make install` now installs, in addition to the v1 set:
`lib64/libkrun_init.so{, .0, .0.1.0}`, `lib64/pkgconfig/libkrun_init.pc`,
`include/libkrun_init.h` (`01-raw-soname-makefile-cargo.txt`, "main Makefile
install target").  `lib64` remains the Linux LIBDIR, so the directory *shape*
is unchanged; only the file names/versions and the extra init library are new.

**What the existing fork asset contains** (pinned `v1.19.5-cang.1`,
`nix/pins.nix:107-121`, 69 exported `krun_*` dynsyms):

```
lib64/libkrun.so -> libkrun.so.1 -> libkrun.so.1.19.5   (SONAME libkrun.so.1)
lib64/pkgconfig/libkrun.pc                              (Version: 1.19.5, libdir=/usr/lib64)
include/libkrun.h  libkrun_display.h  libkrun_input.h
DT_NEEDED: libpipewire-0.3.so.0, libvirglrenderer.so.1, libgcc_s.so.1, libc.so.6, ld-linux-x86-64.so.2
```

**Does `nix/pkgs/libkrun.nix` still find what it expects?**  Yes structurally.
It does `cp -a lib64/. "$out/lib/"` and `cp -a include/. "$out/include/"`
(`nix/pkgs/libkrun.nix:30-31`), and both directories survive on `main`; the
`postFixup` loop `for so in "$out"/lib/libkrun.so.*` (`:57`) still matches the
single real `libkrun.so.2.0.0` and skips the symlinks.  Two follow-ups:

1. `libkrun_init.so*` will now be copied into `$out/lib` and `libkrun_init.pc`
   into `$out/lib/pkgconfig`.  `nix/pkgs/cang-prebuilt.nix:89` and
   `nix/pkgs/cang-rust.nix:46` glob `libkrun.so*`, which does **not** match
   `libkrun_init.so*`, so the new library will silently not be shipped to
   `$out/lib/cang` — cang's init installer would have to be added explicitly
   (see (e)).
2. The `--add-rpath '$ORIGIN:virglrenderer:pipewire'` fixup
   (`nix/pkgs/libkrun.nix:57-63`) and the flake's `libkrun-loadable` check
   (`flake.nix:252-276`) were written for 1.19.5's DT_NEEDED set.  `main`
   removed the built-in virtio-snd device (`7e5c6c4b`) and moved
   `rutabaga_gfx` from an in-tree crate to the crates.io dependency
   `rutabaga_gfx = "0.1.85"` (`src/devices/Cargo.toml:45`), so
   `libpipewire-0.3.so.0` will probably disappear while
   `libvirglrenderer.so.1` should stay (via that crate).  Keeping the extra
   rpath entry is harmless; the comment in `flake.nix` becomes stale.

**Loader**: `DEFAULT_LIBKRUN_NAMES = ["libkrun.so.1", "libkrun.so"]`
(`crates/cang/src/runtime/vm/libkrun/dynamic.rs:13`, candidate order built at
`:290-305`).  On `main` the `libkrun.so.1` candidate stops existing; the
`libkrun.so` fallback still resolves, but `libkrun.so.2` should be added to the
probe list and to the assertions in
`crates/cang/src/runtime/vm/libkrun/tests.rs:444-463`.

---

## (c) Cargo feature-flag delta

Fork CI (`deps/libkrun/.github/workflows/publish-cang-release.yml`,
`Build libkrun full-feature package`) runs:

```
make clean
make BLK=1 NET=1 GPU=1 SND=1 INPUT=1
make DESTDIR="$PWD/tmp" PREFIX=/usr install
```

The in-tree `nix/dev` sub-flake builds the same source through nixpkgs'
recipe: `pkgs.libkrun.override { withBlk = true; withNet = true; withGpu = true;
withSound = true; withInput = true; }` (`nix/dev/flake.nix:33-48`), which
expands to `BLK=1 NET=1 GPU=1 SND=1 INPUT=1` in the Makefile
(`pkgs/by-name/li/libkrun/package.nix` makeFlags).  nixpkgs' recipe also
exposes `withTimesync` → `TIMESYNC=1` (unused by `nix/dev`).

`krun_display`/`krun_input` are **crate** (optional-dependency) features, not
Makefile knobs: `gpu = [..., "krun_display"]` and `input = ["krun_input", ...]`
in both revisions — unchanged.

| feature | 1.19.5 (`fb988873`) | main (`a980e779`) | note |
|---|---|---|---|
| `default` | `["init-blob"]` | *(none)* | the bundled-init default is gone |
| `init-blob` | `["dep:init-blob"]` | **removed** | init is now the separate crate `krun-init-blob` (`init/init-blob`), built by the Makefile with `cargo build --release -p krun-init-blob --features ffi`; `Makefile` still has an `INIT_BLOB=0 → --no-default-features` branch that is now dead |
| `snd` | `["vmm/snd", "devices/snd"]` | **removed** | `7e5c6c4b` *"Remove builtin virtio-snd device"*; sound only via `vhost-user` now.  `make SND=1` on `main` is a **silent no-op** (the `ifeq ($(SND),1)` block was deleted) |
| `efi` | `["blk","net","vmm/efi","devices/efi"]` | **removed** | `7975175a` *"Remove EFI feature, bundled edk2"* |
| `tee` | present | present | unchanged (drives `libkrun-sev.so`/`-tdx.so` variants) |
| `net`, `blk`, `gpu`, `input` | present | present | unchanged names |
| `virgl_resource_map2` | present (`VIRGL_RESOURCE_MAP2=1`) | present | unchanged; fork CI does **not** currently set it |
| `amd-sev`, `tdx`, `aws-nitro` | present | present | `aws-nitro` Makefile now uses `+=` instead of `:=`, so it no longer clobbers the other features |
| `vhost-user` | — | **new** (`VHOST_USER=1`) | vhost-user devices incl. sound; needs file-backed (memfd) guest memory |
| `timesync` | — | **new** (`TIMESYNC=1`) | not present in 1.19.x at all, despite nixpkgs' recipe having a `withTimesync` knob |
| `ffi` | — | **new** (`FFI=1`) | gates the ffier bridge; required to emit the v2 C header / exports |

Consequences for the cang side: the fork CI's `SND=1` becomes dead, `make` now
also builds `libkrun_init.so`, and the `nix/dev` recipe needs a `main`-shaped
build (it currently neither enables `ffi` nor builds `krun-init-blob`) — see (e).

---

## (d) init / kernel protocol — input for ticket 05

**`libkrun_init.so` is a host-side library, not a kernel/firmware artifact.**
It is built from the new crate `krun-init-blob` (`init/init-blob/Cargo.toml`,
`[lib] name = "krun_init", crate-type = ["lib","cdylib"]`) with `--features ffi`,
and installed with soname `libkrun_init.so.0` / version `0.1.0` beside
`libkrun.so.2`.  It exposes
`krun_init_config_builder`, `krun_init_builder_from_oci_json`,
`krun_init_builder_arg/args/env_var/env/workdir/mount/rlimit/rlimits/dhcp`,
`krun_init_builder_build`, and
`krun_init_config_apply[_in](handle, [lib_handle,] KrunFsOverlay, KrunPayload, KrunInitError*)`
(`include/libkrun_init.h:99-178`).

**What changed vs 1.19.5 — init injection is now the caller's job.**

* 1.19.5 injected the guest init implicitly: `src/libkrun/src/lib.rs:2923`
  builds the boot cmdline as `format!("{DEFAULT_KERNEL_CMDLINE} init={INIT_PATH}")`
  with `INIT_PATH = "/init.krun"` (`src/libkrun/src/lib.rs:91`), and
  `krun_add_virtiofs3`/`krun_set_root_disk_remount` exposed the init blob as a
  virtual read-only file.
* `main` removed that at `502116e7` *"libkrun: remove implicit init injection"*
  (the commit lands after the temporary `-ENOTSUP` stubs and is then itself
  superseded by `a3d31822`).  Today the **caller** must build an init `Config`
  and call `Config::apply_in(lib_handle, overlay, payload)` —
  `init/init-blob/src/config.rs:91,110` — which adds the overlay files
  (`INIT_PATH = "/init.krun"` `:50`, `"/.krun_config.json"` `:305`) and appends
  `KERNEL_INIT_ARG = "init=/init.krun"` (`:53`, appended at `:132`/`:152`).

This is the single biggest behavioural change for cang: cang never boots its
workload as PID 1 — `krun_set_exec` carries `exec_path =
/nix/store/…/bin/cang-guest-init` with its `enter …` argv
(`crates/cang/src/runtime/launch/plan.rs`, `launcher.rs:323-327`) and relies on
libkrun's injected init to read `/.krun_config.json` and exec it.  On `main`
cang must either ship+load `libkrun_init.so` and apply a `Config` (built
explicitly, or from the OCI `config.json` via
`krun_init_builder_from_oci_json`, `config.rs:171`), or provide its own
`/init`.  Exec/env/workdir/rlimits are *only* available through that init
config now — `main` keeps `krun_nitro_config_exec_path/args/env/workdir/rlimits`
for AWS Nitro and nothing else (`4d2201e7`).

**Kernel cmdline / firmware expectations are otherwise unchanged:**

* `DEFAULT_KERNEL_CMDLINE` is byte-identical to 1.19.5
  (`src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:5-12`):
  `reboot=k panic=-1 panic_print=0 nomodule console=hvc0 rootfstype=virtiofs rw quiet no-kvmapf`.
  `main` no longer inserts `init=`, block-root or any exec/env tokens itself.
* `libkrunfw` ABI is unchanged: `krunfw_get_kernel(u64* guest, u64* entry,
  usize* size) -> *mut c_char` (`src/libkrun/src/api/payload.rs:174-198`, same
  signature as `fb988873` `src/libkrun/src/lib.rs:138`), plus the optional
  `krunfw_get_qboot`/`krunfw_get_initrd` (TEE only).  The firmware soname is
  still `libkrunfw.so.5` (`src/libkrun/src/api/payload.rs:273`), so the pinned
  `libkrunfwRelease` `v5.6.2-cang.1` (`nix/pins.nix:123-140`) remains
  symbol-compatible.
* `KernelBundle { host_addr, guest_addr, entry_addr, size }` is unchanged in
  the fields that matter (`src/libkrun/src/vmm/vmm_config/kernel_bundle.rs:7-13`)
  and the bundled kernel is still mmap'd at `guest_addr` and booted at
  `entry_addr` (`Payload::KernelMmap`, `src/libkrun/src/vmm/builder.rs:1586-1712`).
* **Virtio feature advertisement is unchanged for cang's devices.**  Transport
  is still virtio-mmio + MP tables by default; ACPI is strictly opt-in
  (`krun_vmm_builder_acpi`, `c98080a8`).  There is no
  `VIRTIO_F_ACCESS_PLATFORM` / `VIRTIO_F_RING_PACKED` introduction on either
  revision (`01-raw-init-kernel-protocol.txt`).  The device-set deltas are
  built-in `virtio-snd` removed, `vhost-user` added (guest sees a normal virtio
  device; the memfd/shared-memory requirement is host-side), and `timesync`
  added — all opt-in.
* `main` moved `rutabaga_gfx` out of the tree to crates.io `0.1.85`; the fork's
  own GPU commits touch `src/rutabaga_gfx/src/{renderer_utils,rutabaga_utils,virgl_renderer}.rs`
  (see `01-raw-init-kernel-protocol.txt`, fork diffstat), so those patches have
  no path to apply onto `main` and must be re-expressed against the crate.

**Verdict for ticket 05:** there is **no kernel/firmware ABI obstacle** to
booting the pinned libkrunfw v5.6.2-cang.1 (6.12.109) under libkrun 2.0.0 —
same `krunfw_get_kernel` contract, same `libkrunfw.so.5` soname, same default
cmdline, same mmio transport, and every new device feature is opt-in.  What
*does* have to be re-plumbed is host-side: the guest init is no longer
injected, so ticket 05/07 must confirm a boot through an explicitly applied
init `Config`.  That cannot be settled read-only — it needs the live boot.

---

## (e) Concrete cang-side edits the re-pin will need

**A. Launcher / ABI rewrite (blocking; this is the bulk of ticket 04 + 07)**

1. `crates/cang/src/runtime/vm/libkrun/api.rs` — replace the 22-method
   `LibkrunApi` trait with the v2 object model (builder → payload → devices →
   `build` → `run`); every `Result<i32>` becomes `Result<()>`/`Result<handle>`
   with `KrunError*` handling.
2. `crates/cang/src/runtime/vm/libkrun/dynamic.rs` —
   * add `"libkrun.so.2"` to `DEFAULT_LIBKRUN_NAMES` (`:13`);
   * rebind: `krun_set_log_level` (`:143`) disappears (drop the fallback in
     `init_log`, `:326-341`); `krun_create_ctx`/`krun_free_ctx` (`:148-155`)
     become `krun_vmm_builder_new`/`krun_vmm_destroy`;
   * `krun_set_gpu_options3` (`:160`), `krun_set_profile_path` (`:210`),
     `krun_set_kernel_cmdline_append` (`:214`) are fork extensions that must be
     re-added on `main` (or dropped) — see (5);
   * add an optional/required binding for `libkrun_init.so.0` (or its
     `krun_init_*` symbols).
3. `crates/cang/src/runtime/vm/libkrun/launcher.rs` — reorder the whole
   `configure_and_start` (`:157-355`): build `Payload` (`krun_payload_load_krunfw`,
   then `append_cmdline` instead of `krun_set_kernel_cmdline_append`), create the
   `MmioDeviceManager` and add fs(root)/blk/console/net/vsock/gpu devices
   (replacing `set_root`/`add_disk`/`disable_implicit_console`/
   `set_console_output`/`add_virtio_console_default`/`add_net_unixstream`/
   `add_vsock_port2`/`set_port_map`), then `VmmBuilder` (`vcpus`, `ram_mib`,
   `payload`, `devices`, `nested_virt`, optional `acpi`) and `krun_vmm_run`.
   `set_port_map` maps to `krun_vsock_device_add_port_forward` + TSI flags on
   `krun_vsock_device_new`.  `set_workdir`/`set_exec`/`set_rlimits` move into
   the init `Config`; `set_profile_path` has no replacement.
4. `crates/cang/src/runtime/vm/libkrun/tests.rs` — the fake `LibkrunApi`
   replay expectations (`:241-254`, `:981-1126`) and the load-order assertions
   (`:444-463`) all encode the 1.x symbol names and call order and must be
   rewritten.
5. **GPU render-server fd** — `main`'s `KrunGpuDevice`/`GpuDevice` has no
   render-server fd (`api/device_builders.rs:1626-1652`), which is exactly what
   `krun_set_gpu_options3` added for cang's venus path
   (`launcher.rs:357-399`, `VIRGLRENDERER_RENDER_SERVER`,
   `CANG_RENDER_SERVER_FD`).  Either the fork re-adds an fd-taking variant on
   top of `main`'s `GpuDevice`, or cang's `GpuMode::Drm` stops working.  Note
   also `GpuDevice::new` now *requires* a `KrunDisplayBackend`; the headless
   case cang uses needs a zero-display backend.  And the fork's 1.19 patches
   live in `src/rutabaga_gfx/`, which `main` no longer carries.
6. **Guest init (new dependency)** — build a
   `Config` (explicit `arg/env_var/workdir/rlimit`, or
   `krun_init_builder_from_oci_json`) and call `krun_init_config_apply_in` with
   the `dlopen`ed libkrun handle, the root `FsOverlay` and the `Payload`;
   keep the `Config` alive for the VM's lifetime.  This replaces the implicit
   `init=/init.krun` injection cang gets today.

**B. Packaging / build**

7. `nix/pkgs/cang-prebuilt.nix:88-96` and `nix/pkgs/cang-rust.nix:45-53` —
   the `libkrun.so*` symlink loops must additionally expose `libkrun_init.so*`
   (and currently do **not** match it) for the new init library to be loadable
   from `$out/lib/cang`.
8. `nix/pins.nix:107-121` — `libkrunRelease.tag` → `v2.0.0-cang.1`
   (asset names stay `libkrun-<arch>-linux-full.tgz`); drive it with
   `scripts/update-libkrun.sh --tag v2.0.0-cang.1` (the script's
   `^(cang-[0-9a-f]{12}|v<ver>-cang.<n>)$` check already accepts it).
9. `deps/libkrun` submodule pointer → the same commit (ticket 04/06/07).
10. `nix/pkgs/libkrun.nix` — unchanged copy logic (`lib64`→`lib`,
    `include`→`include`); re-check the `--add-rpath` list against `main`'s
    DT_NEEDED set (pipewire likely gone) and keep the `libkrun.so.*` fixup
    loop.
11. `flake.nix:252-276` — update the `libkrun-loadable` comment/expectations
    (they describe 1.19.5's pipewire edge); the `ldd` loop itself still works.
12. `nix/dev/flake.nix:33-48` — `withSound = true` → `SND=1` is a no-op on
    `main`; decide `vhost-user` (and drop `withSound`), and teach the recipe
    the `main` build shape (it must also build `krun-init-blob` with `ffi`).
13. Fork CI `publish-cang-release.yml` — drop/repurpose `SND=1`; the
    `LIBDIR_Linux=lib64` trap already handled; the asset will gain
    `lib64/libkrun_init.so*`, `lib64/pkgconfig/libkrun_init.pc`,
    `include/libkrun_init.h` automatically from `make install`.
14. `crates/cang-repository-tests/tests/repository.rs` — assertions at `:279-300`
    (versioned pin tags — `v2.0.0-cang.1` passes), `:373-374`
    (`${libkrun}/lib/libkrun.so*` symlink contract), `:326-332` and
    `:520-533` (workflow / `cang-rust.nix` strings) need review for any new
    `libkrun_init` line.
15. `README.md` and `docs/` — user-visible: new libkrun/libkrunfw versions,
    and (if the GPU path changes) the GPU requirements.

**C. Verification**

16. `nix develop --command cargo fmt --check`, `cargo clippy --all-targets
    --all-features -- -D warnings`, `cargo deny check`, `cargo test`
    (note the pre-existing `unshare_cleanup` exit-127 and the
    `cang-guest-init` fork-hang flake from project memory).
17. `nix build .#cang`, `nix build .#container`; then the live recipe and the
    chromium/wayland GPU smoke on the new pin (ticket 08) — the GPU smoke is
    the one that will catch the missing render-server fd (item 5).

---

## Answer to the ticket's four questions (one line each)

1. **ABI**: 20 of cang's 22 `krun_*` bindings are **gone** in `main` and 2
   (`krun_init_log`, `krun_check_nested_virt`) **changed signature**; the whole
   v1 C API was deleted by `a3d31822` and replaced by an ffier-generated v2
   object/builder API — cang cannot even `dlopen`-bind (`krun_set_log_level`
   missing) without a launcher rewrite.  New entry points cang will want are
   listed in the "New `krun_*` entry points" table.
2. **Soname / layout**: `main` ships `libkrun.so.2.0.0` with SONAME
   `libkrun.so.2` (`ABI_VERSION=2`, `FULL_VERSION=2.0.0`), `LIBDIR_Linux=lib64`
   unchanged, plus a new `libkrun_init.so.0`/`libkrun_init.so.0.1.0` and
   `include/libkrun_init.h`; the existing fork asset contains only
   `libkrun.so.1.19.5`/`.so.1`/`.so` (SONAME `libkrun.so.1`) + `libkrun.pc` +
   3 headers, and `nix/pkgs/libkrun.nix`'s `lib64`/`include` copy still fits.
3. **Features**: fork CI/nix-dev use `BLK NET GPU SND INPUT`; `main` removed
   `snd`, `efi` and the `init-blob` libkrun feature (init is now the separate
   `krun-init-blob` crate built with `ffi`) and added `vhost-user`, `timesync`,
   `ffi`; `krun_display`/`krun_input` and `virgl_resource_map2` are unchanged.
4. **init/kernel**: `krunfw_get_kernel`, `libkrunfw.so.5`, `KernelBundle` and
   the default cmdline are unchanged, so the pinned libkrunfw is ABI-compatible,
   but `main` no longer injects `/init.krun` or `init=` and moves
   exec/env/workdir/rlimits into the host-side `libkrun_init.so` `Config::apply`
   API — ticket 05's live boot must therefore drive an explicit init Config.

### cang-side edit list (short form)

`api.rs` (ABI rewrite) · `dynamic.rs` (rebind, add `libkrun.so.2`, drop
`krun_set_log_level` fallback, bind `libkrun_init`) · `launcher.rs` (builder/
device/init restructure; port map → vsock port-forward) · `tests.rs` (replay
fixtures) · re-add fork's render-server-fd GPU entry point on `main`'s
`GpuDevice` · `nix/pins.nix` tag → `v2.0.0-cang.1` + submodule pointer ·
`nix/pkgs/libkrun.nix` (rpath re-check) · `flake.nix` `libkrun-loadable` ·
`nix/dev/flake.nix` (`SND`→`vhost-user`/drop, `ffi`+init build) ·
`nix/pkgs/cang-{prebuilt,rust}.nix` (ship `libkrun_init.so*`) ·
fork `publish-cang-release.yml` feature list · `cang-repository-tests` pin/asset
assertions · README/docs · then fmt/clippy/deny/test + `nix build` + live GPU
smoke.
