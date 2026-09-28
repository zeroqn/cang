# 05 — Does the pinned libkrunfw boot libkrun 2.0.0?

Ticket `05-libkrunfw-compat-check`, map `docs/wayfinder/libkrun-main-rebase`.
Live leg run 2026-09-28, after tickets 09/10/13: cang 2.0.0-cang.2 pinned release.

## Live evidence

Shape: a btrfs loop image with a fresh graphroot (`localhost/cang:latest` loaded
into it), `btrfs-snapshot` task rootfs, `script -q -e -c … /dev/null`, cang built
by `nix build .#cang` - i.e. the **pinned** `libkrun` v2.0.0-cang.2 and the
**pinned** `libkrunfw` v5.6.2-cang.1 (the asset carries `libkrunfw.so.5.3.0`).

```
$ cang --mem 4 --seccomp=off --landlock=off -- bash -lc '…'
pid1_comm=init.krun            # PID 1 is the injected init blob
self_ppid=688                  # the workload is the blob's fork
kernel=6.12.109-hardened1
virtio-virtio0-id=0x0003       # console (the managed kernel console, hvc0)
virtio-virtio1-id=0x0003       # console (the worker-stdio console, hvc1)
virtio-virtio2-id=0x001a       # virtio-fs (the task rootfs)
virtio-virtio3-id=0x0002       # virtio-blk
virtio-virtio4-id=0x0013       # virtio-vsock
virtio-virtio5-id=0x0001       # virtio-net
/dev/hvc0  /dev/vda  /dev/vsock
/dev/zram0  partition  4027912  0  100     # ZRAM swap is up
status:
state=ready      /run/cang/nix-prep.status
state=running    /run/cang/podman-prep.status
```

## Verdict

**The pinned fw is fine.** The fw/kernel ABI half was settled by ticket 01
(`krunfw_get_kernel`, `KernelBundle` and `DEFAULT_KERNEL_CMDLINE` unchanged); the
live leg confirms it: the kernel unpacks, the hand-off works, PID 1 is the
injected init blob from `krun_init_config_apply_in`, and the guest's own
assumptions hold - `uname -r` is `6.12.109-hardened1`, the cang-guest-init status
files are written under `/run/cang` (`state=ready` for the nix prep, `running`
for the podman prep), and every virtio device cang configures appears (fs, blk,
net, vsock, console, plus the persistent-disk block device).

Two config deltas the fork carries are also live-verified rather than inferred
from the build: the guest has a working `/dev/zram0` swap partition (SWAP=y /
ZRAM=y), and the kernel console is the leading virtio console device (hvc0),
which is what the managed session's kernel-console-log design depends on.

**Not verified here:** the GPU device (this run was `GpuMode::Off`); venus/WebGL
is ticket 08's smoke. Nothing suggests the fw side is involved there.
