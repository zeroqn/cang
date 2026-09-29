# 05 - libkrunfw kernel support: progress (2026-09-28; boot test 2026-09-29)

Ticket `tickets/05-libkrunfw-kernel-support.md`. Executed in a scratch copy
(`/home/dev/.pi/agent/sessions/--home-dev-cang-cang--/2026-09-28T14-01-49-360Z_01a0e852-662f-7443-a963-f55184fd60ea.jsonl.scratch/kw`), not in a dirty submodule; the patches are authored in
`deps/libkrunfw/patches/`.

## What landed in `deps/libkrunfw`

| file | contents |
|---|---|
| `patches/0037-drm-virtio-import-scanout-buffers-from-other-devices.patch` | Vivek's `[PATCH v5 0/5]` (5 upstream commits squashed; only the `virtgpu_plane.c` include re-authored for 6.12.109) |
| `patches/0038-drm-virtio-support-CREATE_GUEST_HANDLE-and-BLOB_CTX_ID_FIX.patch` | Val's six `guest-handle` commits (0-free-commit); feature-table/`kms.c`/uapi hunks re-authored; upstream's `BLOB_ALIGNMENT = 5` and param 9 deliberately **not** imported |
| `patches/0039-drm-virtio-gate-guest-blob-prime-import.patch` | fork-authored: the conditional PRIME-import unlock, plus v3's intent that CREATE_GUEST_HANDLE requires BLOB_CTX_ID_FIX (blob path, flag stamp and ctx_id all gated on both bits), plus a NULL `prime_import_file_priv` guard |
| `config-libkrunfw_*` (all six) | `# CONFIG_UDMABUF is not set` -> `CONFIG_UDMABUF=y` |

Verified: all 39 `patches/0*.patch` plus the linux-hardened patch apply cleanly
to a pristine `linux-6.12.109`, and the result is byte-identical to the tree the
patches were authored in (`git apply`-free `patch -p1`, no rejects).

## Build evidence (this host, 28 cores, `nix develop path:<copy>`)

- plain `make -j28` (config `x86_64-kvm`): **3m40s**, `libkrunfw.so.5.6.2`.
- `make -f MakefileLto package -j28`: **8m31s**, both shipped assets -
  `libkrunfw-x86_64-lto.tgz` (8.8 MB) and `libkrunfw-x86_64-kvm-lto.tgz`
  (8.9 MB). So the "hours" concern does not hold on this machine.

Two build traps worth keeping:

- The LTO build **fails with the devshell's wrapped clang**:
  `clang: error: argument unused during compilation: '-nostdlibinc'
  [-Werror,-Wunused-command-line-argument]` in `scripts/mod/*.s`. The fix is the
  one the previous map recorded: `CC=<unwrapped clang>
  HOSTCC=<wrapped clang>`.
- The libkrunfw flake devshell has clang/lld/llvm-strip/bc/flex/bison and a
  python with pyelftools, but **no lz4** (unused by the make path here).

## Boot test on the locally built firmware (2026-09-29)

The firmware was rebuilt from the committed `deps/libkrunfw` (`3fdbb59`) inside
Nix - `nix build --impure --expr ... pkgs.callPackage ./nix/pkgs/libkrunfw.nix
{ inherit pins; useLocalSource = true; }` - to
`/nix/store/wwj5xwdj509scmasyvadhca8vjj4zh3p-libkrunfw-v5.6.2-cang.1-local`,
and pointed at with `CANG_LIBKRUNFW_LIBRARY` (which replaces cang's
package-relative firmware candidates). cang is the tree build
`/nix/store/5ybxzdcwqwj4x0py7s9yv4dfmxmix5dv-cang-0.9.1`; the VM runs the
`localhost/cang:latest` image on the btrfs loop image at `/home/dev/cang/disk`
(hermetic `storage.conf`, `[task-state] location` on btrfs), through
`script -q -e` with `--mem 4 --gpu=drm --alloc hardened --seccomp=off
--landlock=off`.

