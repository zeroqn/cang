---
label: wayfinder:research
title: The 7.2.7 re-base - what dropped, what was re-authored, what runs
status: closed
blocked_by: []
claimed_by: pi session (2026-09-30)
---

## Question

`deps/libkrunfw` bundles a guest kernel (linux-6.12.109 + linux-hardened
v6.12.109-hardened1) with a 39-patch fork series. What does it take to move it to
linux-7.2.7 + linux-hardened **v7.2.7-hardened1**, and does the result boot?

## Answer

It builds and it boots. The work lives on the fork branch `rebase-7.2.7`
(`694c1f6`). The series shrinks to **23 patches**: nine are now upstream and were
dropped, seven needed re-authoring, and four arm64 patches were left behind.

### Method

The series is `git format-patch` output, so it was re-based rather than
hand-patched: a scratch repo took the pristine 6.12.109 tarball as one commit,
applied all 39 patches as `git am` commits, imported the pristine 7.2.7 tree as an
unrelated base commit, and replayed the series with
`git rebase --onto <7.2.7> <6.12.109> series`. Git's three-way merges resolved the
mechanical drift and stopped only on real conflicts; commits whose change was
already upstream came out empty and were skipped.

The kernel tree is then rebuilt from the fork checkout as usual: the Makefile
extracts the tarball, applies `patches/0*.patch` in order and then the hardened
patch, and `olddefconfig` refreshes the config. All 23 patches and the hardened
patch apply cleanly to a pristine 7.2.7 tree in that order.

### Dropped because 7.2.7 already has it

| fork patch | why it is gone |
| --- | --- |
| `vsock/dgram: generalize recvmsg...`, `vsock: refactor transport lookup`, `vsock: support multi-transport datagrams`, `vsock: make vsock bind reusable`, `virtio/vsock: support dgrams`, `vsock: Add SIOCINQ ioctl`, `virtio/vsock: implement has_data for DGRAM` | the merged dgram support is newer than the fork's pre-merge shape of the same API (`dgram_allow()` kept its `vsk`, no `dgram_get_*`/`dgram_payload_offset` hooks); `SIOCINQ` is in 7.2.7's `af_vsock.c` |
| `can: virtio: Add virtio CAN driver` | `drivers/net/can/virtio_can.c` is upstream |
| `virtio_rtc: *` (5 patches) | `drivers/virtio/virtio_rtc_*.c` is upstream, so `CONFIG_VIRTIO_RTC=y` now selects the upstream driver |
| `virtgpu: gem partial map` | 7.2.7's `virtio_gpu_vram_mmap()` already validates `vm_pgoff` + size against the node |
| `drm/virtio: import scanout buffers from other devices` | upstream since v6.14 (`25c3fd1183c0`, `2885e575abc7`, `ca77f27a2665`) - the patch's own header said so; the fork's `CREATE_GUEST_HANDLE` patches now sit on upstream's code instead |

### Re-authored for 7.x

