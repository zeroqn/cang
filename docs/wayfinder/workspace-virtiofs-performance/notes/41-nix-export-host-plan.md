# Host plan: a second, read-only virtio-fs export for `/nix`

Tree: `main` `b3901cbe`, `deps/libkrun` `3c2eb13b` = tag `v2.0.0-cang.16`. Scope is **host side
only**: prepared root, launcher, launch-config plumbing, tests, ordering. Guest-side mounting and the
`/nix` writability decision are named only where the host plan depends on them.

The fork carries what the device needs: `FsDevice::new_read_only`
(`deps/libkrun/src/libkrun/src/api/device_builders.rs:639-642`) and per-device
`set_worker_threads`/`set_zero_message_open`/`set_no_flush`/`set_negative_entry`/
`set_cache_symlinks`/`set_opcode_stats` (`:722-826`). It carries **no** `set_xattr` (§5.6).

## 1. Prepared root

`prepare` (`crates/cang/src/runtime/vm/prepared_root/mod.rs:49-57`) applies a plan of bind grafts in
order: the task rootfs at `<task_state_dir>/prepared-root` (`:108-117`), every `config.mounts` entry
at `prepared_target(root_export, mount.target)` (`:118-125`), the optional `--guest-init` file
override (`:126-133`), then runtime `/etc` (`:175`). `apply` only knows directory-vs-file sources and
a `read_only` remount (`:159-174`). **There is no `/nix` branch in this file.**

Today's `/nix` graft is an ordinary `BindMount`: `mounts_with_host_nix_overlay` appends
`BindMount::directory(mergeddir, NIX_TAG, NIX_TARGET)` (`config/components/mounts.rs:24-28`) and
`LaunchConfig::build_for_task` calls it (`config/mod.rs:31-34`). `BindMount::directory` sets
`read_only: false` (`model.rs:196-208`); `NIX_TAG = "cang-nix"`, `NIX_TARGET = "/nix"`
(`model.rs:34-35`).

**What must stop being grafted: exactly that one entry** (`mounts.rs:24-28`). Nothing else in the
prepared root knows about `/nix`; what remains as the graft target is the image's own `/nix` inside
the task rootfs, i.e. the root export's own source (`mod.rs:112-117`).

The **mode switch** is `host_nix_overlay` (`model.rs:432`, `:489`). cang launches **only**
host-overlay: `persistent_disks/nix.rs:8-15` unconditionally emits `CANG_NIX_OVERLAY=1` and
`CANG_NIX_HOST_OVERLAY=1`, `nix.rs:1-5` records that no raw `/nix` disk is prepared or attached any
more, and `persistent_disks/mod.rs:84-90` asserts no `cang-nix.raw` is created. The raw-disk *guest*
branch still exists (`crates/cang-guest-init/src/guest_init/components/nix/root.rs:158-228`: find a
btrfs disk, mount at `/run/cang/nix-disk`, `mount -t overlay` with `lowerdir=/nix`) but cang returns
before it (`root.rs:168-170`).

**Raw-disk mode cannot be served by a host-directory export.** There the writable half of the store is
a guest-attached btrfs image (`/dev/vda`, `components/disk/nix.rs:4-10`, used as the overlay upper at
`nix/root.rs:197-218`), so no host directory holds the merged store — the host has only the image
cache lowerdir (`runtime/session/nix_overlay.rs:251-258`). An export could name the image store only,
and the guest's upper would sit *above* the export instead of inside it, so the two exports would
disagree about `/nix`. The export is correct only in host-overlay mode, which is the only mode cang
has.

One interaction: with `/nix` un-grafted the `--guest-init` override target resolves inside the
task-rootfs snapshot instead of the overlay merged dir. The "existing file target is left untouched"
rule and its `EXDEV` rationale (`prepared_root/mod.rs:200-235`) still hold — the image store file
exists and is 0555 — but the copy-up hazard it documents no longer applies to that path.

## 2. Launcher

`configure` registers devices on one MMIO manager in call order (`launcher.rs:385-388` manager,
`:390-392` GPU/console, `:394` `fs_overlay_new`, `:409` init config, `:416-483` rootfs + setters,
`:485-499` disks, `:501-502` vsock/net, `:503` balloon). The root device is
`fs_device_new(CANG_FS_ROOT_TAG, &config.task_rootfs, false)` (`:416-420`).

`CANG_FS_ROOT_TAG = "/dev/root"` (`crates/cang-libkrun/src/lib.rs:24-25`) is the kernel's default root
device name and nothing else: `DEFAULT_KERNEL_CMDLINE` has `rootfstype=virtiofs` and no `root=`
(`deps/libkrun/src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:5-7`). A second export needs its own
constant beside it, with no such constraint.

