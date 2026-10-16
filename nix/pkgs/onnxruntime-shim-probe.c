/*
 * A stand-in for the two consumers of the ONNX Runtime ABI shim:
 *
 *   * a vendored `onnxruntime-node` binding, which NEEDs `libonnxruntime.so.1`
 *     (or the shim's own name, after `patchelf --replace-needed`), imports
 *     `OrtGetApiBase@VERS_1.24.3`, and asks the returned `OrtApiBase` for the
 *     1.24 `OrtApi` - the default build here;
 *   * `zg`, which NEEDs the same soname, imports `OrtGetApiBase@VERS_1.27.1` and
 *     asks for the 1.27 `OrtApi` - the `PROBE_SYMVER`/`PROBE_ORT_API_VERSION=27`
 *     build.
 *
 * Linking this against the shim (rather than against the real library) is what
 * gives it the consumer's versioned import, so running it fails the way that
 * consumer fails if the shim is missing, misnamed, or does not forward to a
 * usable ONNX Runtime. It is deliberately built *without* an rpath: the shim is
 * found through `LD_LIBRARY_PATH`, which is what the image's pi wrapper
 * (`.../cang-native-addon-runtime/lib`) and the nix check both set, so the same
 * binary is also the live evidence for that wiring.
 *
 * The three shipped builds are:
 *
 *   probe-binding        NEEDED libonnxruntime.so.1, `@VERS_1.24.3`, GetApi(24)
 *   probe-zg             NEEDED libonnxruntime.so.1, `@VERS_1.27.1`, GetApi(27)
 *   probe-patched-name   as probe-binding, after
 *                        `patchelf --replace-needed libonnxruntime.so.1
 *                        libonnxrtabi24.so.1`, the shape a binding built to ask
 *                        the shim for itself has.
 */

#include <stdio.h>
#include <stdlib.h>

#include <onnxruntime_c_api.h>

#ifndef PROBE_ORT_API_VERSION
#define PROBE_ORT_API_VERSION 24u
#endif

#ifdef PROBE_SYMVER
/* Declared by the header above; the directive versions the reference. */
__asm__(PROBE_SYMVER);
#endif

int main(void)
{
    const OrtApiBase *api_base = OrtGetApiBase();
    if (api_base == NULL) {
        fprintf(stderr, "probe: OrtGetApiBase() returned NULL\n");
        return 1;
    }

    const OrtApi *api = api_base->GetApi(PROBE_ORT_API_VERSION);
    if (api == NULL) {
        fprintf(stderr,
                "probe: GetApi(%u) returned NULL for onnxruntime %s\n",
                PROBE_ORT_API_VERSION,
                api_base->GetVersionString());
        return 1;
    }

    printf("probe: onnxruntime %s answered GetApi(%u)\n",
           api_base->GetVersionString(),
           PROBE_ORT_API_VERSION);
    return 0;
}
