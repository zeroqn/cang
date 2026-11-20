/* launcher.c
 *
 * Host-side driver for the in-guest virgl venus probe.  It boots a bare libkrun
 * microVM with a virtio-gpu device configured for the VENUS renderer (the path
 * a libkrun guest uses for Vulkan), sets the guest rootfs, routes the guest
 * console to a host file, and hands control to libkrun.
 *
 * The ABI-2 call ordering lives in abivm.h; this file is the argument handling
 * and the in-process (no external render server) variant.
 *
 * krun_vmm_run does not return on success -- it takes over the process and
 * exits with the guest's exit code.  The probe verdict is written to the guest
 * console, which this launcher points at the host file named on the command
 * line (see abivm_open_console); run.sh greps it for RESULT.
 *
 * Flag set: VENUS(1<<6) | NO_VIRGL(1<<7) | RENDER_SERVER(1<<9) = 0x2c0.  This is
 * the host-side combination the virgl-render-server-probe proved creates a
 * venus context; render-server is enabled because that is the isolated path the
 * cang sandbox is expected to trap on, and it is honored by venus.
 */
#include "abivm.h"

/* argv[1] = path to the guest rootfs directory (built by guest-rootfs.nix)
 * argv[2] = path to the host file that should receive the guest console output
 * argv[3] = optional virtio-gpu virgl flag word (default 0x2c0) */
int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr,
                "usage: %s <rootfs-dir> <console-output-file> [gpu-flags-hex]\n",
                argv[0]);
        return 2;
    }
    uint32_t gpu_flags = FLAGS_VENUS_RS;
    if (argc >= 4)
        gpu_flags = (uint32_t)strtoul(argv[3], NULL, 0);

    abivm_boot(argv[1], argv[2], gpu_flags, -1, false);
    return 1;
}
