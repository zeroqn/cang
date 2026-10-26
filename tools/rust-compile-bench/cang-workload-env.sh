#!/bin/sh
# Resolve, from the /nix store the guest sees, the build-time paths a cang-tree
# rust build needs: libkrun's build scripts want virglrenderer, gbm, libdrm and
# libclang. Prints shell assignments.
#
# Meant to be SOURCED from a `set -e` script, so every step must be `set -e`-safe:
# no `[ -n "$x" ] && cmd` at statement level (a false test would abort the caller
# silently - that mistake cost two failed arms), and an explicit return at the end.
#
# Resolve by glob, never by a pinned store hash: a hard-coded hash rots silently.
# A missing piece is reported by name and fails the caller.
set -u

pick() { # pick <glob> -> first existing match, else empty
    for p in $1; do
        if [ -e "$p" ]; then printf '%s' "$p"; return 0; fi
    done
    return 0
}

VIRGL_PC=$(pick '/cang-toolchain/virglrenderer/lib/pkgconfig')
if [ -z "$VIRGL_PC" ]; then VIRGL_PC=$(pick '/nix/store/*virglrenderer-*/lib/pkgconfig'); fi
GBM_PC=$(pick '/nix/store/*mesa-libgbm-*/lib/pkgconfig')
DRM_PC=$(pick '/nix/store/*libdrm-*-dev/lib/pkgconfig')
LIBCLANG=$(pick '/nix/store/*clang-*-lib/lib')
VIRGL_LIB=$(pick '/cang-toolchain/virglrenderer/lib/libvirglrenderer.so')
if [ -z "$VIRGL_LIB" ]; then VIRGL_LIB=$(pick '/nix/store/*virglrenderer-*/lib/libvirglrenderer.so'); fi
# init-blob's build script needs a prebuilt static krun-init (or a musl target to
# build its own); the image ships neither, so the arm bind-mounts the host's.
# Staged by the harness into /cang-toolchain (the image ships neither, and the
# guest's /nix is the image's store, so host store paths are not visible).
KRUN_INIT=$(pick '/cang-toolchain/krun-init')
GBM_LIB=$(pick '/nix/store/*mesa-libgbm-*/lib/libgbm.so')

missing=
if [ -z "$VIRGL_PC" ]; then missing="$missing virglrenderer.pc"; fi
if [ -z "$GBM_PC" ]; then missing="$missing gbm.pc"; fi
if [ -z "$DRM_PC" ]; then missing="$missing libdrm.pc"; fi
if [ -z "$LIBCLANG" ]; then missing="$missing libclang"; fi
if [ -z "$VIRGL_LIB" ]; then missing="$missing libvirglrenderer.so"; fi
if [ -z "$GBM_LIB" ]; then missing="$missing libgbm.so"; fi
if [ -z "$KRUN_INIT" ] && [ -z "${KRUN_INIT_BINARY_PATH:-}" ]; then missing="$missing krun-init at /cang-toolchain/krun-init"; fi

if [ -n "$missing" ]; then
    echo "cang-workload-env: cannot build the cang tree, missing:$missing" >&2
    return 1 2>/dev/null || exit 1
fi

pc_list=
for d in "$VIRGL_PC" "$GBM_PC" "$DRM_PC"; do
    if [ -n "$d" ]; then pc_list="${pc_list:+$pc_list:}$d"; fi
done
if [ -n "$pc_list" ]; then
    export PKG_CONFIG_PATH="$pc_list${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    echo "cang-workload-env: PKG_CONFIG_PATH=$PKG_CONFIG_PATH"
fi
if [ -n "$LIBCLANG" ]; then
    export LIBCLANG_PATH="$LIBCLANG"
    echo "cang-workload-env: LIBCLANG_PATH=$LIBCLANG_PATH"
fi
lib_list=
for f in "$VIRGL_LIB" "$GBM_LIB"; do
    if [ -n "$f" ]; then lib_list="${lib_list:+$lib_list }-L$(dirname "$f")"; fi
done
if [ -n "$KRUN_INIT" ]; then
    export KRUN_INIT_BINARY_PATH="$KRUN_INIT"
    echo "cang-workload-env: KRUN_INIT_BINARY_PATH=$KRUN_INIT_BINARY_PATH"
fi
if [ -n "$lib_list" ]; then
    export RUSTFLAGS="${RUSTFLAGS:-} $lib_list"
    echo "cang-workload-env: RUSTFLAGS=$RUSTFLAGS"
fi
return 0 2>/dev/null || exit 0
