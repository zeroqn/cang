#!/usr/bin/env bash
# Build the guest rootfs + launcher, boot the libkrun VM, and report whether the
# guest saw a venus Vulkan device.
#
# Usage:
#   bash run.sh [gpu-flags-hex]
#   nix run .                       # equivalent (uses the flake's devShell env)
#
# The launcher routes the guest console to a host file; this script greps it for
# the probe verdict.  RENDER_SERVER_EXEC_PATH must point at the render server of
# the same virglrenderer the launcher's libkrun links (its libvirglrenderer.so.1);
# unless it is already set, it is resolved from the built launcher's own
# library paths.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLAGS="${1:-0x2c0}"

echo "=== building launcher (nix build .#launcher, which builds .#libkrun) ==="
LAUNCHER="$(nix build "$HERE#launcher" --no-link --print-out-paths)"
echo "launcher: $LAUNCHER"

# Resolve the virglrenderer that libkrun actually links (its resolved lib path)
# so RENDER_SERVER_EXEC_PATH matches the libvirglrenderer loaded at runtime.
# `ldd` on the launcher walks the whole chain: the launcher -> libkrun ->
# libvirglrenderer.
find_launcher_vgl() {
    local rp
    rp="$(ldd "$LAUNCHER/bin/launcher" 2>/dev/null | grep -oE '/nix/store/[A-Za-z0-9.+_-]+-virglrenderer-[^/]+/lib' | head -1)"
    if [[ -n "$rp" ]]; then
        echo "${rp%/lib}"
        return 0
    fi
    return 1
}

if [[ -z "${RENDER_SERVER_EXEC_PATH:-}" ]]; then
    vgl="$(find_launcher_vgl || true)"
    if [[ -n "$vgl" ]]; then
        export RENDER_SERVER_EXEC_PATH="$vgl/libexec/virgl_render_server"
    else
        echo "error: cannot find virgl_render_server; set RENDER_SERVER_EXEC_PATH" >&2
        exit 1
    fi
fi
echo "RENDER_SERVER_EXEC_PATH=$RENDER_SERVER_EXEC_PATH"

echo "=== building guest rootfs (nix build .#guest-rootfs) ==="
ROOTFS="$(nix build "$HERE#guest-rootfs" --no-link --print-out-paths)"
echo "rootfs: $ROOTFS"

# libkrun loads the guest firmware (`libkrunfw.so.5`) with dlopen by soname, so
# the firmware has to be on the loader path; take the pinned one the product
# boots rather than whatever else is installed on this host.
FIRMWARE="$(nix build "$HERE#libkrunfw" --no-link --print-out-paths)"
echo "libkrunfw: $FIRMWARE"

# Resolve the host-side 64-bit vulkan-loader and mesa from the rootfs closure
# (which uses the same flake nixpkgs).  The render server's venus renderer (vkr)
# dlopens libvulkan.so.1 on the host; it needs a 64-bit loader and a host ICD.
# Prefer the hardware ICD (radeon): on this L1 the virtio-gpu exposes a DRM
# capset, so radv reaches the real GPU and the guest venus device is
# hardware-backed.  Fall back to lavapipe (software) if no radeon ICD exists
# (e.g. a host whose virtio-gpu only advertises virgl capsets).  Override with
# VK_ICD=radeon|lvp.
VKLOADER="$(ls -d "$ROOTFS"/nix/store/*-vulkan-loader-*/lib 2>/dev/null | head -1)"
MESA="$(ls -d "$ROOTFS"/nix/store/*-mesa-*/lib 2>/dev/null | head -1)"
if [[ -z "$VKLOADER" || -z "$MESA" ]]; then
    echo "error: cannot resolve host vulkan-loader or mesa from rootfs" >&2
    exit 1
fi
ICD="${VK_ICD:-radeon}"
MESAICD="$(ls "$ROOTFS"/nix/store/*-mesa-*/share/vulkan/icd.d/${ICD}_icd.x86_64.json 2>/dev/null | head -1)"
if [[ -z "$MESAICD" && "$ICD" = "radeon" ]]; then
    echo "note: no radeon ICD on this host; falling back to lavapipe (software)" >&2
    ICD=lvp
    MESAICD="$(ls "$ROOTFS"/nix/store/*-mesa-*/share/vulkan/icd.d/${ICD}_icd.x86_64.json 2>/dev/null | head -1)"
fi
if [[ -z "$MESAICD" ]]; then
    echo "error: cannot resolve a ${ICD} ICD from rootfs" >&2
    exit 1
fi
export LD_LIBRARY_PATH="${FIRMWARE}/lib:${VKLOADER}:${MESA}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export VK_DRIVER_FILES="$MESAICD"
echo "VK_DRIVER_FILES=$VK_DRIVER_FILES (ICD=$ICD)"

BASE="$(mktemp -d)"
CONSOLE="$BASE/guest-console.log"
trap 'rm -rf "$BASE"' EXIT

echo "=== booting libkrun VM (gpu_flags=$FLAGS) ==="
set +e
RENDER_SERVER_EXEC_PATH="$RENDER_SERVER_EXEC_PATH" \
    "$LAUNCHER/bin/launcher" "$ROOTFS" "$CONSOLE" "$FLAGS"
rc=$?
set -e
echo "=== launcher exit=$rc (kernel/workload may have produced output above) ==="

echo
echo "=== guest console session ==="
cat "$CONSOLE" 2>/dev/null || true

echo
echo "=== verdict ==="
if grep -q "RESULT: PASS" "$CONSOLE" 2>/dev/null; then
    echo "PASS: the libkrun guest exposed a venus Vulkan device, created a logical device,"
    echo "      and a vkQueueSubmit fence completed."
    exit 0
elif grep -q "RESULT: FAIL\|FAIL" "$CONSOLE" 2>/dev/null; then
    echo "FAIL: the guest did not get a usable venus Vulkan device, or its submit fence"
    echo "      did not complete (see console above)."
    exit 1
else
    echo "INCONCLUSIVE: no probe verdict in the console (see log above)."
    exit 2
fi
