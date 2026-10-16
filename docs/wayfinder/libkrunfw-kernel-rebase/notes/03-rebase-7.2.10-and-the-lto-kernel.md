---
label: wayfinder:research
title: The 7.2.10 follow-up - the upstream revert that landed, and the pinned LTO kernel
status: closed
blocked_by: []
claimed_by: pi session (2026-10-16)
---

## Question

The `cang` kernel line landed on linux-7.2.7 + linux-hardened v7.2.7-hardened1
([notes/01](01-rebase-findings.md), [notes/02](02-two-kernel-lines-and-lto.md)).
linux-hardened has since released **v7.2.10-hardened1**: what does moving the
line one patch level take, does the fork's `CREATE_GUEST_HANDLE`/PRIME-import
work survive, and does the kernel the *pin* carries still drive the GPU and
VA-API paths?

## Answer

It is a patch-level bump, not a lineage change: the series needs one re-authored
hunk, the four x86_64 configs move by a handful of lines, and the released
`-kvm-lto` firmware passes both live smokes with the baseline's numbers to the
digit. Released as the fork's **`v5.6.2-cang.4`** at `901412d`; the cang side
pins it.

### What the series needed: `1e3b08de` came back

22 of the 23 patches applied to a pristine 7.2.10 tree untouched. The one that
did not is `0023-drm-virtio-gate-the-guest-blob-PRIME-import-on-the-n.patch`,
whose third hunk patched the bare

```c
if (!vgdev->has_resource_blob)
    return drm_gem_prime_import(dev, buf);
```

that 7.2.7 had. linux-7.2.9 backported upstream's revert of the very commit the
fork's series builds on - `1e3b08de` ("Revert `drm/virtio: Allow importing prime
buffers when 3D is enabled`", stable `bed76f2c0f9e`, Fixes `df4dc947c46b`), for
the mouse-cursor regression in
[virglrenderer#674](https://gitlab.freedesktop.org/virgl/virglrenderer/-/work_items/674) -
so on 7.2.10 that line already reads `if (!vgdev->has_resource_blob ||
vgdev->has_virgl_3d)`. The hunk now replaces the upstream form, and the line the
kernel ends up with is byte-for-byte the one 7.2.7 produced:

```c
if (!vgdev->has_resource_blob ||
    (vgdev->has_virgl_3d &&
     !(vgdev->has_create_guest_handle && vgdev->has_blob_ctx_id_fix)))
```

Which kernel is which matters here: anthraxx cuts a hardened release only after
the matching stable release, so `v7.2.10-hardened1` is **7.2.10** (it contains
`1e3b08de`) - not 7.2.7 with a different label. Verified in the compiled tree,
not just the patch: `linux-7.2.10-kvm-lto/drivers/gpu/drm/virtio/virtgpu_prime.c`
line 342 carries the three-condition gate.

The rest is upstream work the line now picks up on the code the fork touches: a
NULL `virtio_gpu_fence_alloc()` dereference in `virtio_gpu_init_submit()`, a
fence event leaked on execbuffer failure, and `drm_gem_object_release()` on
`virtio_gpu_vram_create()`'s error paths.

Re-checked the two 7.2.7 traps (they are the ones that cost real time before):

- **Feature bits still do not collide.** 7.2.8/9/10 add no virtio-gpu feature
  bit: 5 is still upstream's `BLOB_ALIGNMENT`, 6/7 stay the fork's
  `CREATE_GUEST_HANDLE`/`BLOB_CTX_ID_FIX`, and `has_blob_alignment` is still
  never set, so the fork's `FENCE_PASSING` reading of bit 5 is intact.
- **No new drift under the patches.** The series applied with no fuzz and no
  rejects on the pristine tree (`patch --dry-run` per patch, checked before the
  build).

### The four x86_64 configs

`olddefconfig` against the re-based tree, one run per config with the compiler
that build uses - gcc 15.3.0 for the plain and `-kvm` configs, clang 21.1.8 with
`LLVM=1` for the two ThinLTO ones. The whole churn is

```diff
-# Linux/x86 7.2.7-hardened1 Kernel Configuration
+# Linux/x86 7.2.10-hardened1 Kernel Configuration
+CONFIG_CC_OPT_INLINE_MEMSET="-finline-stringops=memset"   (gcc configs only)
-CONFIG_OPENSSL_SUPPORTS_ML_DSA=y                           (dropped by 7.2.10)
```

Every option cang depends on survives in all four (`CONFIG_UDMABUF`,
`CONFIG_SECURITY_LANDLOCK`, `CONFIG_DRM_VIRTIO_GPU`, `CONFIG_FUSE_DAX`,
`CONFIG_NFT_TPROXY`, `CONFIG_ZRAM`, `CONFIG_KVM`, `CONFIG_NR_CPUS=48`), and the
LTO configs keep `CONFIG_LTO_CLANG_THIN`.

### Building `-kvm-lto` locally: two traps

`MakefileLto` is the release's LTO recipe, and building it outside CI needs two
fixes:

- **unwrapped clang** (documented in notes/02): nix's wrapped clang feeds its own
  `--target`/`-nostdlibinc` into the kernel's flags and the final `-shared` link
  cannot find `crtn.o`. Use `nixpkgs#llvmPackages.clang-unwrapped` +
  `nixpkgs#llvmPackages.bintools-unwrapped` and
  `'CLANG=clang -nostartfiles' HOSTCC=gcc HOSTCXX=g++`.
