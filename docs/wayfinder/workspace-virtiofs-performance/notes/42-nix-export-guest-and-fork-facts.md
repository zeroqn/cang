---
label: wayfinder:research
title: The guest side of a second read-only /nix virtio-fs export, and what the fork can express
status: open
blocked_by: []
claimed_by: pi research child (2026-11-03)
---

# 42 - A second `/nix` fs device: the guest half, and what the server can express

Read-only: nothing here changed the tree, ran a build or booted a VM. Citations are
code at fork `deps/libkrun` @ `3c2eb13b` (`v2.0.0-cang.16`) or an earlier note.

## 1. How a guest mounts fs devices today

**The kernel mounts the root share; nothing in the guest does.** The cmdline is
`... console=hvc0 rootfstype=virtiofs rw quiet no-kvmapf` with no `root=`
(`deps/libkrun/src/libkrun/src/vmm/vmm_config/kernel_cmdline.rs:5-6`), so the root
mount's source is the literal string `/dev/root`, which matches only because cang
registers its one device with that tag (`crates/cang-libkrun/src/lib.rs:25`; call
site `crates/cang/src/runtime/vm/libkrun/launcher.rs:419`). Note 05 §1.1
(`notes/05-dax-window-viability.md:114`) traces that fallback through
`init/do_mounts.c` and concludes "the tag is not a free parameter".

**`init.krun` mounts nothing virtiofs.** The embedded init is PID 1
(`init/init-blob/src/config.rs:50,53,132`); it mounts devtmpfs, proc, sysfs,
cgroup2, devpts and tmpfs only (`init/init-binary/src/fs.rs:25-77`), picks one
tmpfs out of the OCI mount list (`init/init-binary/src/config.rs:122-131`), sets
`MS_SHARED` on `/` (`fs.rs:184-192`) and `execvp`s cang's argv
(`init/init-binary/src/exec.rs:82-145`). No `mount -t virtiofs`, no fstab, no tag
list anywhere under `deps/libkrun/init/`.

**`cang-guest-init` mounts no virtiofs tag either**: `/tmp` tmpfs
(`runtime/cang.rs:538,573`), and - only in the disk-backed mode - the `/nix` btrfs
disk then overlayfs (`components/nix/root.rs:197-220`). `grep -rn virtiofs
crates/` finds no tag mount.

**Grafts inside the one export.** `/workspace`, `/nix`, `~/.cargo`, ... are host
bind grafts under the prepared root
(`crates/cang/src/runtime/vm/prepared_root/mod.rs:105-140`) and reach the guest as
`FUSE_ATTR_SUBMOUNT` sub-volumes - `SUBMOUNTS` is claimed when the client offers it
(`.../virtio/fs/linux/passthrough.rs:1111-1114`) and fills `attr_flags`
(`:312-321`, `:1179-1182`, `:1216-1219`) - hence every lever is connection-wide
(MAP.md:335-339).

**A second tag needs a second MMIO device plus an explicit guest mount.** One MMIO
transport per device, one IRQ from a shared allocator
(`src/libkrun/src/vmm/device_manager/kvm/mmio.rs:128-166`, `.../irq.rs:33-58`), and
on x86 each device is announced as a `virtio_mmio.device=1K@0x...:<irq>` cmdline
token (`kvm/mmio.rs:170-189`); the tag travels in the device config word
(`devices/src/virtio/fs/device.rs:30-33,82-83`). Nothing new is needed for the
device to exist; what is missing is the guest `mount`.

## 2. Guest-init insertion point and ordering

`enter` (`crates/cang-guest-init/src/guest_init/runtime/cang.rs:120-330`) is the
only foreground sequence, and it is itself exec'd from a store path
(`exec_path = /nix/store/<hash>-cang/bin/cang-guest-init`,
`crates/cang/src/runtime/launch/config/tests.rs:62`). Order, with `/nix` readers
marked: `read-env` (:124) -> `validate-prepared-root` (:127, fails when a graft is
missing) -> `ensure-tmp-tmpfs` (:131) -> swap/userns/tun/loopback (:132-163) ->
`resolve-identity` (:165) -> **`prepare-wrappers` (:175, copies helper binaries)**
-> shell env (:190-201) -> **`build-opengl-driver-farm` (:203, symlinks to store
paths)** -> **`materialize-home` (:215, `components/home/root.rs:49`)** ->
**`materialize-allocator-preload` (:218, `CANG_MIMALLOC_LIB` etc.)** ->
dmesg/io_uring/perf/passt-DNS (:223-258) -> `start-nix-prep` (:260, **background
child**) -> `start-podman-prep` (:263) -> `export-nix-remote` (:269) ->
wayland/no-file floor (:276-283) -> pulse/waypipe/exec, then managed session or
`drop_to_identity_and_exec` (:285-330).