The second device is `fs_device_new(tag, host_path, true)` (`crates/cang-libkrun/src/api.rs:50` →
`linked.rs:317-326` → `new_read_only` → `Fs::new(.., read_only)` → `worker.rs:394-395` wrapping
`PassthroughFsRo`).

**Setters it gets**, with the reason the fork gives each: `set_worker_threads` (the pool is per device,
`api.rs:52`, note 01 `notes/01-fuse-negotiation-and-queue-facts.md:527`); `set_no_flush` (the RO
wrapper's `flush` is a delegated no-op, `read_only.rs:136-139`); `set_zero_message_open` (caveat
below); `set_negative_entry` (store names are immutable, and a guest create/rename invalidates its
own dentry, `lib.rs:139-143`); `set_cache_symlinks` (`/nix/store` holds the targets that repeat,
`lib.rs:108-118`); `set_opcode_stats` (the new device's histogram has to be readable separately).

**Setters it must not get.** `fs_device_set_overlay`: the overlay carries the krun-init blob and init
config injected for the *root* export (`launcher.rs:394`, `:409`, `:721-726`; `api.rs:32`, `:40-45`),
and `attach` resolves overlay entries into the device's own namespace
(`device_builders.rs:871-891`), so setting it would graft the init blob and the `/.cang_config.json`
entry inside the store export. `set_entry_timeout`/`set_attr_timeout`: the fork has them
(`device_builders.rs:801-816`) but the seam exposes neither (`api.rs:50-62`) and note 40 §5 priced
them at 0-5% net requests on `/nix` (`notes/40-cache-symlinks-shipped.md:86-112`). Nothing for DAX
(note 05 `notes/05-dax-window-viability.md:163`; note 40 §6.1).

**Zero-message-open caveat on a read-only export.** With it on the guest sends no `FUSE_OPEN`, so the
RO wrapper's open-time check (`read_only.rs:37-55`, `:110-120`) never runs: a write-open succeeds in
the guest and the first `WRITE`/`CREATE`/`SETATTR` returns `EROFS` (`read_only.rs:415-443`,
`:338-347`). With it off the same write-open fails at `open(2)`. Enforcement is identical, only the
errno point differs; reads still work because the zero-message-open read path opens the inode
`O_RDONLY` and `:1075-1077` gates only `ATOMIC_O_TRUNC`.

**Device order.** MMIO bases/IRQs are handed out in registration order
(`vmm/device_manager/kvm/mmio.rs:128-166`) and the `virtio_mmio.device=` entries are regenerated from
the actual registrations (`vmm/builder.rs:1444-1458`, `mmio.rs:170-187`; ACPI is off — default
`false` at `api/vmm_builder.rs:132-143`, and the cang seam has no `acpi` method). A new device cannot
collide, and the one written-down invariant is "balloon last" (`launcher.rs:838-858`,
`tests.rs:747-779`). Registering the export after `configure_network` and before `configure_balloon`
keeps that and leaves the root/console indices alone; registering it right after the rootfs device
instead shifts every later base and IRQ by one slot, which nothing keys on but buys nothing.

Two devices mean two servers, two inode tables, two pools. The exit-status ioctl interception wraps
any fs server (`worker.rs:212` `AugmentFs`), so the guest's `0x7602` path
(`crates/cang-guest-init/src/guest_init/runtime/exit_status.rs:18-40`) is unaffected.

Host path is `host_nix_overlay.mergeddir` (`model.rs:311-319`), mounted in the worker namespace at
`nix_overlay.rs:114-135`, materialised before the prepared root (`supervisor/vm_child.rs:340-348`);
landlock already grants it (`runtime/landlock.rs:272-273`, `:414-441`).

## 3. Config plumbing

Minimal shape: derive the export from `spec.host_nix_overlay` inside `build_for_task`
(`config/mod.rs:31-34`, `:122-161`), so there is **no new `LaunchSpec` field** and none of the 33
`LaunchSpec { .. }` literals move (`config/tests.rs:57..1984`, `prepared_root/mod.rs:521`,
`libkrun/tests.rs:494`, `landlock.rs:908`, `session/mod.rs:312`, `supervisor/mod.rs:201`,
`vm_child.rs:816`, `entry.rs:486`, `container_store.rs:151`, `plan.rs:378`). A user-facing switch adds
the `RuntimeOptions` → `LaunchPlan` → `LaunchSpec` hop in the last two bullets.

- `model.rs:485-516` `LaunchConfig`: `nix_export: Option<NixExport>`; new `struct NixExport { tag:
  String, target: String, source: PathBuf, read_only: bool, zero_message_open: Option<bool>,
  no_flush: Option<bool>, negative_entry_millis: Option<u64>, cache_symlinks: Option<bool>,
  worker_threads: usize }` beside `HostNixOverlay` (`:310-319`).
- `model.rs:34-35`: the target constant (`NIX_TARGET`, reused, or `NIX_STORE_TARGET`).
- `config/components/mounts.rs:24-28`: do not append the `/nix` mount when the export owns it.
- `config/mod.rs:122-161`: fill `nix_export`, skip the append, and put tag+target into
  `guest_config_env` (`:118-120`, which reaches the guest via `/.cang_config.json`,
  `codec.rs:22-38`, read by `CangEnv`; `IMAGE_CANG_ENV_ALLOWLIST` at `model.rs:63-82` is image env,
  not this channel). Names would sit beside `CANG_CONTAINERS_*`
  (`components/persistent_disks/containers.rs:68-75`).
- `config/codec.rs`: `serialize` beside `:159-174` (`nix_export.tag|target|source|read_only|
  zero_message_open|no_flush|negative_entry_millis|cache_symlinks`); allowlist in the `matches!` at
  `:385-437`; parse at `:472-478`; `parse_nix_export` modelled on `parse_host_nix_overlay`
  (`:657-681`) with the same presence idiom; struct literal `:506-540`.
- `runtime/launch/plan.rs:29-60`, `:148-151` (`LaunchPlan.nix_export`), `cli/mod.rs:676-684`
  (`FsOption` — flat and connection-wide today, so per-export keys need new variants such as
  `NixExport(bool)`/`NixExportXattr(bool)`), parser `:689-714`, extractors `:738-771`,
  `RuntimeOptions` fields `:617-620` and struct `:646-672`: only if a `--fs=` switch is wanted.
- `runtime/session/mod.rs:312-374` is the one production `LaunchSpec` (nothing to add in the derived
  shape); `supervisor/mod.rs:201`, `vm_child.rs:816`, `entry.rs:486` and
  `maintenance/container_store.rs:151` build `LaunchConfig` directly and need only `nix_export: None`.

## 4. Tests

1. `libkrun/tests.rs:260-268` — the fake's `fs_device_new` returns `ROOTFS` (`:33`) for every call, so
   two devices share handle 8 and the expected vector cannot tell them apart. Needs an fs-handle
   counter like `next_console_device`/`next_block_device` (`:100-163`).
2. `libkrun/tests.rs:661-745` — the exact expected-call vector gains one `FsDeviceNew`, its setters
   and one `MmioDeviceManagerAdd`; the closing message ("no per-bind virtiofs device") becomes wrong.
3. `config/tests.rs:1000-1061` — asserts a `/nix` mount exists whose source is `mergeddir`
   (`:1048-1055`); it becomes "no `/nix` mount; `nix_export.source` is `mergeddir`".
4. `config/tests.rs:1064-1114` — still valid; its error now comes from the export check rather than
   `mounts.rs:18-22`.
5. `prepared_root/mod.rs:570-830` does not break: those tests feed `test_mounts()`, which never
   contains `/nix` (the graft is added in `build_for_task`, not in the plan).

Two tests worth adding: `nix_export_is_registered_read_only_without_the_root_overlay` — exactly two
`FsDeviceNew`, the second with the nix tag / `mergeddir` / `read_only = true`; exactly one
`FsDeviceSetOverlay`, carrying `ROOTFS`; every nix setter carries the nix handle; both
`MmioDeviceManagerAdd`s precede `BalloonDeviceNew`. And
`nix_export_replaces_the_reserved_nix_mount_and_round_trips` — with a `HostNixOverlay`, `mounts` has
no `/nix` target, `nix_export.source == mergeddir` and `read_only`, and
`LaunchConfig::parse(&config.serialize()) == config` (the shape of `:1057-1060`).

## 5. Risk and ordering

1. **No cmdline or init-config change.** Device visibility comes from libkrun's generated
   `virtio_mmio.device=` entries (§2). The init config describes only argv/env/workdir/rlimits/DHCP
   (`launcher.rs:668-728`) and its blob goes to the root export (`:394`, `:721-726`).
2. **The guest must mount the tag itself.** The kernel mounts only `root=`; `/nix` today is a
   `FUSE_ATTR_SUBMOUNT` sub-volume of the root export (`SUBMOUNTS` at `server.rs:1058`,
   `ATTR_SUBMOUNT` at `linux/passthrough.rs:308-322`, `:1111-1113`). A second device is nobody's
   submount, so guest-init must `mount -t virtiofs <tag> <target>` before `/nix` is used.
3. **`/nix` writability constrains the mountpoint and the host side cannot decide it.** guest-init
   creates `/nix/var/nix/daemon-socket`
   (`crates/cang-guest-init/src/guest_init/components/nix/root.rs:297-315`) and nix-daemon writes
   `/nix/var/nix/{db,temproots,profiles,gcroots}` in the same tree (`nix_overlay.rs:337-370`,
   `:493-504`). A read-only export at `/nix` turns those into `EROFS`; at `/nix/store` it keeps
   `/nix` writable but also refuses the guest's own `nix build` writes, because the guest store *is*
   the host overlay upper. The host plan is the same either way — only `source` differs
   (`mergeddir` vs `mergeddir/store`).
4. **How the launch fails today when a device is missing.** Every libkrun call is wrapped by
   `setup(name, ..)` → `"libkrun setup failed: <name>"` (`launcher.rs:884-886`); a bad host path fails
   inside `fs_device_new` (`linked.rs:317-326` → `FsDevice::new` → `"fs device: {e:?}"`,
   `device_builders.rs:831-841`). Devices are added before `vmm_builder_payload`/`build`
   (`launcher.rs:521-528`), so a missing export aborts before the guest boots. The asymmetric risk is
   the other way: a device the guest never mounts is **silently invisible** — nothing host-side checks
   the guest mounted it — and a guest mount of an absent tag is `ENODEV` in the guest only.
5. **Overlay lifetime must bracket the VM.** The export's host path is the worker's overlay mount,
   materialised at `vm_child.rs:340-345`, i.e. before `prepared_root::prepare` (`:346-348`) and the
   sandboxed child (`:350`); teardown unmounts the prepared root first and the overlay second
   (`:351-357`, `:728-760`). Reversed, the merged dir stays busy (`nix_overlay.rs:214-221`).
6. **`xattr = false` needs a fork change first.** `Config.xattr` is written only inside
   `Fs::set_permission_semantics` (`deps/libkrun/src/devices/src/virtio/fs/device.rs:124-142`), which
   refuses `LinuxSimplified` on Linux (`:125-127`); `FsDevice` exposes permission semantics, cache
   policy and the two timeouts but no `set_xattr` (`device_builders.rs:670-826`). Goal (c) therefore
   needs `Fs::set_xattr` + `FsDevice::set_xattr`, a new `LibkrunApi` method with its `linked.rs`
   implementation and fake variant, regenerated bindings/`include/libkrun.h`, and a new fork tag
   before cang can call it. Assert afterwards: `ENOSYS` on `GETXATTR` stops the kernel forwarding
   xattr calls **for the whole mount** (`filesystem.rs:931-933`), and file capabilities stop being
   honoured there (note 40 §4, `notes/40-...:72-78`).
7. **Budget.** One extra MMIO IRQ: 19 available (`IRQ_BASE 5`..`IRQ_MAX 23`,
   `deps/libkrun/src/arch/src/x86_64/layout.rs:28-30`) against 7 used (note 40 §6.1,
   `notes/40-...:120-121`); one extra pool of `CANG_FS_WORKER_THREADS` threads (`lib.rs:39`).

## Ordered edit checklist

1. `crates/cang-libkrun/src/lib.rs` — `CANG_FS_NIX_TAG` + doc beside `CANG_FS_ROOT_TAG`; later
   `fs_device_set_xattr` on the trait (`src/api.rs:50-62`).
2. `crates/cang-libkrun/src/linked.rs:317-326` — `fs_device_set_xattr` impl (fork method first).
3. `deps/libkrun` (own change, own tag) — `Fs::set_xattr` in `devices/src/virtio/fs/device.rs`,
   `FsDevice::set_xattr` in `api/device_builders.rs`, bindings + `include/libkrun.h` + `CANG.md`.
4. `config/model.rs` — `NixExport`, `LaunchConfig.nix_export`, target constant.
5. `config/components/mounts.rs:10-30` — stop appending the `/nix` mount when the export owns it.
6. `config/mod.rs:29-161` `build_for_task` — build the export from `host_nix_overlay` (`mergeddir`,
   `read_only: true`, levers from the `cang-libkrun` defaults), thread the append decision, emit the
   guest env pair.
7. `config/codec.rs` — serialize, allowlist, `parse_nix_export`, parse, struct literal.
8. `runtime/vm/libkrun/launcher.rs` `configure` — between `configure_network` and
   `configure_balloon` (`:501-503`): create the device, apply the per-export setters, `add_device`;
   never `fs_device_set_overlay`.
9. `runtime/vm/libkrun/tests.rs` — fs-handle counter, expected vector, new ordering test;
   `launch/config/tests.rs:1000-1061` rewrite plus the round-trip test.
10. `cli/mod.rs` + `runtime/launch/plan.rs` — only for a `--fs=` switch: `FsOption` variants, parser,
    extractors, `RuntimeOptions`/`LaunchPlan` fields, then the 33 `LaunchSpec` literals.
11. Guest side (separate slice) — mount the tag before `components/nix/root.rs:297-315`, handle
    `ENODEV`, and settle the `/nix`-vs-`/nix/store` mountpoint from §5.3.