In-guest probe (`python3` + ctypes for `DRM_IOCTL_VIRTGPU_GETPARAM`; the ioctl is
`_IOWR('d', 0x40 + DRM_VIRTGPU_GETPARAM(0x03), struct drm_virtgpu_getparam)` and
its `value` field is a *userspace pointer* the kernel copies to, so it must be
`ctypes.addressof` of a real buffer):

| check | `--gpu=drm` | `--gpu=drm --zero-copy-shm` |
|---|---|---|
| `uname -r` | `6.12.109-hardened1` | `6.12.109-hardened1` |
| `/dev/udmabuf` | present (`crw------- root root 10,258`) | present |
| `VIRTGPU_PARAM_3D_FEATURES` (1) | 1 | 1 |
| `VIRTGPU_PARAM_RESOURCE_BLOB` (3) | 1 | 1 |
| `VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs` (7) | 54 | 54 |
| **`VIRTGPU_PARAM_CREATE_GUEST_HANDLE` (10)** | **0** | **1** |
| negotiated `features` bitstring | bits 0-4 + 32 | bits 0-4, **6**, **7** + 32 |

`/sys/bus/virtio/devices/virtio0/features` is a *bitstring* whose character `i` is
bit `i` (not a number), so the two runs read
`1111100000000000000000000000000010000000000000000000000000000000` and
`1111101100000000000000000000000010000000000000000000000000000000` - the
`--zero-copy-shm` run adds exactly bits 6 `CREATE_GUEST_HANDLE` and 7
`BLOB_CTX_ID_FIX`, i.e. the host withheld them until the `/dev/udmabuf` probe
passed. `SUPPORTED_CAPSET_IDs` 54 = `0b110110` (virgl + venus/gfxstream
capsets), and `status=0x0000000f` means the driver completed feature
negotiation.

The shipped **`libkrunfw-x86_64-kvm-lto.tgz`** (the variant cang pins) was
extracted from the scratch `make -f MakefileLto package` output and booted the
same way: identical results. The plain `x86_64-kvm` variant is the Nix
`useLocalSource` build above.

### Host prerequisite found on the way

`--zero-copy-shm` cannot engage at all unless the host's `/dev/udmabuf` is
openable *from inside cang's keep-id user namespace*, which is where the probe
runs. On this host the node was `crw-rw---- root kvm`, and the worker's uid/gid
map (`0:165536:...`, `993:993:1`, `994:166529:...`) leaves gid 302 unmapped, so
neither the group bits nor the namespace's `CAP_DAC_OVERRIDE` can reach it - and
the invoking user is not in `kvm` either. `/dev/kvm` works because it is
`crw-rw-rw-`. After `chmod 0666 /dev/udmabuf` the fast path engaged (the table
above). Worth an explicit udev rule wherever the fast path is meant to work; the
libkrun warning for the unusable device is now visible by default (cang floors
libkrun's own log level at `warn`).

### Not verified here

- **The blob itself**: param 10 = 1 proves the guest driver accepted bit 6, so
  `verify_blob` will let a `VIRTGPU_BLOB_FLAG_CREATE_GUEST_HANDLE` (0x8) blob
  through; an actual blob creation needs a virgl/venus client and is ticket 09.
- **aarch64 / riscv64**: only x86_64 was built and booted; `CONFIG_UDMABUF=y`
  went into all six configs, but the per-arch build is CI's job (ticket 06).
- **The guest driver's own `features: ... +create_guest_handle` line**: the image
  boots with `kernel.dmesg_restrict=1` and its `dmesg` cannot read the ring
  buffer, and `/dev/kmsg` returned nothing through the session; the negotiated
  bitstring and param 10 are the equivalent evidence.

## Status after the boot test

- Boot-tested (below). The artifacts above exist only in the scratch copy; the
  release + per-system repin is ticket 06, and bob tags.
- `MakefileLto` still says `FULL_VERSION = 5.3.0` while `Makefile` says 5.6.2
  (pre-existing): the LTO tarballs therefore carry `libkrunfw.so.5.3.0`. cang
  opens the library by soname (`libkrunfw.so.5`), so it is harmless today - and
  the kvm-lto boot test below ran with exactly that asset.
