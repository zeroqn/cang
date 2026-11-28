# Internals

Host-side lookup order, prepared-root grafting, guest memory and huge pages, and
the guest entry contract.

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
bind-mounted view of the task rootfs with the workspace, tool state, Cargo
(which also carries the `kache` compiler cache), and host-prepared `/nix`
overlay directories grafted into their final guest paths before
`krun_set_root`. Cang
intentionally does not register one `krun_add_virtiofs3` device per developer
path; keeping those binds inside the root export avoids the legacy x86
IRQ/device exhaustion that can otherwise occur before libkrun's implicit vsock
device is registered.

## Guest memory and huge pages

Both halves of the huge-page path are on, and they are independent:

- **Guest side** - the pinned libkrunfw kernel is built with
  `CONFIG_TRANSPARENT_HUGEPAGE=y` and, like the upstream defconfig, a
  `CONFIG_TRANSPARENT_HUGEPAGE_MADVISE=y` default, which nobody in the guest
  ever opts in to. Every launch therefore appends `transparent_hugepage=always`
  with `krun_payload_append_cmdline` (`GUEST_KERNEL_CMDLINE_APPEND` in
  `crates/cang/src/runtime/vm/libkrun/launcher.rs`), so a booted guest reports
  `enabled: [always] madvise never`. A workload that prefers the old behaviour
  can still `madvise(MADV_NOHUGEPAGE)` its own ranges. `defrag` stays at
  `[madvise]`, and the kernel's tmpfs/shmem knobs stay `[never]`.
- **Host side** - guest RAM is one anonymous mapping (memfd-backed under
  `--gpu=drm`/`--zero-copy-shm`), and libkrun's `create_guest_memory` now
  `madvise(MADV_HUGEPAGE)`s every region before the payload and the VM are set
  up, so a host with the distro default `THP=madvise` still hands KVM 2 MiB
  pages and 2 MiB EPT entries. The advice is a hint and never fatal: a host at
  `THP=never` ignores it, and a memfd region ignores it while shmem THP is off.

Measured in a cang guest (534-crate offline `cargo build`, and a 2 GiB
random-access pointer chase), arms alternating so host drift hits both:

| change | compile wall clock | 2 GiB random access | host `AnonHugePages` |
| --- | --- | --- | --- |
| guest `transparent_hugepage=always` | 0.8% apart, sign flips between pairs - noise | 133.2 -> 125.3 ns (-6.0%), consistent | n/a (guest-side) |
| host `MADV_HUGEPAGE` on guest RAM | 79.7 -> 65.8 s (-17%), consistent across 3 pairs with the arm order flipped | sign flips - inconclusive | 0 -> ~3.9 GiB at unchanged RSS |

So the guest-side switch alone buys latency on large-heap random access but not
compile throughput, and the host-side advice is what moves a compile: fewer host
page faults and 2 MiB EPT entries. Both cost RSS granularity (2 MiB per touched
area) rather than CPU, which matters because cang sizes guest RAM at 80% of the
host. The virtio-balloon's free-page reporting `madvise(MADV_DONTNEED)`s the
ranges the guest hands back, which splits the huge pages covering them again.

## Guest clock

cang attaches the virtio-rtc device on every launch: libkrun's own in-process
port of rust-vmm's `vhost-device-rtc`, device ID 17, with its two queues of
1024. It is not a vhost-user backend and it asks for no shared memory, so it
costs the guest an MMIO slot and nothing else.

The pinned kernel carries the driver (`CONFIG_VIRTIO_RTC`,
`CONFIG_VIRTIO_RTC_PTP`, `CONFIG_VIRTIO_RTC_CLASS` in the x86_64 libkrunfw
config), so a booted guest has:

- `/dev/rtc0` (char `251:0`) - the UTC (`CLOCK_REALTIME`) clock, with the wire
  protocol's alarm queue behind `wakealarm`, and the kernel's own boot lines
  registering it and setting the system clock from it;
- three PTP clocks - UTC, TAI and monotonic - as `/dev/ptp0`, `/dev/ptp1` and
  `/dev/ptp2` (`cat /sys/class/ptp/ptp*/clock_name` reads e.g.
  `Virtio PTP type 1/variant 0`).

A guest booted from this tree reports exactly that: `virtio_rtc virtio6` as
`/sys/class/rtc/rtc0/name`, the three `ptp*` clocks, and `since_epoch` matching
the host's `date +%s`, with the packaged seccomp policy enforced.

What the guest previously had was kvmclock alone: an x86 microVM has no CMOS
RTC, and libkrun registers a legacy PL031 RTC only on aarch64/riscv64 (a
`RtcDevice` replaces it instead of joining it). kvmclock gives a wall clock
anchored at boot and frequencies from the host TSC; the RTC is what answers
`hwclock`, the TAI clock, and a settable alarm.

Two consequences worth knowing when changing launch code:

- The device is constructed inside the VM worker *after* its seccomp policy is
  applied, and it creates the alarm `timerfd` and asks `clock_getres` for each
  clock (TAI is not a vDSO clock, so that is a real syscall). The packaged
  `assets/seccomp/default.json` therefore names `clock_getres`,
  `timerfd_create` and `timerfd_settime`; removing them turns every launch into
  a SIGSYS in the worker.
- It is attached before the balloon, which stays the last device registered
  (`configure_balloon` in `crates/cang/src/runtime/vm/libkrun/launcher.rs`):
  the MMIO device manager numbers slots in registration order, and
  `console=hvc0` plus the tagged root filesystem depend on that order.

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
