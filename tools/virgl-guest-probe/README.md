# Virgl Guest Probe

A bare libkrun microVM that boots a venus virtio-gpu and checks whether the
guest sees a usable Vulkan device through virglrenderer's VENUS renderer.

**Status: PASSING (hardware-backed), on libkrun's ABI-2.**  The guest creates a
venus Vulkan instance, enumerates one physical device backed by the host GPU
(`Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33))`), creates a logical
device, and completes both a submitted fence and the heavy burst/churn stage.
The host ICD is `radeon` (hardware, via the L1 virtio-gpu DRM capset); on hosts
without a hardware path it falls back to lavapipe.  Both variants pass:
`run.sh` (venus through an in-process virglrenderer) and `run-rs.sh` (venus
through an external, sandboxed `virgl_render_server`, i.e. cang's own path).

## Files

- `guest-probe.c` — in-guest binary: dlopens `libvulkan.so.1`, enumerates
  physical devices, creates a logical device, then submits a fill under a fence
  and a burst/churn stage; prints `BASELINE: PASS` / `HEAVY RESULT: PASS` or a
  `RESULT: FAIL` verdict.  Compiles against `vulkan-headers` but does NOT link
  libvulkan (it is loaded at runtime so the probe always uses whatever
  Mesa/Vulkan the guest image ships).
- `guest-rootfs.nix` — Nix derivation that assembles a minimal rootfs
  directory tree containing glibc, Mesa (with the venus/virtio ICD), the
  Vulkan loader, busybox, and the baked `guest-probe` binary.  Its `/init`
  mounts `/dev`, `/proc`, `/sys` and runs the probe (libkrun's init already
  mounts those; the extra mounts are harmless and keep the tree usable outside
  libkrun).
- `abivm.h` — the shared libkrun **ABI-2** boot path: payload (the `libkrunfw`
  kernel), the init config that names the guest command, the virtio-fs root at
  tag `/dev/root`, the console (with the guest console redirected to the host
  file named on the command line), the venus virtio-gpu device and its headless
  display backend, then `krun_vmm_builder_*`/`krun_vmm_run`.
- `launcher.c` — host-side driver for the in-process variant: argv is
  `<rootfs> <console-file> [gpu-flags-hex]`, default flags `0x2c0`
  (VENUS|NO_VIRGL|RENDER_SERVER), 256 MiB virtio-gpu shm window, no external
  render server.
- `launcher-rs.c` — host-side driver for the external render-server variant:
  creates a `SOCK_SEQPACKET` socketpair, forks `virgl_render_server
  --socket-fd=<child>`, and hands the parent end to the GPU device with
  `krun_gpu_device_set_render_server_fd` (the ABI-2 spelling of v1's
  `krun_set_gpu_options3` fourth argument).  Flags default to `0xe43`
  (cang's `VIRGLRENDERER_VENUS_FLAGS`).  Optional env knobs, unchanged from the
  original tool: `CANG_BOOTSTRAP=<cang-bin>` runs the real cang render-server
  bootstrap (Landlock + packaged seccomp), `SANDBOX_RS=1` applies the seccomp
  allowlist alone (via `sandbox-rs`), `LANDLOCK_RS=<variant>` applies cang's
  Landlock rule set alone (via `landlock-rs`).
- `sandbox-rs.c` / `landlock-rs.c` — the seccomp and Landlock wrappers those env
  knobs exec.
- `run.sh` — builds the rootfs, the pinned firmware and the launcher via `nix
  build`, boots the VM, and greps the console log for the probe verdict.
  **Sets the host-side render-server environment** (LD_LIBRARY_PATH for a
  64-bit vulkan-loader + mesa, VK_DRIVER_FILES → the hardware ICD when
  available, and the pinned `libkrunfw` on the loader path because libkrun
  dlopens it by soname).  Default ICD is `radeon` (hardware); falls back to
  lavapipe (software) when no radeon ICD exists.  Override with
  `VK_ICD=radeon|lvp`.
- `run-rs.sh` — same, through `launcher-rs` and the external render server.
- `flake.nix` — Nix flake that wires everything together: `libkrun` (the C
  API, built here from the fork revision the parent repo pins), `libkrunfw`
  (the parent's pinned firmware), `guest-probe`, `guest-rootfs`, `launcher`,
  `launcher-rs`, `sandbox-rs`, `landlock-rs`, plus a dev shell with the build
  inputs.

## libkrun, and why the flake builds it

The repo does not export a `packages.<system>.libkrun`: cang links libkrun's
*Rust* API by compiling the `deps/libkrun` checkout into its own binary, so it
never builds the shared libraries a C launcher links.  This flake therefore
builds `libkrun.so` + `libkrun_init.so` (`ffi` + `gpu` features) from the
parent's `libkrun-src` input — the same commit `.gitmodules` pins for
`deps/libkrun` — and takes the musl guest init from the parent's own
`nix/pkgs/libkrun-source.nix`, so the guest init and its kernel are what the
product uses.  `cargoDeps` is the hash of that workspace's vendored registry
(it includes libkrun's pinned `rutabaga_gfx` and `ffier` git dependencies);
refresh it with `nix build .#libkrun` after a pin bump.

## Usage

```bash
# Build the rootfs and launcher, boot the VM, and check the verdict:
nix run .           # or
bash run.sh

# The external render-server variant:
bash run-rs.sh

# Override GPU flags (run.sh default 0x2c0 = VENUS|NO_VIRGL|RENDER_SERVER,
# run-rs.sh default 0xe43 = cang's VIRGLRENDERER_VENUS_FLAGS):
bash run.sh 0x2c0
bash run-rs.sh 0xe43

# Enter the dev shell for manual compile/test:
nix develop . -c bash
```

## Host-side requirements

The venus render server (`virgl_render_server`, a 64-bit process spawned by
libkrun on the host) dlopens `libvulkan.so.1` **by soname** to do the actual
Vulkan work.  Its search is governed by LD_LIBRARY_PATH (its own RUNPATH only
covers gbm/glibc), so two host-side failures are possible:

- `wrong ELF class: ELFCLASS32` — the render server found a **32-bit**
  `libvulkan.so.1` on its search path.  NixOS machines can ship a 32-bit
  `vulkan-loader` (e.g. pulled in by i686 deps of `ffmpeg-headless` or
  `pipewire`); if that dir is on LD_LIBRARY_PATH, the 64-bit server loads the
  32-bit loader and fails.
- `cannot open shared object file` — no `libvulkan.so.1` is findable at all
  (clean environment, since the render server does not search `/run/opengl-driver`).

Both produce the same guest symptom: `vkCreateInstance => -1`.

The run scripts fix both by prepending the rootfs closure's 64-bit
`vulkan-loader` and `mesa` lib dirs to LD_LIBRARY_PATH and setting
VK_DRIVER_FILES to a host ICD from the same mesa (`radeon_icd.x86_64.json` by
default, `lvp_icd` as a software fallback).  On this L1 the virtio-gpu exposes
the **DRM capset** (`VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs` = 0x46 → bits
{1,2,6} = virgl, virgl2, DRM), which is how radv reaches the real GPU; a host
whose virtio-gpu only advertises virgl capsets has no hardware Vulkan device and
must use lavapipe.  Any product runtime that spawns this render server must set
the same environment.

Two further loader-path facts, both handled by the run scripts:

- libkrun loads the guest firmware with `dlopen("libkrunfw.so.5")`, so the
  firmware's `lib` directory (the parent's pinned `packages.libkrunfw`) has to
  be on LD_LIBRARY_PATH.
- the launcher, `libkrun.so` and `libvirglrenderer.so.1` come from the nix
  store with their own runpaths, so nothing else needs to be exported.

## Architecture

```
┌─ host ──────────────────────────────────────────────────────┐
│  launcher.c / launcher-rs.c                                  │
│     │                                                        │
│     │  krun_payload_load_krunfw        (libkrunfw kernel)     │
│     │  krun_init_config_apply          (guest /init + argv)   │
│     │  krun_fs_device_new("/dev/root") (virtio-fs root)       │
│     │  krun_console_device_builder     (console -> host file) │
│     │  krun_gpu_device_new(0x2c0|0xe43)(virtio-gpu, venus)    │
│     │  krun_gpu_device_set_render_server_fd  (launcher-rs)    │
│     │  krun_vmm_builder_build / krun_vmm_run                 │
│     └───────┬─────────────────────────────────────────────────┘
│             │  virtio-gpu (venus)  ──►  virglrenderer           │
│             │     in-process (run.sh) or virgl_render_server     │
│             │     over the SOCK_SEQPACKET fd (run-rs.sh)         │
└─────────────┼────────────────────────────────────────────────────┘
              │
┌─ guest ─────┴──────────────────────────────────────────────────┐
│  /init  ──►  guest-probe  ──►  libvulkan.so.1                 │
│                      │          (virtio ICD)                    │
│                      └──►  vkCreateInstance                     │
│                      └──►  vkEnumeratePhysicalDevices           │
│                      └──►  vkCreateDevice                       │
│                      └──►  vkQueueSubmit + fence                │
│                      └──►  "BASELINE: PASS" / "HEAVY RESULT: PASS" │
└─────────────────────────────────────────────────────────────────┘
```