The `/nix` mount is *not* in that foreground path. `start_background_prep`
(`components/nix/root.rs:89-127`) spawns `internal nix prep`
(`run_prep_to_status`, `:129-156`), whose work is `run_prep` (`:158-236`):
`mount-disk` (`:197-206`) -> `preseed_upper`, a `cp -a` of the lower `/nix/var`
(`:356-432`) -> `mount-overlay` with `lowerdir=/nix`,
`upperdir=<disk>/upper`, `userxattr` (`:212-220`, `:288-295`) -> `repair-store`
(`:244-278`) -> socket dir (`:221-223`) -> `nix-daemon` (`:225`).

In the **shipped** mode none of that runs: `run_prep` short-circuits to
`run_host_overlay_prep` (`:297-320`), because normal launches attach no `/nix`
disk (`crates/cang/src/runtime/launch/components/persistent_disks/nix.rs:1-5`).
`/nix` is then the host's overlay `merged` dir, grafted into the prepared root
(`crates/cang/src/runtime/session/nix_overlay.rs`, `prepare_and_mount` +
`overlay_options`; graft added by `components/mounts.rs:26-30` and applied at
`prepared_root/mod.rs:119-132`), and the guest only runs `repair-store` and starts
the daemon on it.

So the second mount point cannot be `/nix` in either mode: in host-overlay mode
`/nix` already is the graft (and the writable merged dir), and in disk mode `/nix`
is the overlay's own mountpoint whose lowerdir is the string `/nix`. A new mount
point (e.g. `/run/cang/nix-lower`) plus a repointed graft/lowerdir is required,
and every foreground `/nix` reader above must run after that mount.

**Failure reporting.** A foreground failure (`command::run` builds
"`<program> exited with status …`", `command.rs:38-47`) propagates through `enter`
to `entrypoint`, which prints `cang-guest-init: {err:#}` and exits 1
(`crates/cang-guest-init/src/lib.rs:17-24`): the workload never starts and the code
reaches the host via init's `KRUN_EXIT_CODE_IOCTL` (`exec.rs:67-79`). There is **no
timeout in `enter`** - a slow device simply blocks the mount syscall. A *background*
prep failure is different and is the existing precedent for "never half-mount
`/nix`": it is recorded as `state=failed` with `error=`/`log_path=` in
`/run/cang/nix-prep.status` (`root.rs:129-156`, `status.rs:9-24`) and every waiter
(`components/nix/user.rs:8-70`) bails with a deterministic message instead of using
a partly-prepared tree.

## 3. Read-only: who enforces it and what the guest sees

Server-side, in the fork. `read_only` is the `Fs::new` flag after `shared_dir`
and `exit_code` (`devices/src/virtio/fs/device.rs:67-104,100-101`) and selects a whole wrapper filesystem
(`.../fs/worker.rs:394-397`). `PassthroughFsRo` `-EROFS`es every mutating op -
`setattr`, `symlink`, `mknod`, `mkdir`, `unlink`, `rmdir`, `rename`, `link`,
`create`, `write`, `fallocate`, `setxattr`, `removexattr`, `copyfilerange`
(`.../fs/read_only.rs:338-484`), write/truncate/`O_TMPFILE` opens (`:37-58`),
`access(W_OK)` (`:247-256`) and `opendir` on `O_WRONLY` (`:182-195`) - and strips
`WRITEBACK_CACHE` from the INIT reply (`:81-86`). The guest sees `EROFS`
(`devices/src/virtio/linux_errno.rs:278`). cang already plumbs the flag
(`crates/cang-libkrun/src/api.rs:50`).

