/* launcher-rs.c
 *
 * Host-side driver for the external-render-server venus probe ("run-rs.sh").
 *
 * Mirrors cang's exact render-server path (compare render_server.rs):
 *   - creates a SOCK_SEQPACKET socketpair (parent/child)
 *   - forks virgl_render_server --socket-fd=<child> with the cang-equivalent
 *     render-server environment (LD_LIBRARY_PATH=vulkan-loader:mesa,
 *     VK_DRIVER_FILES=<radeon ICD>, MESA_SHADER_CACHE_DIR=/dev/shm/mesa-cache)
 *   - hands the PARENT end to libkrun's GPU device
 *     (krun_gpu_device_set_render_server_fd, flags 0xe43) -- the ABI-2 spelling
 *     of what v1's krun_set_gpu_options3 took as a fourth argument
 *
 * This is the exact path the in-process launcher.c probe does NOT exercise, and
 * the path under which chromium's venus ring/fence init stalls in production.
 *
 * The ABI-2 VM assembly lives in abivm.h.
 */
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>

#include "abivm.h"

static void exec_render_server(int child_fd, int parent_fd) {
    const char *exec_path = getenv("RENDER_SERVER_EXEC_PATH");
    if (!exec_path || !*exec_path) {
        fprintf(stderr, "launcher-rs: RENDER_SERVER_EXEC_PATH is not set\n");
        _exit(2);
    }
    char socket_opt[64];
    snprintf(socket_opt, sizeof(socket_opt), "--socket-fd=%d", child_fd);

    /* If CANG_BOOTSTRAP=<cang-bin>, use the REAL cang render-server bootstrap
     * (applies Landlock + the packaged render-server seccomp policy, then execs
     * the RS) -- the exact product sandbox.  We pass the real PARENT end as
     * CANG_RENDER_SERVER_PARENT_FD (the bootstrap closes that copy) and the
     * child end as CANG_RENDER_SERVER_CHILD_FD (the RS keeps it).  If
     * SANDBOX_RS=1, apply the seccomp allowlist alone.  Otherwise exec the RS
     * directly (unsandboxed). */
    const char *cang_bin = getenv("CANG_BOOTSTRAP");
    if (cang_bin && *cang_bin) {
        char parent_env[64], child_env[64];
        snprintf(child_env, sizeof(child_env), "CANG_RENDER_SERVER_CHILD_FD=%d", child_fd);
        putenv(child_env);
        snprintf(parent_env, sizeof(parent_env), "CANG_RENDER_SERVER_PARENT_FD=%d", parent_fd);
        putenv(parent_env);
        /* The parent end must be open in the child so the bootstrap's close()
         * actually closes this process's copy (and the RS still has its own). */
        char *argv[] = { (char *)"cang", (char *)"internal",
                         (char *)"render-server-bootstrap", NULL };
        execv(cang_bin, argv);
        fprintf(stderr, "launcher-rs: execv(cang bootstrap) failed errno=%d (%s)\n", errno,
                strerror(errno));
        _exit(2);
    }

    /* If LANDLOCK_RS=<variant>, apply cang's render-server Landlock rule set
     * (as-shipped without REFER, or refer-fix with it) before exec -- the A/B
     * of the Landlock root cause.  Takes precedence over the sandbox modes so
     * Landlock alone can be tested. */
    const char *landlock_variant = getenv("LANDLOCK_RS");
    const char *landlock_path = getenv("LANDLOCK_RS_PATH");
    if (landlock_variant && *landlock_variant && landlock_path && *landlock_path) {
        char *argv[] = { (char *)"landlock-rs", (char *)landlock_variant,
                         (char *)exec_path, socket_opt, NULL };
        execv(landlock_path, argv);
        fprintf(stderr, "launcher-rs: execv(landlock-rs) failed errno=%d (%s)\n", errno,
                strerror(errno));
        _exit(2);
    }

    const char *sandbox_path = getenv("SANDBOX_RS_PATH");
    if (getenv("SANDBOX_RS") && sandbox_path && *sandbox_path) {
        char *argv[] = { (char *)"sandbox-rs", (char *)exec_path, socket_opt, NULL };
        execv(sandbox_path, argv);
        fprintf(stderr, "launcher-rs: execv(sandbox-rs) failed errno=%d (%s)\n", errno,
                strerror(errno));
        _exit(2);
    }

    char *argv[] = { (char *)"virgl_render_server", socket_opt, NULL };
    execv(exec_path, argv);
    fprintf(stderr, "launcher-rs: execv(%s) failed errno=%d (%s)\n", exec_path, errno,
            strerror(errno));
    _exit(2);
}

/* argv[1] = rootfs dir, argv[2] = console output file
 * argv[3] = optional virtio-gpu virgl flag word (default 0xe43) */
int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <rootfs-dir> <console-output-file> [gpu-flags-hex]\n",
                argv[0]);
        return 2;
    }
    const char *rootfs = argv[1];
    const char *console = argv[2];
    uint32_t gpu_flags = FLAGS_VENUS_EXTERNAL;
    if (argc >= 4)
        gpu_flags = (uint32_t)strtoul(argv[3], NULL, 0);

    int fds[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, fds) != 0)
        abivm_fail("socketpair", errno);
    int parent_fd = fds[0];
    int child_fd = fds[1];

    /* Clear CLOEXEC on both ends: the fd must survive the exec of the render
     * server (child end) and the VM-worker fork (parent end). */
    for (int i = 0; i < 2; i++) {
        int fl = fcntl(fds[i], F_GETFD);
        if (fl >= 0)
            fcntl(fds[i], F_SETFD, fl & ~FD_CLOEXEC);
    }

    pid_t pid = fork();
    if (pid < 0)
        abivm_fail("fork", errno);
    if (pid == 0) {
        /* Child: run the render server (direct, or through the sandbox /
         * cang bootstrap).  The parent socketpair end stays open only in
         * CANG_BOOTSTRAP mode (the bootstrap closes its own copy); otherwise
         * the child must not carry it. */
        if (!getenv("CANG_BOOTSTRAP"))
            close(parent_fd);
        exec_render_server(child_fd, parent_fd);
    }
    close(child_fd); /* parent keeps only parent_fd */

    fprintf(stderr,
            "launcher-rs: render server pid=%d socketpair parent_fd=%d child_fd=%d "
            "gpu_flags=0x%x\n",
            (int)pid, parent_fd, child_fd, gpu_flags);

    /* `true`: cang's render-server work is debugged from libkrun's log too. */
    abivm_boot(rootfs, console, gpu_flags, parent_fd, true);
    return 1;
}
