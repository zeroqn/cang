#!/usr/bin/env bash
# External render-server venus probe: boot a bare libkrun VM with the EXACT
# product render-server path (virtio-gpu + SOCK_SEQPACKET render-server fd +
# sandboxed external virgl_render_server) and check the guest's venus submit
# fence.
#
# Usage:
#   bash run-rs.sh [gpu-flags-hex]
#
# Builds the same guest rootfs + guest-probe as run.sh, but launches through
# launcher-rs.c which mirrors cang's render_server.rs: socketpair -> spawn
# virgl_render_server --socket-fd=<child> -> krun_gpu_device_set_render_server_fd
# (flags 0xe43, the ABI-2 spelling of v1's krun_set_gpu_options3). Requires the
# dev-shell env (RENDER_SERVER_EXEC_PATH and an LD_LIBRARY_PATH that resolves
# the host-side 64-bit vulkan loader).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLAGS="${1:-0xe43}"

echo "=== building launcher-rs (nix build .#launcher-rs, which builds .#libkrun) ==="
LAUNCHER="$(nix build "$HERE#launcher-rs" --no-link --print-out-paths)"
echo "launcher-rs: $LAUNCHER"

ROOTFS="$(nix build "$HERE#guest-rootfs" --no-link --print-out-paths)"
echo "rootfs: $ROOTFS"

# libkrun loads the guest firmware (`libkrunfw.so.5`) with dlopen by soname, so
# the firmware has to be on the loader path; take the pinned one the product
# boots rather than whatever else is installed on this host.
FIRMWARE="$(nix build "$HERE#libkrunfw" --no-link --print-out-paths)"
echo "libkrunfw: $FIRMWARE"

# The render server must be the one from the virglrenderer libkrun links; resolve
# it from the launcher's own library paths unless it is already set.
if [[ -z "${RENDER_SERVER_EXEC_PATH:-}" ]]; then
    rp="$(ldd "$LAUNCHER/bin/launcher-rs" 2>/dev/null | grep -oE '/nix/store/[A-Za-z0-9.+_-]+-virglrenderer-[^/]+/lib' | head -1)"
    if [[ -z "$rp" ]]; then
        echo "error: cannot find virgl_render_server; set RENDER_SERVER_EXEC_PATH" >&2
        exit 1
    fi
    export RENDER_SERVER_EXEC_PATH="${rp%/lib}/libexec/virgl_render_server"
fi
echo "RENDER_SERVER_EXEC_PATH=$RENDER_SERVER_EXEC_PATH"

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
    echo "error: cannot resolve a host ICD from rootfs" >&2
    exit 1
fi
echo "VK_DRIVER_FILES=$MESAICD (ICD=$ICD)"
export LD_LIBRARY_PATH="${FIRMWARE}/lib:${VKLOADER}:${MESA}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export VK_DRIVER_FILES="$MESAICD"

BASE="$(mktemp -d)"
CONSOLE="$BASE/guest-console.log"
trap 'rm -rf "$BASE"' EXIT

echo "=== booting libkrun VM via external render server (flags=$FLAGS) ==="
set +e
"$LAUNCHER/bin/launcher-rs" "$ROOTFS" "$CONSOLE" "$FLAGS"
rc=$?
set -e
echo "=== launcher-rs exit=$rc ==="
echo
echo "=== guest console session ==="
cat "$CONSOLE" 2>/dev/null || true

echo "=== verdict ==="
if grep -q "RESULT: PASS" "$CONSOLE" 2>/dev/null; then
    echo "PASS: external render-server path (SOCK_SEQPACKET render-server fd) completes a venus submit fence."
    exit 0
elif grep -q "RESULT: FAIL\|FAIL" "$CONSOLE" 2>/dev/null; then
    echo "FAIL: the external render-server venus path did not produce a usable device or completed fence."
    exit 1
else
    echo "INCONCLUSIVE: no probe verdict in the console (see log above)."
    exit 2
fi