**What still writes `/nix` in the guest**, all of it in the shipped host-overlay
mode: `nix-daemon` (`root.rs:312`), the daemon-socket dir and URI (`root.rs:307`,
`components/env.rs:14-15`), `cang-nix-store-repair` - which copies objects into
`/nix/store` and deletes `ValidPaths`/`Refs` rows (`root.rs:244-278`,
`nix/image/nix-store-repair.nix`) - and, in disk mode, `preseed_upper`'s `cp -a`
plus `chown :nixbld upper/store` (`root.rs:356-432`, `:434-440`). Consequence,
stated as a fact: a read-only *export* makes the image layer read-only only if a
writable layer sits on top of it. In host-overlay mode the export would be the
whole of `/nix`: `nix-daemon` (store additions, DB, temproots), `cang-nix-store-repair`
(object copies into `/nix/store`) and the daemon socket (created when absent,
`root.rs:307`) all need write access, so the store share can only be read-only if
a writable layer exists above it.

## 4. `Config.xattr`

`Config.xattr` (field default `false`, `linux/passthrough.rs:437-442`) gates all
four handlers: `setxattr`/`getxattr`/`listxattr`/`removexattr` answer `-ENOSYS`
when false (`linux/passthrough.rs:2035-2036,2083-2084,2130-2131,2171-2172`). The
guest loses `security.capability` - the only xattr name any cang workload asks for
(note 40 §4, `notes/40-cache-symlinks-shipped.md:72-78`) - and with it file
capability honouring; `-ENOSYS` is the protocol's "not supported", after which the
client stops asking (note 40:76; note 01:610-616). The exact userspace errno for
`getxattr` on such a share is **not determinable from this repo** (no kernel tree
is vendored); the repo only records the request disappearing.

**It cannot be set today.** `Fs::set_permission_semantics` writes `cfg.xattr =
complete` (`device.rs:124-140`), `FsDevice::new_inner` always passes
`LinuxComplete` (`src/libkrun/src/api/device_builders.rs:831-840`), and the only
other value is refused on Linux - `if cfg!(target_os = "linux") &&
matches!(semantics, LinuxSimplified) { return false }` (`device.rs:124-127`),
surfaced as `InvalidParam` (`device_builders.rs:695-707`). No `set_*` on `FsDevice`
touches xattr (`device_builders.rs:706-820`), and `LinuxSimplified` also zeroes
`attr_timeout` (`device.rs:130-138`) - the opposite of what a store wants. Item (c)
of the plan needs a new fork setter.

## 5. DAX

`set_dax_window_size` is per device (`device_builders.rs:659-667` -> `shm_size` ->
`DeviceRequirements` -> one `ShmManager` region, note 05 §2.1); the mode is not a
server field at all but the guest mount option `dax=`, consumed before `FUSE_INIT`
(note 05 §1.2). Note 05 dropped it, verbatim: "DAX is selected per connection
(`virtio_fs.c:104-145`, `:1596-1603`, no `.reconfigure` at `:1761-1765`) and cang
has one connection, which is `/`" (`notes/05-dax-window-viability.md:439-441`);
"**No fallback.** Exhaustion is a wait on `range_waitq` …, not a downgrade to the
normal read path" (`:449-451`); "**Boot-bricking failure modes.** `dax=always`
without a window panics at root mount …, a window that is not a multiple of the
guest's memory-block size fails the device probe … and the guest cannot mount its
root at all" (`:452-455`). Note 40 already ruled the scoped variant: "DAX stays
out: … a `/nix` export carries the same boot binaries, so the risk does not shrink
just because the export does" (`notes/40-cache-symlinks-shipped.md:122-124`).

What a second device *does* change, as a mechanism fact only: `rootflags=`
addresses the root mount (note 05 §1.2), so a second tag is the one case where
`dax=` can arrive as a mount option without touching the boot cmdline, and
`virtio_fs_test_super`'s same-device match (note 05:128-135) does not apply to a
different tag. Unchanged: the probe/alignment/panic constraints above, and note 05
§3.2 - every `execve` of a `/nix` binary maps an `S_DAX` inode, so the tool chain's
text faults land on the share that carries the boot binaries.

## 6. Worker pool, queue and IRQ