- **TSI** (`net/tsi/af_tsi.c`): `proto_ops.bind`/`.connect` take
  `struct sockaddr_unsized *` since 7.x (`.getname` did **not** change - blanket
  casts break it), and `__udp4_lib_lookup`/`__udp6_lib_lookup` lost the
  `udp_table` argument (8 arguments now, the net's default table is used).
- **drm/virtio fence passing - the bit-5 collision.** Upstream gave bit 5 to
  `VIRTIO_GPU_F_BLOB_ALIGNMENT`; the fork and the libkrun host use bit 5 as
  `FENCE_PASSING`/`RESOURCE_SYNC`. With both readings live, `virtio_has_feature()`
  set `has_blob_alignment`, `vgdev->blob_alignment` stayed zero because the
  libkrun host has no such config field, and the create-blob ioctl's
  `IS_ALIGNED(size, 0)` check **rejected every blob** - a fatal GPU break, not a
  cosmetic clash. The driver no longer reads the bit as `BLOB_ALIGNMENT`
  (`virtgpu_kms.c`), so bit 5 means fence passing only.
- **`CREATE_GUEST_HANDLE` / `BLOB_CTX_ID_FIX`** (5 commits in one patch file):
  re-applied on upstream's scanout-import code; bits 6 and 7 are free in 7.2.7.
- **virtio-media**: `v4l2_fh_add()`/`v4l2_fh_del()` take the `struct file *`
  since 7.x (the session helper already carried it in `session->file`).
- Trivial drift: `MAINTAINERS`, `virtio_ids.h` (`VIRTIO_ID_MEDIA 48` is free),
  `net/Makefile` (`CONFIG_NET_SHAPER` arrived), selinux `classmap.h`
  (`PF_MAX > 49` now that TSI adds 46-48).

### Left behind

The four arm64 patches (SCOPE_LOCAL_CPU early/late, arm64
`PR_{GET,SET}_MEM_MODEL`, ACTLR_EL1 threading, Apple IMPDEF TSO) are not ported:
none of them affect an x86_64 guest. The generic half
(`include/linux/memory_ordering_model.h`, the `PR_{GET,SET}_MEM_MODEL` prctl
plumbing) is kept, so the arm64 work is "re-apply four small patches", not "redo
the value thread". `config-libkrunfw_x86_64-kvm-lto`, the config behind the
*released* `libkrunfw-x86_64-kvm-lto.tgz`, was already stale at 6.12.91 and is
not refreshed here.

### Evidence

Build (gcc 15.3.0, `-kvm` config, 28 cores; the kernel and the `libkrunfw.so`
both build):

```
make                      # in a fork checkout with the rebased patches
  ...  Kernel: arch/x86/boot/bzImage is ready  (#1)
  Generating kernel.c from linux-7.2.7/vmlinux...
  cc -fPIC -DABI_VERSION=5 -shared -Wl,-soname,libkrunfw.so.5 -o libkrunfw.so.5.6.2 kernel.c
```

The refreshed `config-libkrunfw_x86_64-kvm` keeps every option cang cares about:
`CONFIG_UDMABUF=y`, `CONFIG_SECURITY_LANDLOCK=y`, `CONFIG_DRM_VIRTIO_GPU=y`,
`CONFIG_FUSE_DAX=y`, `CONFIG_NFT_TPROXY=y`, `CONFIG_ZRAM=y`, `CONFIG_KVM=y`,
`CONFIG_NR_CPUS=48`.

Live boot: the harness at `/home/dev/cang/disk/kernel727-live/` runs a stock
`cang 0.10.1` (`--mem 4 --seccomp=off --landlock=off`) with
`CANG_LIBKRUNFW_LIBRARY` pointing at the freshly built firmware
(sha256 `5f4b05ed8f7a99ce8d0d8f24243286a2946cec7ca4d5924285bd48790d06f751`,
24,119,080 bytes), and the guest reports:

```
# uname: Linux localhost 7.2.7-hardened1 #1 SMP PREEMPT_DYNAMIC Mon Sep 21 20:29:27 CEST 2026 x86_64 GNU/Linux
# proc-version: Linux version 7.2.7-hardened1 (root@libkrunfw) (gcc (GCC) 15.3.0 ...) #1
# nproc: 26
# mem: Mem:  3933  233  3741
# vsock: crw------- 1 root root 10, 259 ... /dev/vsock
# hvc0: crw------- 1 root root 229, 0 ... /dev/hvc0
# protocols-tsi: 3          # TSI is registered in /proc/net/protocols
# protocols-vsock: 1
# root-fstype: virtiofs
# zram: /dev/zram0  partition  4027576  0  100
=== probe done ===        # vm-exit=0
```

### Reproducing a build from a local fork branch

`nix/pkgs/libkrunfw.nix` holds the kernel tarball and hardened-patch hashes; the
7.2.7 pair is

```
linux-7.2.7.tar.xz                        sha256-SsNMR9slQP+ycTlD+NiR/xcC4LppNFJaSTt9HK1DFFo=
linux-hardened-v7.2.7-hardened1.patch     sha256-6AZcuBsr6Ax26Z0sdBUhYqwo8cndegrq+W7Jwt1x2MI=
```

so `nix build .#cang-dev --override-input libkrunfw-src path:<fork checkout on
rebase-7.2.7>` builds the 7.2.7 kernel plus cang against it.
