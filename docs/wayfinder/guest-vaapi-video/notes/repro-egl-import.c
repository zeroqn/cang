
/* Minimal reproducer for the cang guest-encode chroma defect.
 *
 * Mimics what vrend does: create a driver-allocated NV12 surface, export it as a
 * DMA-BUF (VA_EXPORT_SURFACE_SEPARATE_LAYERS | WRITE_ONLY), build one EGLImage per
 * plane with EGL_DMA_BUF_PLANE0_FD/OFFSET/PITCH_EXT (no modifier, like vrend),
 * bind each image to a texture and write a known pattern through GL.
 *
 * Then read the surface back with vaGetImage (the driver's own copy-out, i.e. the
 * layout the encoder itself reads) and with vaDeriveImage, and print both, so the
 * question "did the GL write land where the driver reads it?" is answered in
 * seconds instead of a VM boot. argv[1] = "cpu" writes the pattern through
 * vaDeriveImage+vaMapBuffer as a control.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <va/va.h>
#include <va/va_drm.h>
#include <va/va_drmcommon.h>
#include <epoxy/egl.h>
#include <epoxy/gl.h>

#define W 320
#define H 240

static int fail(const char *what, int code)
{
    fprintf(stderr, "FAIL %s (%d)\n", what, code);
    return 1;
}

/* luma constant 16, chroma: V ramps with Y, U constant 128 */
static void pattern_plane(unsigned plane, unsigned char *dst, unsigned pitch, unsigned rows, unsigned cols)
{
    unsigned y, x;

    for (y = 0; y < rows; y++) {
        for (x = 0; x < cols; x++) {
            unsigned char *p = dst + (size_t)y * pitch + x;
            if (plane == 0)
                *p = 16;
            else
                *p = (x & 1) ? (unsigned char)(16 + 200 * y / rows) : 128;
        }
    }
}