Each device owns its `Fs` and its `FsWorker`: `worker_threads` is a device field
(`device.rs:61,189`), the pool is started per worker
(`.../fs/worker.rs:443-467`), and its size is
`requested.clamp(1, available_parallelism)` (`worker.rs:55-60`) - **clamped per
device, not globally**, so two devices at cang's `CANG_FS_WORKER_THREADS = 4`
(`crates/cang-libkrun/src/lib.rs:39`) run eight pool threads on an eight-core host.
Each device also gets its own MMIO transport, its own single request queue
(virtio-fs has no multiqueue; `NUM_QUEUES = 2`, `devices/src/virtio/fs/mod.rs:95-97`;
note 01:618-627) and its own IRQ from `IRQ_BASE 5 ..= IRQ_MAX 23`
(`src/arch/src/x86_64/layout.rs:26-30`). Budget: 19 lines, 7 used in a real boot
(note 40:120-122) - so one IRQ and one 4 KiB window (`MMIO_LEN = 0x1000`,
`kvm/mmio.rs:86`). Registration order is load-bearing ("the MMIO device manager
numbers devices (and their IRQs) in registration order, and the guest's boot
contract already depends on that order", `launcher.rs:848-851`), so the second fs
device goes after the root device (added at `launcher.rs:483`) and away from the
console/balloon slots. What the *guest* does with two virtiofs devices
(per-connection `fuse_conn`, per-queue kworkers, independent congestion/`max_background`
limits, page cache per superblock) is not determinable from this repo.

## 7. Unknowns, each with the probe that settles it

1. **Does the guest mount a second tag the way cang wants?** Probe: in one guest,
   `mount -t virtiofs <tag> /run/cang/nix-lower -o ro`, then `cat /proc/mounts` and
   `findmnt -T /run/cang/nix-lower`.
2. **Do file capabilities survive an `-ENOSYS` store share, and what errno does
   `getxattr` return?** Probe: `python3 -c 'os.getxattr(p,"security.capability")'`
   on a capped file under the new mount, then `execve` it and read `/proc/self/status`.
3. **Are the two connections actually independent under load?** Probe: note 20's
   scan probe on `/workspace` concurrent with a store scan, `CANG_FS_STATS=1`, and
   compare the per-device histograms (`device.rs:303`).
4. **Does the per-device clamp bite?** Probe: on the host during a two-device boot,
   count `fs pool` threads in `/proc/<vmm-pid>/task/*/comm` against `nproc`.
5. **Absent/slow device behaviour.** Probe: boot once with the device unregistered
   (record mount errno, guest-init stderr line, guest exit code) and once with its
   `INIT` delayed (time the boot) - `enter` has no timeout.

## Ordered code edits

1. `deps/libkrun/src/devices/src/virtio/fs/device.rs` - `Fs::set_xattr(&mut self,
   bool)`: write `cfg.xattr` without touching `attr_timeout` (`:124-140` is the only
   writer today).
2. `deps/libkrun/src/libkrun/src/api/device_builders.rs` - `FsDevice::set_xattr`
   beside the other setters (`:706-820`); regenerate the FFI bindings.
3. `crates/cang-libkrun/src/api.rs` - `LibkrunApi::fs_device_set_xattr` (`:50-62`);
   `crates/cang-libkrun/src/linked.rs` - the real implementation.
4. `crates/cang/src/runtime/vm/libkrun/launcher.rs` - `configure`: a second
   `fs_device_new(NIX_FS_TAG, &nix_path, true)` with its own
   `set_xattr(false)`, `set_worker_threads`, `set_zero_message_open`,
   `set_no_flush`, `set_negative_entry`, `set_cache_symlinks`, `add_device` after
   the root device (`:411-483`).
5. `crates/cang/src/runtime/launch/config/components/mounts.rs` -
   `mounts_with_host_nix_overlay`: stop adding `/nix` as a `BindMount` (`:26-30`).
6. `crates/cang/src/runtime/vm/prepared_root/mod.rs` -
   `PreparedRootPlan::new_with_runtime_etc` (`:105-140`): no `/nix` graft, so the
   root export no longer carries the store as a sub-volume.
7. `crates/cang-guest-init/src/guest_init/runtime/cang.rs` - `enter`: a new
   `mount-nix-export` step after `validate-prepared-root` (`:127`) and before
   `prepare-wrappers` (`:175`), so every `/nix` reader in §2 still resolves; add it
   to `planned_enter_operations` (`:62-105`) and its ordering tests (`:607-625`).
8. `crates/cang-guest-init/src/guest_init/components/nix/root.rs` - `run_prep`
   (`:158-236`) and `run_host_overlay_prep` (`:297-320`): point the disk-mode
   `lowerdir` at the new mount point rather than `/nix`, and extend the idempotent
   `findmnt` check (`:472-474`) to cover it.
9. `crates/cang-guest-init/src/guest_init/components/fdwatch.rs` -
   `probe_virtiofs` (`:301-310`): probe the new mount as the store target.