- **libelf for objtool**: a `nix shell` puts only `bin` on `PATH`, so the kernel's
  `tools/objtool` dies with `fatal error: gelf.h: No such file or directory`.
  Point `CPATH` at `nixpkgs#elfutils.dev`'s `include` and `LIBRARY_PATH` at
  elfutils' `lib` output.
- **the linker's library search path**: the unwrapped clang has no configured GCC
  installation, so the final `-shared` link fails with `ld.lld: error: unable to
  find library -lgcc / -lgcc_s / -lc`. Add `-L` (or `LIBRARY_PATH`) entries for
  `dirname "$(gcc -print-file-name=libgcc.a)"`,
  `dirname "$(gcc -print-file-name=libgcc_s.so)"` and
  `dirname "$(gcc -print-file-name=libc.so)"` - the wrapped gcc on `PATH` knows
  these; clang does not.
- **pyelftools for `bin2cbundle.py`**: `MakefileLto:71` runs
  `python3 bin2cbundle.py`, which imports `elftools`. A `nix shell` with both
  `python3` and `python3Packages.pyelftools` does *not* put the module on the
  path; set `PYTHONPATH` to pyelftools' `lib/python3.*/site-packages`.

All four are devshell artefacts of building outside CI (`nix develop` supplies
them, which is why the 7.2.7 notes only record the clang wrap). Also copy the
kernel tarball into `tarballs/` and the hardened patch yourself, or
`MakefileLto` re-downloads 153 MB from cdn.kernel.org.

The resulting local artifact, used for the LTO arms below:

```
libkrunfw.so.5.6.2  sha256 3363592037a0d6f731655b63365c68dbc804484efd52eb6135f5f5e37698a852
                    24576952 bytes, clang 21.1.8 + lld, "Linux version 7.2.10-hardened1"
```

### Live verification

The arms are serialized and differ in exactly one variable; every "kernel" line
below is read from the run's *own* `vaapi-diag.txt`/`gpu-diag.txt`, whose mtime is
after that run started.