static int dump_back(const char *tag, const unsigned char *base, unsigned pitch, unsigned off, unsigned rows, unsigned cols)
{
    unsigned y;
    int bad = 0;

    printf("%s: pitch=%u off=%u\n", tag, pitch, off);
    for (y = 0; y < rows; y += 4) {
        const unsigned char *r = base + off + (size_t)y * pitch;
        printf("  row %3u: %3u %3u %3u %3u | %3u %3u %3u %3u\n", y,
               r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]);
    }
    return bad;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "egl";
    setvbuf(stdout, NULL, _IONBF, 0);
    int drm_fd, major, minor;
    VADisplay dpy;
    VASurfaceID sfc;
    VAStatus st;
    VADRMPRIMESurfaceDescriptor desc;
    VAImage img;
    void *map = NULL;

    drm_fd = open("/dev/dri/renderD128", O_RDWR);
    if (drm_fd < 0) return fail("open render node", drm_fd);
    dpy = vaGetDisplayDRM(drm_fd);
    st = vaInitialize(dpy, &major, &minor);
    if (st != VA_STATUS_SUCCESS) return fail("vaInitialize", st);
    printf("libva %d.%d\n", major, minor);

    st = vaCreateSurfaces(dpy, VA_RT_FORMAT_YUV420, W, H, &sfc, 1, NULL, 0);
    if (st != VA_STATUS_SUCCESS) return fail("vaCreateSurfaces", st);
    printf("surface %u created %dx%d\n", sfc, W, H);

    st = vaExportSurfaceHandle(dpy, sfc, VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                               VA_EXPORT_SURFACE_SEPARATE_LAYERS | VA_EXPORT_SURFACE_WRITE_ONLY, &desc);
    if (st != VA_STATUS_SUCCESS) return fail("vaExportSurfaceHandle", st);
    printf("export: fourcc=0x%08x %ux%u objects=%u layers=%u\n",
           desc.fourcc, desc.width, desc.height, desc.num_objects, desc.num_layers);
    for (unsigned i = 0; i < desc.num_layers; i++) {
        printf("  layer[%u]: fmt=0x%08x planes=%u obj=%u off=%u pitch=%u modifier=0x%llx\n",
               i, desc.layers[i].drm_format, desc.layers[i].num_planes,
               desc.layers[i].object_index[0], desc.layers[i].offset[0], desc.layers[i].pitch[0],
               (unsigned long long)desc.objects[desc.layers[i].object_index[0]].drm_format_modifier);
    }

    if (strcmp(mode, "cpu") == 0) {
        unsigned i;
        st = vaDeriveImage(dpy, sfc, &img);
        if (st != VA_STATUS_SUCCESS) return fail("vaDeriveImage(cpu)", st);
        st = vaMapBuffer(dpy, img.buf, &map);
        if (st != VA_STATUS_SUCCESS) return fail("vaMapBuffer(cpu)", st);
        for (i = 0; i < img.num_planes && i < 2; i++)
            pattern_plane(i, (unsigned char *)map + img.offsets[i], img.pitches[i],
                          i == 0 ? H : H / 2, i == 0 ? W : W / 2);
        vaUnmapBuffer(dpy, img.buf);
        vaDestroyImage(dpy, img.image_id);
        printf("mode cpu: pattern written through vaDeriveImage\n");
    } else {
        EGLDisplay edpy;
        EGLContext ctx;
        EGLSurface surf;
        EGLint cfg_attrs[] = { EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
                               EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
                               EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8,
                               EGL_NONE };
        EGLConfig cfg;
        EGLint ncfg = 0, major_e, minor_e;

        edpy = eglGetDisplay((EGLNativeDisplayType)EGL_DEFAULT_DISPLAY);
        if (edpy == EGL_NO_DISPLAY) {
            edpy = eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
        }
        if (edpy == EGL_NO_DISPLAY) return fail("eglGetDisplay", 0);
        if (!eglInitialize(edpy, &major_e, &minor_e)) return fail("eglInitialize", eglGetError());
        printf("egl %d.%d\n", major_e, minor_e);
        if (!eglChooseConfig(edpy, cfg_attrs, &cfg, 1, &ncfg) || !ncfg)
            return fail("eglChooseConfig", eglGetError());
        {
            EGLint cattr[] = { EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE };
            ctx = eglCreateContext(edpy, cfg, EGL_NO_CONTEXT, cattr);
            if (ctx == EGL_NO_CONTEXT)
                ctx = eglCreateContext(edpy, cfg, EGL_NO_CONTEXT, NULL);
        }
        if (ctx == EGL_NO_CONTEXT) return fail("eglCreateContext", eglGetError());
        surf = eglCreatePbufferSurface(edpy, cfg, NULL);
        if (surf == EGL_NO_SURFACE) return fail("eglCreatePbufferSurface", eglGetError());
        if (!eglMakeCurrent(edpy, surf, surf, ctx)) return fail("eglMakeCurrent", eglGetError());
        printf("gl: %s / %s\n", glGetString(GL_VENDOR), glGetString(GL_RENDERER));

        {
            unsigned i;
            int any = 0;

            for (i = 0; i < desc.num_layers && i < 2; i++) {
                EGLint use_pitch = (EGLint)desc.layers[i].pitch[0];
                EGLint use_w = (EGLint)(W / (i + 1)), use_h = (EGLint)(H / (i + 1));

                if (argc > 2 && i == 1)
                    use_pitch = (EGLint)atoi(argv[2]);
                if (argc > 3 && i == 1)
                    use_w = (EGLint)atoi(argv[3]);
                EGLint attrs[24] = {
                    EGL_LINUX_DRM_FOURCC_EXT, (EGLint)desc.layers[i].drm_format,
                    EGL_WIDTH, use_w,
                    EGL_HEIGHT, use_h,
                    EGL_DMA_BUF_PLANE0_FD_EXT, desc.objects[desc.layers[i].object_index[0]].fd,
                    EGL_DMA_BUF_PLANE0_OFFSET_EXT, (EGLint)desc.layers[i].offset[0],
                    EGL_DMA_BUF_PLANE0_PITCH_EXT, use_pitch,
                    EGL_NONE, EGL_NONE, EGL_NONE, EGL_NONE,
                    EGL_NONE, EGL_NONE, EGL_NONE, EGL_NONE
                };
                EGLImageKHR eimg;
                unsigned nattrs = 7;
                GLuint tex, fbo;
                unsigned char *buf;
                unsigned pitch, rows, cols, r, c;

                if (strcmp(mode, "eglmod") == 0) {
                    unsigned long long mod = desc.objects[desc.layers[i].object_index[0]].drm_format_modifier;
                    attrs[nattrs*2] = EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT;
                    attrs[nattrs*2+1] = (EGLint)(mod & 0xffffffffULL);
                    nattrs++;
                    attrs[nattrs*2] = EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT;
                    attrs[nattrs*2+1] = (EGLint)(mod >> 32);
                    nattrs++;
                    printf("  plane %u: passing modifier 0x%llx\n", i, mod);
                }
                eglGetError();
                eimg = eglCreateImageKHR(edpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attrs);
                printf("  plane %u: eglCreateImage -> %p err=0x%x\n", i, (void *)eimg, eglGetError());
                if (eimg == EGL_NO_IMAGE_KHR) continue;
                {
                    GLuint tex = 0;

                    glGenTextures(1, &tex);
                    glBindTexture(GL_TEXTURE_2D, tex);
                    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
                    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
                    glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, (GLeglImageOES)eimg);

                    pitch = desc.layers[i].pitch[0];
                    rows = i == 0 ? H : H / 2;
                    cols = i == 0 ? W : (unsigned)use_w;
                    unsigned bpp = (i == 0) ? 1 : 2;
                    unsigned stride = cols * bpp;

                    buf = calloc(1, stride * rows);
                    if (i == 0) {
                        memset(buf, 16, stride * rows);
                        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, cols, rows, GL_RED, GL_UNSIGNED_BYTE, buf);
                    } else {
                        for (r = 0; r < rows; r++)
                            for (c = 0; c < cols; c++) {
                                buf[r * stride + 2 * c] = 128;
                                buf[r * stride + 2 * c + 1] = (unsigned char)(16 + 200 * r / rows);
                            }
                        glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
                        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, cols, rows, GL_RG, GL_UNSIGNED_BYTE, buf);
                    }
                    printf("  plane %u: pattern uploaded through the EGL texture (gl error 0x%x)\n",
                           i, glGetError());
                    glFinish();
                    free(buf);
                    any = 1;
                }
            }
            printf("mode egl: pattern written through %d EGL images\n", any);
        }
    }

    /* read back with the driver's own copy-out */
    st = vaGetImage(dpy, sfc, 0, 0, W, H, (VAImageID)0);
    printf("vaGetImage (no image to fill) st=0x%x\n", st);
    {
        VAImage vi;
        void *vmap = NULL;
        VAImageFormat fmt;

        memset(&fmt, 0, sizeof(fmt));
        fmt.fourcc = VA_FOURCC_NV12;
        fmt.byte_order = VA_LSB_FIRST;
        fmt.bits_per_pixel = 12;
        st = vaCreateImage(dpy, &fmt, W, H, &vi);
        printf("vaCreateImage st=0x%x id=%u\n", st, vi.image_id);
        st = vaGetImage(dpy, sfc, 0, 0, W, H, vi.image_id);
        printf("vaGetImage st=0x%x\n", st);
        if (st == VA_STATUS_SUCCESS && vi.num_planes >= 2) {
            st = vaMapBuffer(dpy, vi.buf, &vmap);
            if (st == VA_STATUS_SUCCESS && vmap) {
                desc.layers[0].pitch[0] = vi.pitches[0];
                printf("vaGetImage view: pitches %u/%u offsets %u/%u\n",
                       vi.pitches[0], vi.pitches[1], vi.offsets[0], vi.offsets[1]);
                dump_back("  vaGetImage plane0", vmap, vi.pitches[0], vi.offsets[0], H, W);
                dump_back("  vaGetImage plane1", vmap, vi.pitches[1], vi.offsets[1], H / 2, W / 2);
                vaUnmapBuffer(dpy, vi.buf);
            }
        }
        vaDestroyImage(dpy, vi.image_id);
    }

    {
        VAImage di;
        void *dmap = NULL;

        st = vaDeriveImage(dpy, sfc, &di);
        printf("vaDeriveImage st=0x%x\n", st);
        if (st == VA_STATUS_SUCCESS) {
            st = vaMapBuffer(dpy, di.buf, &dmap);
            if (st == VA_STATUS_SUCCESS && dmap) {
                printf("vaDeriveImage view: pitches %u/%u offsets %u/%u\n",
                       di.pitches[0], di.pitches[1], di.offsets[0], di.offsets[1]);
                dump_back("  derived plane0", dmap, di.pitches[0], di.offsets[0], H, W);
                dump_back("  derived plane1", dmap, di.pitches[1], di.offsets[1], H / 2, W / 2);
                vaUnmapBuffer(dpy, di.buf);
            }
            vaDestroyImage(dpy, di.image_id);
        }
    }

    (void)map; (void)img;
    vaDestroySurfaces(dpy, &sfc, 1);
    vaTerminate(dpy);
    close(drm_fd);
    return 0;
}
