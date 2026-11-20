/* abivm.h
 *
 * The ABI-2 libkrun boot path shared by launcher.c and launcher-rs.c.
 *
 * libkrun 2.0 has no global VM context: a VM is assembled from a payload
 * (kernel + firmware), an init config that describes the guest command, and a
 * device manager whose devices are all consumed by the VMM builder.  The
 * ordering here is the one deps/libkrun/examples/chroot_vm.c uses:
 *
 *   krun_init_log -> krun_fs_overlay_new -> krun_payload_load_krunfw
 *   -> init-config builder/apply -> krun_mmio_device_manager_new
 *   -> devices (console, virtio-fs root, virtio-gpu) -> krun_vmm_builder_*
 *   -> krun_vmm_builder_build -> krun_vmm_run
 *
 * The v1 entry points these launchers were originally written against
 * (krun_create_ctx / krun_set_root / krun_set_gpu_options2/3 / krun_set_exec /
 * krun_start_enter) no longer exist in the header.
 *
 * Include this from a single .c file (everything here is static).
 */
#ifndef VIRGL_GUEST_PROBE_ABIVM_H
#define VIRGL_GUEST_PROBE_ABIVM_H

#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <libkrun.h>
#include <libkrun_display.h>
#include <libkrun_init.h>

/* The virglrenderer flag bits libkrun forwards verbatim to virglrenderer.
 * libkrun.h exports the ones its own bitflags know (KRUN_VIRGL_RENDERER_FLAGS_*);
 * the wire value is a plain u32, so the bits it does not name are spelled out
 * here with virglrenderer's values. */
#define VIRGL_USE_EGL (1u << 0)
#define VIRGL_THREAD_SYNC (1u << 1)
#define VIRGL_VENUS (1u << 6)
#define VIRGL_NO_VIRGL (1u << 7)
#define VIRGL_RENDER_SERVER (1u << 9)
#define VIRGL_DRM (1u << 10)
#define VIRGL_USE_VIDEO (1u << 11)

/* venus through an in-process virglrenderer (launcher.c) */
#define FLAGS_VENUS_RS (VIRGL_VENUS | VIRGL_NO_VIRGL | VIRGL_RENDER_SERVER)
/* cang's VIRGLRENDERER_VENUS_FLAGS: the external render-server path with the
 * DRM (host GPU) and video flags the product asks for (launcher-rs.c) */
#define FLAGS_VENUS_EXTERNAL                                                       \
    (VIRGL_USE_EGL | VIRGL_THREAD_SYNC | VIRGL_VENUS | VIRGL_RENDER_SERVER |       \
     VIRGL_DRM | VIRGL_USE_VIDEO)

/* The virtio-gpu shared-memory window: cang maps venus blob resources through
 * slices of it (`virtio_gpu::resource_map_blob`), so its size is the cumulative
 * budget for live mappings.  256 MiB is far more than a device creation plus a
 * fence round trip needs -- the product's `--gpu=drm:shm=` is what raises it. */
#define GPU_SHM_SIZE_BYTES (256ull * 1024 * 1024)

static int abivm_diag_fd = 2;

static void abivm_fail(const char *what, int rc) {
    dprintf(abivm_diag_fd, "launcher: %s failed rc=%d errno=%d (%s)\n", what, rc, errno,
            strerror(errno));
    exit(1);
}

static bool abivm_push_to_stderr(void *userdata, KrunStr s) {
    (void)userdata;
    fwrite(s.data, 1, s.len, stderr);
    return true;
}

static KrunVtableHandle abivm_init_writer = KRUN_VTABLE_HANDLE(
    KRUN_INIT_PUSH_STR_TYPE_TAG,
    ((KrunInitPushStrVtable){ .drop = NULL, .push = abivm_push_to_stderr }),
    NULL);
static KrunVtableHandle abivm_krun_writer = KRUN_VTABLE_HANDLE(
    KRUN_PUSH_STR_TYPE_TAG,
    ((KrunPushStrVtable){ .drop = NULL, .push = abivm_push_to_stderr }),
    NULL);

/* Every ABI-2 call reports failure through an out-parameter error handle (both
 * error types are opaque `void*`).  These turn "handle set" into a message plus
 * exit, so the call sites read like the v1 `if (rc < 0) fail(...)`. */
static void abivm_init_error_exit(KrunInitError *err, const char *what) {
    if (!*err)
        return;
    fprintf(stderr, "%s failed: ", what);
    krun_init_error_message(*err, &abivm_init_writer);
    fputc('\n', stderr);
    krun_init_error_destroy(*err);
    exit(1);
}

static void abivm_krun_error_exit(KrunError *err, const char *what) {
    if (!*err)
        return;
    fprintf(stderr, "%s failed: ", what);
    krun_error_message(*err, &abivm_krun_writer);
    fputc('\n', stderr);
    krun_error_destroy(*err);
    exit(1);
}