| arm | cang | firmware | guest kernel (from the run's evidence) |
|---|---|---|---|
| baseline | `.#cang` (pin `v5.6.2-cang.3`) | pinned prebuilt, 7.2.7-hardened1 | `uname: Linux localhost 7.2.7-hardened1 #1 SMP PREEMPT_DYNAMIC ... x86_64 GNU/Linux` |
| new (VA-API) | `.#cang-dev` | locally built 7.2.10-hardened1 | `uname: Linux localhost 7.2.10-hardened1 #1 SMP PREEMPT_DYNAMIC ... x86_64 GNU/Linux` |
| new (Chromium/venus) | `.#cang-dev` | same | same |

`tools/virglrenderer-cang-smoke` (`--skip-chromium`), both kernels:

```text
PASS  vaapi-vm-exit / vaapi-guest-driver / vaapi-profiles / vaapi-steps
PASS  vaapi-frames      (both hwframe decodes reported exactly 60 decoded frames)
PASS  vaapi-first-gop   (hardware decode equals software decode, first GOP, every plane)
PASS  vaapi-frame-bytes (20736000 bytes = 60 x 640x360 nv12)
PASS  vaapi-psnr-decode     y:46.633758 u:44.227307 v:44.408519
PASS  vaapi-psnr-encode     y:46.537574 u:43.627419 v:42.909682
PASS  vaapi-psnr-roundtrip  y:46.537574 u:43.627419 v:42.909682
VERDICT: PASS     (baseline and new, identical to the last digit)
```

`tools/chromium-cang-smoke --waypipe`, new kernel:

```text
PASS  webgl-vulkan       ANGLE (AMD, Vulkan 1.4.343 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)) (0x00007480)), venus)
PASS  venus-presenting   waypipe-venus:ANGLE (AMD, Vulkan 1.4.343 (Virtio-GPU Venus ...
PASS  frame-presented    host-frame-early.png holds 100928 pattern pixels
PASS  renderer-on-frame  4613 pixels of the renderer overlay
PASS  control-no-frame   0 pattern pixels without --waypipe
VERDICT: PASS
```

So the 7.2.10 kernel reproduces the 7.2.7 baseline's VA-API numbers exactly and
still presents WebGL through venus - the `-kvm-lto` asset arm on the *pinned*
bytes is [below](#the-pinned-artifact).

### The packaged chain, not an injected firmware

Every arm above injects the firmware with `CANG_LIBKRUNFW_LIBRARY`. After the pin
moved to `v5.6.2-cang.4`, both arms were re-run against `.#cang-prebuilt` with
that variable unset, so the firmware was the one the package ships
(`lib/cang/libkrunfw.so.5` -> `/nix/store/36r5npbp9llf7v0hv9c56cv42y41wf14-libkrunfw-v5.6.2-cang.4/lib/libkrunfw.so.5.6.2`,
the `bd4cd0e5...` file). Both PASS with the same numbers, and the host side has
ground truth rather than a guest's word for it: sampling `/proc/*/maps` while the
VMs ran shows the package's `.cang-wrapped` ELF mapping exactly that store path,
and the pin's SRI is the `outputHash` recorded in the fetchurl derivation
`nix-store -q --deriver` reaches from it. Guest `uname` in both arms:
7.2.10-hardened1.

### Disk

The previous attempt at these arms died of `ENOSPC` (see `git log` on this note's
commit): the loop's backing filesystem (`/dev/sda2`, which also holds `$HOME` and
`/tmp`) went to zero while a parent `nix` build was writing into the loop, and
the failure surfaced as podman `load`'s `input/output error` on a btrfs layer.
Two habits fix it, and they are worth keeping: load the OCI archive **once** into
a store shared by all arms (the smoke documents this: an already-loaded image
reference keeps the caller's store), and refuse to start an arm below ~10 GiB
free on the backing filesystem.

## The pinned artifact

cang pins the tag release's `libkrunfw-x86_64-kvm-lto.tgz` -
`1d718e70f08fbd7705f2888eea674704c9dfb69b931508b97436778a1e7b6bc3`, 9,608,644
bytes, `sha256-HXGOcPCPvXcF8oiO6mdHBMnftpuTFQi5dDZ3ih57a8M=` as an SRI - and the
`libkrunfw.so.5.6.2` inside it is the `bd4cd0e5...` file both arms above ran
against with `CANG_LIBKRUNFW_LIBRARY`. The CI asset really is the ThinLTO
variant - its config carries `CONFIG_CC_IS_CLANG=y`,
`CONFIG_LTO_CLANG_THIN=y` and the job runs `make -f MakefileLto package` -
rather than being trusted by name. A locally built ThinLTO firmware (clang
21.1.8, sha `3363592037a0d6f731655b63365c68dbc804484efd52eb6135f5f5e37698a852`,
24,576,952 bytes) passes the same arms with the same numbers; it differs from
CI's only in toolchain, which makes it useful as a pre-tag check.

**The archive hash moves between CI runs of the same commit.** The rolling
prerelease for `901412d` published `libkrunfw-x86_64-kvm-lto.tgz` as
`78efc875...`, the first tag run as `43c15144...`, the second as
`1d718e70...` - while the `libkrunfw.so.5.6.2` extracted from all three is
byte-identical. `tar czf` stores each member's mtime, so only the archive
wrapper is nondeterministic. Two consequences: pin the hash of the asset the tag
release serves *after the last workflow run for that tag has finished* (a queued
or re-triggered run re-uploads with `--clobber` and invalidates a pin taken
earlier), and treat the library's own sha256 - not the archive's - as the
artifact's identity when comparing builds.

## Where this landed

The fork's `cang` branch at `901412d` (two commits: the re-base with the
re-authored 0023, and the refreshed configs), released as `v5.6.2-cang.4`, and
pinned in cang by `libkrunfwRelease` (x86_64) plus the `libkrunfw-src` input.
`Makefile`/`MakefileLto`, `nix/pkgs/libkrunfw.nix` and the documentation carry the
same 7.2.10 pair of versions.

## Open

- The `-lto` (non-KVM) variant is built and published but not smoked; only
  `-kvm-lto` is, because it is the asset the pin consumes. The same was true on
  the 7.2.7 line.
- aarch64/riscv64 stay on `cang-lts` (their configs are still 6.12-era); nothing
  here moves them.
