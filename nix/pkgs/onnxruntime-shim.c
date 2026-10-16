/*
 * libonnxruntime.so.1 - the ONNX Runtime ABI shim.
 *
 * Two consumers in this image need `OrtGetApiBase` from a library named
 * `libonnxruntime.so.1`, and they import it under different versions:
 *
 *   * a vendored `onnxruntime-node` binding NEEDs `libonnxruntime.so.1` (its
 *     `RUNPATH $ORIGIN/` points at a directory with no library in it) and imports
 *     `OrtGetApiBase@VERS_1.24.3`;
 *   * `zg` NEEDs the same soname and imports `OrtGetApiBase@VERS_1.27.1`
 *     (nixpkgs-unstable's ONNX Runtime, which its own `RUNPATH` names).
 *
 * No library nixpkgs builds can serve both: each release exports its own node
 * (`@@VERS_1.24.4` for the image's ONNX Runtime, `@@VERS_1.27.1` for the newer
 * one), and the loader checks the node *before* ONNX Runtime's own append-only
 * `OrtApi` negotiation can happen. This file therefore defines the symbol under
 * both node names and forwards both to one real library.
 *
 * The forwarded library must be the newest one any consumer asks for - the
 * `@VERS_1.27.1` caller wants the 1.27 `OrtApi`, while the `@VERS_1.24.3` caller
 * only wants the 1.24 table, which a 1.27 library still serves because ONNX
 * Runtime only appends to that vtable.
 *
 * The file is installed under both names (`libonnxruntime.so.1` for the
 * consumers above, `libonnxrtabi24.so.1` for a binding built with
 * `patchelf --replace-needed` to ask for the shim itself), and its SONAME is
 * `libonnxruntime.so.1`, the name those consumers' `DT_NEEDED` carries. glibc
 * matches a lookup against the *file* name in the search path, so either name
 * serves either consumer.
 *
 * The real library is `dlopen`ed rather than linked, and its directory is read
 * at run time, so another ONNX Runtime can be swapped in without rebuilding this
 * file:
 *
 *   ORT_SHIM_REAL_LIB      an exact file to load; the shim's test knob.
 *   ONNXRUNTIME_LIB_PATH   a directory whose `libonnxruntime.so.1` is loaded.
 *   otherwise              the directory compiled in at build time.
 */

#define _GNU_SOURCE

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define STR_(value) #value
#define STR(value) STR_(value)

#ifndef ONNXRUNTIME_DEFAULT_LIB_DIR
#error "ONNXRUNTIME_DEFAULT_LIB_DIR must name the directory holding the ONNX Runtime this shim loads"
#endif

/* The soname of the library inside that directory. */
static const char *const real_lib_basename = "libonnxruntime.so.1";

typedef const void *(*ort_get_api_base_fn)(void);

static ort_get_api_base_fn real_ort_get_api_base;

static void die(const char *path, const char *reason)
{
    fprintf(stderr,
            "onnxruntime ABI shim (libonnxruntime.so.1): cannot load the real ONNX Runtime"
            " from '%s': %s\n",
            path,
            reason != NULL ? reason : "unknown error");
    abort();
}

static const char *real_lib_dir(void)
{
    const char *dir = getenv("ONNXRUNTIME_LIB_PATH");
    if (dir == NULL || dir[0] == '\0') {
        dir = STR(ONNXRUNTIME_DEFAULT_LIB_DIR);
    }
    return dir;
}

const void *OrtGetApiBase(void)
{
    if (real_ort_get_api_base != NULL) {
        return real_ort_get_api_base();
    }

    const char *exact = getenv("ORT_SHIM_REAL_LIB");
    char path[4096];
    const char *library;

    if (exact != NULL && exact[0] != '\0') {
        library = exact;
    } else {
        const char *dir = real_lib_dir();
        const int written = snprintf(path, sizeof(path), "%s/%s", dir, real_lib_basename);
        if (written < 0 || (size_t)written >= sizeof(path)) {
            die(dir, "the library path is too long");
        }
        library = path;
    }

    void *handle = dlopen(library, RTLD_NOW | RTLD_GLOBAL);
    if (handle == NULL) {
        die(library, dlerror());
    }

    ort_get_api_base_fn entry = (ort_get_api_base_fn)dlsym(handle, "OrtGetApiBase");
    if (entry == NULL) {
        die(library, "the library does not define OrtGetApiBase");
    }

    real_ort_get_api_base = entry;
    return entry();
}

/*
 * The second version node, for the consumers that import `@VERS_1.27.1` (`zg`).
 * The alias is the only place `OrtGetApiBase` is exported under that node; the
 * version script below makes the plain symbol the `@@VERS_1.24.3` default, so a
 * consumer linked against the shim gets the ABI the vendored binding asks for.
 */
const void *ort_get_api_base_v1271(void);
const void *ort_get_api_base_v1271(void)
{
    return OrtGetApiBase();
}

__asm__(".symver ort_get_api_base_v1271,OrtGetApiBase@VERS_1.27.1");
