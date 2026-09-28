# 05 - libkrunfw kernel support: progress (2026-09-28)

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

## Still open

- Boot-test in a cang guest (the guest-side checks: `/dev/udmabuf` present,
  param 10 non-zero, blob flag accepted, `uname -r` unchanged). Needs the
  live-VM recipe and a cang built from the ported tree.
- Whether to tag a new libkrunfw release now (bob) and repin per system; the
  artifacts above exist only in the scratch copy.
- `MakefileLto` still says `FULL_VERSION = 5.3.0` while `Makefile` says 5.6.2
  (pre-existing): the LTO tarballs therefore carry `libkrunfw.so.5.3.0`. cang
  opens the library by soname (`libkrunfw.so.5`), so it is harmless today.