#define ABIVM_CHECK_INIT(err, call)                                                \
    do {                                                                           \
        (err) = NULL;                                                              \
        call;                                                                      \
        abivm_init_error_exit(&(err), #call);                                      \
    } while (0)

#define ABIVM_CHECK(err, call)                                                     \
    do {                                                                           \
        (err) = NULL;                                                              \
        call;                                                                      \
        abivm_krun_error_exit(&(err), #call);                                      \
    } while (0)

/* The display backend a GPU device is built on.
 *
 * `krun_gpu_device_new` takes a `KrunDisplayBackend` handle instead of the v1
 * "no display" default, and `krun_display_backend_new` rejects a backend that
 * advertises BASIC_FRAMEBUFFER without the four methods behind it.  cang drives
 * the GPU headless -- venus renders through the render server and nothing ever
 * scans out -- so this advertises the feature with methods that refuse every
 * request, turning a scanout nobody set up into an explicit error instead of a
 * null call.  Mirrors crates/cang-libkrun/src/display.rs. */
static int32_t abivm_display_disable_scanout(void *instance, uint32_t scanout_id) {
    (void)instance;
    (void)scanout_id;
    return KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED;
}

static int32_t abivm_display_configure_scanout(void *instance, uint32_t scanout_id,
                                               uint32_t display_width,
                                               uint32_t display_height, uint32_t width,
                                               uint32_t height, uint32_t format) {
    (void)instance;
    (void)scanout_id;
    (void)display_width;
    (void)display_height;
    (void)width;
    (void)height;
    (void)format;
    return KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED;
}

static int32_t abivm_display_alloc_frame(void *instance, uint32_t scanout_id, uint8_t **buffer,
                                         size_t *buffer_size) {
    (void)instance;
    (void)scanout_id;
    (void)buffer;
    (void)buffer_size;
    return KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED;
}

static int32_t abivm_display_present_frame(void *instance, uint32_t scanout_id,
                                           uint32_t frame_id,
                                           const struct krun_rect *damage_area) {
    (void)instance;
    (void)scanout_id;
    (void)frame_id;
    (void)damage_area;
    return KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED;
}

static KrunDisplayBackend abivm_headless_display_backend(KrunError *err) {
    struct krun_display_backend config;
    memset(&config, 0, sizeof(config));
    config.features = KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER;
    config.create_userdata = NULL;
    config.create = NULL;
    config.vtable.basic_framebuffer.destroy = NULL;
    config.vtable.basic_framebuffer.disable_scanout = abivm_display_disable_scanout;
    config.vtable.basic_framebuffer.configure_scanout = abivm_display_configure_scanout;
    config.vtable.basic_framebuffer.alloc_frame = abivm_display_alloc_frame;
    config.vtable.basic_framebuffer.present_frame = abivm_display_present_frame;

    KrunDisplayBackend backend = krun_display_backend_new(&config, sizeof(config), err);
    if (!backend)
        abivm_fail("krun_display_backend_new", -1);
    return backend;
}

/* Redirect this process's stdout/stderr at the named console file and return an
 * fd for the guest's stdin.
 *
 * The guest console (port 0 / hvc0) writes through libkrun's log at error
 * level, i.e. to this process's stderr, and the krun-stdout / krun-stderr
 * redirect ports write to whatever descriptors the console device was built
 * with.  Pointing both at the console file is what puts the kernel messages,
 * /init's output and the probe's RESULT line in one place -- the same thing
 * `run.sh` used to get by redirecting the whole launcher.  Launcher diagnostics
 * keep a copy of the original stderr, and stdin is /dev/null so that no fd the
 * console builder sees is a terminal (a terminal would turn port 0 into a raw
 * TTY console and take over the caller's terminal). */
static int abivm_open_console(const char *console) {
    int console_fd = open(console, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
    if (console_fd < 0)
        abivm_fail("open(console)", -1);
    abivm_diag_fd = dup(STDERR_FILENO);
    if (abivm_diag_fd < 0)
        abivm_fail("dup(stderr)", -1);
    if (dup2(console_fd, STDOUT_FILENO) < 0 || dup2(console_fd, STDERR_FILENO) < 0)
        abivm_fail("dup2(console)", -1);
    int devnull_fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    if (devnull_fd < 0)
        abivm_fail("open(/dev/null)", -1);
    return devnull_fd;
}

/* Attach the venus virtio-gpu device to `devices`.
 *
 * `render_server_fd` is the caller's end of the render-server socketpair, or -1
 * for an in-process virglrenderer (the v1 `krun_set_gpu_options2` shape). */
static void abivm_add_gpu(KrunDeviceManager devices, uint32_t gpu_flags, int render_server_fd,
                          KrunError *err) {
    KrunError backend_err = NULL;
    KrunDisplayBackend backend = abivm_headless_display_backend(&backend_err);
    abivm_krun_error_exit(&backend_err, "krun_display_backend_new");
    KrunGpuDevice gpu = krun_gpu_device_new(gpu_flags, backend);
    if (!gpu)
        abivm_fail("krun_gpu_device_new", -1);
    if (render_server_fd >= 0)
        ABIVM_CHECK(*err,
                    krun_gpu_device_set_render_server_fd(&gpu, render_server_fd, err));
    krun_gpu_device_shm_size(&gpu, (size_t)GPU_SHM_SIZE_BYTES);
    krun_device_manager_add(devices, gpu);
}

/* Build the VM and run it.  Does not return on success: krun_vmm_run ends the
 * process with the guest's exit code. */
static void abivm_boot(const char *rootfs, const char *console, uint32_t gpu_flags,
                       int render_server_fd, bool debug_log) {
    int devnull_fd = abivm_open_console(console);

    KrunError kerr = NULL;
    KrunInitError ierr = NULL;

    ABIVM_CHECK(kerr, krun_init_log(STDERR_FILENO,
                                    debug_log ? KRUN_LOG_LEVEL_DEBUG : KRUN_LOG_LEVEL_WARN,
                                    KRUN_LOG_STYLE_NEVER, 0, &kerr));

    KrunFsOverlay overlay = krun_fs_overlay_new();

    KrunPayload payload = krun_payload_load_krunfw(&kerr);
    if (!payload)
        abivm_fail("krun_payload_load_krunfw", -1);

    /* The guest command: the rootfs /init, which mounts /dev, /proc, /sys, sets
     * the Mesa venus discovery env and runs guest-probe.  In v1 this was
     * krun_set_exec(ctx, "/init", ...); ABI-2 puts it in the init config, which
     * also installs the guest init binary and its kernel command line. */
    KrunInitConfig cfg;
    {
        KrunInitBuilder b = krun_init_config_builder();
        krun_init_builder_arg(&b, KRUN_STR("/init"));
        krun_init_builder_workdir(&b, KRUN_STR("/"));
        cfg = krun_init_builder_build(&b);
        ABIVM_CHECK_INIT(ierr, krun_init_config_apply(cfg, overlay, payload, &ierr));
    }

    KrunDeviceManager devices = krun_mmio_device_manager_new();

    {
        KrunConsoleBuilder cb = krun_console_device_builder();
        ABIVM_CHECK(kerr, krun_console_builder_add_default_console(
                               cb, devnull_fd, STDOUT_FILENO, STDERR_FILENO, &kerr));
        KrunConsoleDevice console_device = krun_console_builder_build(cb, &kerr);
        if (!console_device)
            abivm_fail("krun_console_builder_build", -1);
        krun_device_manager_add(devices, console_device);
    }

    {
        /* The virtio-fs root the guest init mounts as /. */
        KrunFsDevice root =
            krun_fs_device_new(KRUN_STR("/dev/root"), KRUN_STR(rootfs), &kerr);
        if (!root)
            abivm_fail("krun_fs_device_new(/dev/root)", -1);
        krun_fs_device_set_overlay(root, overlay);
        krun_device_manager_add(devices, root);
    }

    abivm_add_gpu(devices, gpu_flags, render_server_fd, &kerr);

    KrunVmmBuilder builder = krun_vmm_builder_new();
    ABIVM_CHECK(kerr, krun_vmm_builder_vcpus(&builder, 2, &kerr));
    ABIVM_CHECK(kerr, krun_vmm_builder_ram_mib(&builder, 512, &kerr));
    krun_vmm_builder_payload(&builder, payload);
    krun_vmm_builder_devices(&builder, devices);
    ABIVM_CHECK(kerr, krun_vmm_builder_split_irqchip(&builder, false, &kerr));
    ABIVM_CHECK(kerr, krun_vmm_builder_acpi(&builder, false, &kerr));

    /* The init config is borrowed by the running VM; keep it referenced. */
    (void)cfg;

    fprintf(stderr,
            "launcher: booting rootfs=%s gpu_flags=0x%x shm=%lluMiB render_server_fd=%d "
            "console=%s\n",
            rootfs, gpu_flags, (unsigned long long)(GPU_SHM_SIZE_BYTES >> 20),
            render_server_fd, console);

    KrunVmm vmm = krun_vmm_builder_build(&builder, &kerr);
    if (!vmm)
        abivm_fail("krun_vmm_builder_build", -1);
    krun_vmm_run(vmm);
    /* On success the VM takes over this process and exits; reaching here means
     * the microVM never ran a guest. */
    abivm_fail("krun_vmm_run", -1);
}

#endif /* VIRGL_GUEST_PROBE_ABIVM_H */
