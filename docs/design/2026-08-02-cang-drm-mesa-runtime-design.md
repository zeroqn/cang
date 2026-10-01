# Cang DRM Mesa Runtime Design

Approved on 2026-08-02.

## Problem

The cang image includes `pkgs.mesa`, but its stable runtime link and explicit graphics-driver environment currently serve the software-only Waypipe path. A guest command launched with `--gpu=drm` receives libkrun virtio-GPU DRM nodes, yet guest-init does not explicitly expose Mesa's DRI, EGL, and Vulkan driver discovery paths to that command.

DRM nodes alone provide the kernel interface. Applications such as Chromium also need a discoverable Mesa userspace driver stack to create EGL/OpenGL or Vulkan contexts.

## Scope

### Included

- Make Mesa's hardware-capable DRI, EGL, and Vulkan runtime metadata discoverable to guest commands when `CANG_GPU_DRM=1`.
- Apply the same behavior to `--wayland`, because it automatically selects `--gpu=drm`.
- Keep the configuration scoped to DRM-enabled launches.
- Preserve the existing DRM-device permissions and supplementary-group setup.
- Add focused guest-init and image checks.
- Document the user-visible `--gpu=drm` behavior.

### Excluded

- Globally exposing Mesa driver variables to all cang guest commands.
- Changing libkrun GPU configuration or DRM-node creation.
- Forcing software rendering in DRM mode.
- Selecting llvmpipe, lavapipe, or a single hardware driver by name.
- Changing the software-only Waypipe renderer contract.
- Adding Chromium-specific launch flags or branches.
- Solving Chromium's separately observed hardened-kernel UMIP `SIGTRAP`.

## Approaches Considered

### Global Mesa discovery

Export Mesa paths for every cang guest process.

This is simple, but it changes non-GPU launches and can interfere with the intentionally separate Waypipe software-renderer policy. It is rejected.

### Application wrappers

Wrap Chromium or other graphics applications with Mesa environment variables.

This does not generalize to other EGL/OpenGL or Vulkan applications and puts runtime policy in application packaging. It is rejected.

### DRM-scoped guest-init environment

When the launch contract reports `CANG_GPU_DRM=1`, guest-init exports Mesa's DRI, EGL vendor, and Vulkan ICD discovery paths before executing the workload. The image provides a stable, hash-independent path to the Mesa package.

This is the selected approach because it follows the existing guest-init launch boundary, applies to every DRM client, and leaves other launch modes unchanged.

## Architecture

### Image contract

The cang image continues to include the complete pinned `pkgs.mesa` output. It exposes that package through a stable guest path under `/usr/lib`, allowing guest-init to reference Mesa without embedding a Nix store hash.

The stable path must preserve the complete package layout so Mesa metadata can resolve its libraries and drivers correctly. Image checks verify the relevant directories and metadata needed for:

- DRI/EGL/GBM/OpenGL discovery
- GLVND EGL vendor discovery
- Vulkan ICD discovery

The checks must cover hardware-capable driver availability, not only the existing llvmpipe and lavapipe software artifacts.

### Guest-init behavior

During cang bootstrap, after parsing the launch contract and before dropping privileges and executing the command, guest-init checks `gpu_drm`.

When `gpu_drm` is true, it exports:

- `LIBGL_DRIVERS_PATH` pointing at the stable Mesa DRI directory
- `LIBVA_DRIVERS_PATH` pointing at the same directory, so the guest-side
  `virtio_gpu` VA-API driver (the vrend video path) is discoverable; libva has no
  default search path for the mesa runtime directory
- `__EGL_VENDOR_LIBRARY_FILENAMES` pointing at Mesa's EGL vendor metadata
- Vulkan ICD discovery pointing at the stable Mesa Vulkan ICD directory or its hardware-capable metadata set

The implementation must not set `LIBGL_ALWAYS_SOFTWARE` and must not pin the Vulkan loader to lavapipe. Mesa and the loader remain responsible for selecting the driver compatible with the libkrun DRM device.

When `gpu_drm` is false, guest-init does not export these DRM Mesa variables.

### Relationship to Wayland

`--wayland` automatically enables DRM mode, so it receives the same Mesa discovery environment in addition to its existing `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` exports.

This ensures clients using the local Wayland passthrough can discover the Mesa userspace driver for the virtio-GPU DRM nodes.

### Relationship to Waypipe

Waypipe remains separate. Its child process keeps the existing software-only environment:

- `LIBGL_ALWAYS_SOFTWARE=1`
- llvmpipe DRI discovery
- Mesa EGL vendor metadata
- lavapipe Vulkan ICD

Waypipe cannot be combined with `--gpu=drm`, so the two graphics policies do not overlap.

## Data Flow

- The CLI selects `GpuMode::Drm` directly through `--gpu=drm` or indirectly through `--wayland`.
- Host launch planning serializes the DRM mode and passes `CANG_GPU_DRM=1` to guest-init.
- Libkrun configures virtio-GPU through `krun_set_gpu_options2` and exposes DRM nodes in the guest.
- Guest-init prepares DRM-node ownership and permissions.
- Guest-init exports Mesa hardware-driver discovery paths for the workload.
- The application graphics loader discovers Mesa and selects the driver matching the virtual DRM device.

## Error Handling

No runtime fallback or application-specific recovery is added.

- If libkrun cannot configure DRM mode, the existing host-side setup error remains authoritative.
- If the image lacks required Mesa artifacts, image validation fails during the build.
- If an application cannot initialize the exposed GPU, its EGL/OpenGL or Vulkan loader error remains visible.
- Guest-init does not silently fall back to llvmpipe or lavapipe for DRM mode.

## Testing

### Guest-init tests

Add focused tests that verify:

- DRM mode exports Mesa DRI, EGL, and Vulkan discovery paths.
- Non-DRM mode does not export those variables.
- DRM mode does not set `LIBGL_ALWAYS_SOFTWARE`.
- The existing Waypipe software-renderer environment remains unchanged.

### Image checks

Extend cang image validation to verify:

- The stable Mesa runtime path exists.
- Mesa's DRI driver directory includes the driver artifacts required for the libkrun native-context DRM device.
- Mesa EGL vendor metadata exists.
- Hardware-capable Vulkan ICD metadata is available when supported by the pinned Mesa package.
- The stable path roots all referenced Mesa artifacts.

### Validation

Run:

- `nix develop --command cargo fmt --check`
- `nix develop --command cargo clippy --all-targets --all-features -- -D warnings`
- `nix develop --command cargo deny check`
- `nix develop --command cargo test`
- The focused cang image check or `nix build .#container` for the cang image artifact

Runtime verification should launch a guest command with `--gpu=drm` and confirm that EGL/OpenGL and Vulkan enumeration select the libkrun virtual GPU rather than llvmpipe or lavapipe. Chromium can then be used as an application-level smoke test, while treating the known hardened-kernel UMIP `SIGTRAP` as a separate issue from Mesa discovery.
