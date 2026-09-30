---
label: wayfinder:research
title: Two kernel lines, the versioned releases, and the LTO kernel on the GPU smoke
status: closed
blocked_by: []
claimed_by: pi session (2026-09-30)
---

## Question

With the 7.2.7 re-base verified, how do we land it without giving up the LTS
kernel - and does the released LTO variant (`libkrunfw-x86_64-kvm-lto.tgz`, the
asset cang's pin consumes) actually run Chromium's GPU path?

## Answer

One kernel line per branch, two permanently-published releases, and the LTO
variant passes the live Chromium smoke on the 7.2.7 kernel.

### The branch/tag layout

| branch | kernel | vsock/patch line | `release-arches` | tag shape |
| --- | --- | --- | --- | --- |
| `cang` | linux-7.2.7 + v7.2.7-hardened1 | the 23-patch 7.x series; **no** arm64 patches | `x86_64` | `v5.6.2-cang.<n>` |
| `cang-lts` | linux-6.12.109 + v6.12.109-hardened1 | the 39-patch 6.12 series, arm64 included | `x86_64`, `aarch64`, `riscv64` | `v5.6.2-cang-lts.<n>` |

`release-arches` is the branch's release scope: the publish workflow builds and
asserts exactly the architectures listed there, so a line whose kernel config
has not been refreshed for its `KERNEL_VERSION` publishes nothing for that
architecture instead of a stale asset. Rolling dev releases get a per-line
prefix (`cang-<sha>` / `cang-lts-<sha>`) and each line prunes its own newest ten,
so the two lines never shadow each other.

This is the shape bob asked for: arm64 patches are maintained **only** on
`cang-lts`, and the newest kernel does not carry them.

Published on 2026-09-30:

- `v5.6.2-cang.3` at `0f11a88` (`cang`, x86_64: `libkrunfw-x86_64.tgz`,
  `-lto`, `-kvm-lto`)
- `v5.6.2-cang-lts.1` at `d46d8e7` (`cang-lts`, the same three plus
  `libkrunfw-aarch64.tgz`, `libkrunfw-riscv64.tgz`)

(Fork commits: `694c1f6` re-base, `e0ec7d1` release workflow + `release-arches`,
`0f11a88` refreshed x86_64 configs, `d46d8e7` the scope-gate fix below.)

### The LTO variant

`MakefileLto` carried its own copy of the kernel base **and** `FULL_VERSION`
(5.3.0), so the LTO assets of the `cang` line had been built from the old kernel
under a new tag; it now matches `Makefile` (5.6.2, linux-7.2.7), and all four
x86_64 configs are refreshed from a real build of their own variant:

| config | produced by | what it is |
| --- | --- | --- |
| `config-libkrunfw_x86_64` | gcc build | the plain release kernel (gzip, no deferred init) |
| `config-libkrunfw_x86_64-kvm` | gcc build | what cang's own local kernel build uses (lz4 + deferred init) |
| `config-libkrunfw_x86_64-lto`, `-kvm-lto` | clang ThinLTO build | the two LTO assets |

Local LTO builds need care with a nix toolchain, and the traps are worth
recording:

- Nix's *wrapped* clang feeds its own `--target`/`-nostdlibinc` into the kernel's
  flags, which the kernel's `-Werror` sites turn into hard errors; and the final
  `-shared` link cannot find `crtn.o`. Build with
  `nixpkgs#llvmPackages.clang-unwrapped` + `nixpkgs#llvmPackages.bintools-unwrapped`
  plus `HOSTCC=gcc HOSTCXX=g++`. CI's stock clang needs none of this.
- `CLANG=` on the make command line is **also** the kernel's own compiler
  variable, so overriding it to add link flags breaks the *kernel* build
  (`tools/objtool/...`). Build the kernel with `make -f MakefileLto
  KERNEL_SOURCES=... KERNEL_CONFIG=... <tree>/vmlinux` and link the firmware
  separately: `clang -nostartfiles -L<gcc-libdir> -L<libgcc_s-dir> -L<libc-dir>
  -flto=thin -fuse-ld=lld -fPIC -DABI_VERSION=5 -shared
  -Wl,-soname,libkrunfw.so.5 -o libkrunfw.so.5.6.2 kernel.c`.
- A config file newer than the extracted kernel tree makes make re-extract and
  re-apply every patch into an already-patched tree ("patching file ... done"
  then failure); write refreshed configs to a `*.new` beside the build and move
  them into the fork tree only once the builds are done.

### The live smoke on `-kvm-lto`

`tools/chromium-cang-smoke/chromium-smoke.sh` against the locally built
`-kvm-lto` firmware (clang 21.1.8 / LLD, sha256
`b92ee738810ad641807dcb077f9ac2696a36b3ad273dba4cb7794ed2830ff1a1`), the
tree-built `cang` 0.10.1 and `.#container`:

```text
PASS  version       Chromium 154.0.8037.57
PASS  chromium-rc   gpu-dom=0 webgl=0 dom=0
PASS  webgl-vulkan  ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)) (0x00007480)), venus)
PASS  webgl-png     non-empty PNG screenshot
VERDICT: PASS
```

`gpu-diag.txt` in that run's evidence reads `uname: Linux localhost
7.2.7-hardened1 #1 SMP PREEMPT_DYNAMIC ... x86_64`, so the venus renderer is the
7.2.7 kernel's. A separate boot probe of the same bytes reports
`Linux version 7.2.7-hardened1 (root@libkrunfw) (clang version 21.1.8, LLD
21.1.8)`, TSI registered in `/proc/net/protocols`, `/dev/vsock` + `/dev/hvc0`,
zram0 swap and a virtiofs root.

### The cang side

`nix/pins.nix` keeps one primary `tag` - the line cang's own x86_64 asset comes
from, and the one `publish_release.yml` gates a tagged cang release on - while a
system may carry a `tag` of its own when its asset lives on the other line:
aarch64 and riscv64 point at `v5.6.2-cang-lts.1`, x86_64 at `v5.6.2-cang.3`.
`nix/pkgs/libkrunfw.nix` resolves `systemPins.tag or release.tag`, and
`scripts/update-libkrunfw.sh` keeps that invariant (an x86_64 run moves the
primary tag; another system records its own only when it differs, and a system
tag equal to the primary is dropped as redundant). The repository test
`is_versioned_fork_release_tag` accepts both `-cang.<n>` and `-cang-lts.<n>`,
and `pinned_fork_releases_use_permanent_version_tags` now checks every tag in
the `libkrunfwRelease` block, not just the primary.

### The release-scope gate bug (found by this work)

The first `v5.6.2-cang-lts.1` run published only the x86_64 assets and then
failed in the publish job with `missing expected asset
dist/libkrunfw-aarch64.tgz`. Cause: the `Check release scope` step ran **before**
`actions/checkout`, so `release-arches` was never in the workspace, every gate
reported `ready=false` (silently skipping the aarch64/riscv64 jobs) while the
publish job - which does have the file - asserted their assets. Fixed in
`d46d8e7` by moving the gate after the checkout; the re-tagged run is the one
that publishes.

## Open

- The `cang` line has no refreshed `config-libkrunfw_aarch64`/`-riscv64`, and no
  arm64 patches. Enabling an architecture there is a deliberate act: port the
  patches, refresh the config, add the arch to `release-arches`.
- The plain (non-KVM) `libkrunfw-x86_64.tgz` and the non-KVM `-lto` asset are
  published but not consumed by cang (cang's pin takes `-kvm-lto`); they are for
  libkrun-side users.
- `cang-lts` still carries `MakefileLto` at its own kernel base; a future LTS
  re-base repeats the same procedure there.
