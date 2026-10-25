#!/bin/sh
# Resolve, from the /nix store the guest already sees, the build-time paths a
# cang-tree rust build needs: libkrun's build scripts want virglrenderer, gbm,
# libdrm and libclang. Prints shell assignments.
#
# Resolve by glob, never by a pinned store hash: the store is the image's closure
# (plus whatever the harness bind-mounts), only as of the commit in use, and a
# hard-coded hash would silently rot. A missing piece is exported as a
# deliberately nonexistent path and reported, so the build fails loudly with the
# name of what is missing instead of silently picking up something else.
set -u

found=
pick() { # pick <glob> -> first existing match, else empty
    for p in $1; do
        if [ -e "$p" ]; then printf '%s' "$p"; return 0; fi
    done
    return 1
}

VIRGL_PC=$(pick '/nix/store/*virglrenderer-*/lib/pkgconfig')
GBM_PC=$(pick '/nix/store/*mesa-libgbm-*/lib/pkgconfig')
DRM_PC=$(pick '/nix/store/*libdrm-*-dev/lib/pkgconfig')
LIBCLANG=$(pick '/nix/store/*clang-*-lib/lib')
VIRGL_LIB=$(pick '/nix/store/*virglrenderer-*/lib/libvirglrenderer.so')
GBM_LIB=$(pick '/nix/store/*mesa-libgbm-*/lib/libgbm.so')

for name in virglrenderer.pc:$VIRGL_PC gbm.pc:$GBM_PC libdrm.pc:$DRM_PC libclang:$LIBCLANG libvirglrenderer.so:$VIRGL_LIB libgbm.so:$GBM_LIB; do
    label=${name%%:*}
    value=${name#*:}
    if [ -z "$value" ]; then
        echo "cang-workload-env: missing $label in this /nix store" >&2
        found="$found $label"
    fi
done

pc_list=
for d in "$VIRGL_PC" "$GBM_PC" "$DRM_PC"; do
    [ -n "$d" ] && pc_list="${pc_list:+$pc_list:}$d"
done
[ -n "$pc_list" ] && printf 'export PKG_CONFIG_PATH=%s${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}\n' "$pc_list"
[ -n "$LIBCLANG" ] && printf 'export LIBCLANG_PATH=%s\n' "$LIBCLANG"
lib_list=
for f in "$VIRGL_LIB" "$GBM_LIB"; do
    [ -n "$f" ] && lib_list="${lib_list:+$lib_list }-L$(dirname "$f")"
done
[ -n "$lib_list" ] && printf 'export RUSTFLAGS="${RUSTFLAGS:-} %s"\n' "$lib_list"

if [ -n "$found" ]; then
    echo "cang-workload-env: cannot build the cang tree, missing:$found" >&2
    exit 1
fi
exit 0
