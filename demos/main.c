#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <drm.h>
#include <drm_mode.h>
#include <drm_fourcc.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
#include <gbm.h>

#define EGL_PLATFORM_GBM_KHR 0x31D7

/* Some older gbm.h only define the enum, not the fourcc macros.
   gbm_surface_create() wants a fourcc on modern Mesa. */
#ifndef GBM_FORMAT_ARGB8888
#define GBM_FORMAT_ARGB8888 GBM_BO_FORMAT_ARGB8888
#endif

#define CHECK(cond, ...) do { \
    if (!(cond)) { \
        fprintf(stderr, "ERROR at %s:%d: ", __FILE__, __LINE__); \
        fprintf(stderr, __VA_ARGS__); \
        fprintf(stderr, " (errno=%d, %s)\n", errno, strerror(errno)); \
        exit(1); \
    } \
} while(0)

#define EGL_CHECK(val, ok, call) do { \
    if (!(ok)) { \
        EGLint err = eglGetError(); \
        fprintf(stderr, "EGL ERROR at %s:%d: %s failed (eglGetError=0x%x)\n", \
                __FILE__, __LINE__, call, err); \
        exit(1); \
    } \
} while(0)

static void dump_drm_topology(int fd, drmModeRes *res) {
    printf("--- DRM topology ---\n");
    printf("CRTCS: %d  Connectors: %d  Encoders: %d  FBs: %d\n",
           res->count_crtcs, res->count_connectors,
           res->count_encoders, res->count_fbs);

    for (int i = 0; i < res->count_crtcs; ++i) {
        drmModeCrtc *crtc = drmModeGetCrtc(fd, res->crtcs[i]);
        if (!crtc) continue;
        printf("  CRTC %u: buffer=%u  mode=%dx%d@%d\n",
               crtc->crtc_id, crtc->buffer_id,
               crtc->mode.hdisplay, crtc->mode.vdisplay,
               crtc->mode.vrefresh);
        drmModeFreeCrtc(crtc);
    }

    for (int i = 0; i < res->count_connectors; ++i) {
        drmModeConnector *conn = drmModeGetConnector(fd, res->connectors[i]);
        if (!conn) continue;
        printf("  Connector %u: %s  encoder=%u  modes=%d\n",
               conn->connector_id,
               conn->connection == DRM_MODE_CONNECTED ? "CONNECTED" : "disconnected",
               conn->encoder_id, conn->count_modes);
        if (conn->connection == DRM_MODE_CONNECTED && conn->count_modes > 0) {
            printf("    Preferred mode: %dx%d@%d\n",
                   conn->modes[0].hdisplay, conn->modes[0].vdisplay,
                   conn->modes[0].vrefresh);
        }
        drmModeFreeConnector(conn);
    }

    for (int i = 0; i < res->count_encoders; ++i) {
        drmModeEncoder *enc = drmModeGetEncoder(fd, res->encoders[i]);
        if (!enc) continue;
        printf("  Encoder %u: crtc=%u  possible_crtcs=0x%x\n",
               enc->encoder_id, enc->crtc_id, enc->possible_crtcs);
        drmModeFreeEncoder(enc);
    }
    printf("--------------------\n");
}

int main() {
    // --- Step 1: Open DRM device ---
    int drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(drm_fd >= 0, "open /dev/dri/card0");

    // --- Step 2: Initialize GBM ---
    struct gbm_device *gbm_dev = gbm_create_device(drm_fd);
    CHECK(gbm_dev, "gbm_create_device");

    // --- Step 3: Initialize EGL ---
    PFNEGLGETPLATFORMDISPLAYEXTPROC eglGetPlatformDisplayEXT =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    CHECK(eglGetPlatformDisplayEXT, "eglGetProcAddress(eglGetPlatformDisplayEXT)");

    EGLDisplay egl_dpy = eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR, gbm_dev, NULL);
    EGL_CHECK(egl_dpy, egl_dpy != EGL_NO_DISPLAY, "eglGetPlatformDisplayEXT");

    EGLBoolean ok = eglInitialize(egl_dpy, NULL, NULL);
    EGL_CHECK(ok, ok, "eglInitialize");

    ok = eglBindAPI(EGL_OPENGL_ES_API);
    EGL_CHECK(ok, ok, "eglBindAPI");

    // --- Step 4: Choose EGL config ---
    EGLConfig config;
    EGLint num_configs;
    const EGLint config_attribs[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RED_SIZE, 8,
        EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8,
        EGL_ALPHA_SIZE, 8,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_NONE
    };
    ok = eglChooseConfig(egl_dpy, config_attribs, &config, 1, &num_configs);
    EGL_CHECK(ok, ok && num_configs > 0, "eglChooseConfig");

    // --- Step 5: Get display resources and connector ---
    drmModeRes *res = drmModeGetResources(drm_fd);
    CHECK(res, "drmModeGetResources");

    CHECK(res->count_connectors > 0, "no connectors found");

    drmModeConnector *conn = NULL;
    for (int i = 0; i < res->count_connectors; ++i) {
        conn = drmModeGetConnector(drm_fd, res->connectors[i]);
        if (conn && conn->connection == DRM_MODE_CONNECTED && conn->count_modes > 0) {
            break;
        }
        drmModeFreeConnector(conn);
        conn = NULL;
    }
    CHECK(conn, "no connected connector found");

    // Pick preferred mode, or the first mode
    drmModeModeInfo *mode = NULL;
    for (int i = 0; i < conn->count_modes; ++i) {
        if (conn->modes[i].type & DRM_MODE_TYPE_PREFERRED) {
            mode = &conn->modes[i];
            break;
        }
    }
    if (!mode) {
        mode = &conn->modes[0];
    }
    printf("Using mode: %dx%d@%d\n", mode->hdisplay, mode->vdisplay, mode->vrefresh);

    // --- Find a suitable CRTC for the connector ---
    uint32_t crtc_id = 0;
    drmModeEncoder *enc = drmModeGetEncoder(drm_fd, conn->encoder_id);
    if (enc) {
        crtc_id = enc->crtc_id;
        drmModeFreeEncoder(enc);
    } else {
        for (int i = 0; i < conn->count_encoders; ++i) {
            enc = drmModeGetEncoder(drm_fd, conn->encoders[i]);
            if (!enc) continue;
            for (int j = 0; j < res->count_crtcs; ++j) {
                if (enc->possible_crtcs & (1 << j)) {
                    crtc_id = res->crtcs[j];
                    break;
                }
            }
            drmModeFreeEncoder(enc);
            if (crtc_id) break;
        }
    }
    CHECK(crtc_id, "no suitable CRTC found");
    printf("Connector %u -> encoder %u -> CRTC %u\n",
           conn->connector_id, conn->encoder_id, crtc_id);

    // Save current CRTC so we can restore on exit
    drmModeCrtc *saved_crtc = drmModeGetCrtc(drm_fd, crtc_id);
    if (saved_crtc) {
        printf("Saved CRTC %u state: buffer=%u, mode=%dx%d@%d\n",
               saved_crtc->crtc_id, saved_crtc->buffer_id,
               saved_crtc->mode.hdisplay, saved_crtc->mode.vdisplay,
               saved_crtc->mode.vrefresh);
    }

    // --- Step 6: Create GBM/EGL surface ---
    struct gbm_surface *gbm_surf = gbm_surface_create(
        gbm_dev, mode->hdisplay, mode->vdisplay,
        GBM_FORMAT_ARGB8888,
        GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
    CHECK(gbm_surf, "gbm_surface_create");

    EGLSurface egl_surf = eglCreateWindowSurface(egl_dpy, config, (EGLNativeWindowType)gbm_surf, NULL);
    EGL_CHECK(egl_surf, egl_surf != EGL_NO_SURFACE, "eglCreateWindowSurface");

    // --- Step 7: Create OpenGL ES context ---
    const EGLint ctx_attribs[] = {
        EGL_CONTEXT_CLIENT_VERSION, 2,
        EGL_NONE
    };
    EGLContext egl_ctx = eglCreateContext(egl_dpy, config, EGL_NO_CONTEXT, ctx_attribs);
    EGL_CHECK(egl_ctx, egl_ctx != EGL_NO_CONTEXT, "eglCreateContext");

    ok = eglMakeCurrent(egl_dpy, egl_surf, egl_surf, egl_ctx);
    EGL_CHECK(ok, ok, "eglMakeCurrent");

    // --- Step 8: Render ---
    glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    ok = eglSwapBuffers(egl_dpy, egl_surf);
    EGL_CHECK(ok, ok, "eglSwapBuffers");

    // --- Step 9: Commit buffer to display ---
    struct gbm_bo *bo = gbm_surface_lock_front_buffer(gbm_surf);
    CHECK(bo, "gbm_surface_lock_front_buffer");

    uint32_t handle = gbm_bo_get_handle(bo).u32;
    uint32_t pitch = gbm_bo_get_stride(bo);
    uint32_t bo_width = gbm_bo_get_width(bo);
    uint32_t bo_height = gbm_bo_get_height(bo);

    // Create a DRM framebuffer from the GBM buffer object.
    // Use the legacy drmModeAddFB which creates an implicit XRGB buffer.
    // This avoids format-mismatch rejections from drivers that only
    // accept XRGB8888 for scanout.
    uint32_t fb_id = 0;
    int ret = drmModeAddFB(drm_fd, bo_width, bo_height, 24, 32,
                           pitch, handle, &fb_id);
    if (ret < 0) {
        fprintf(stderr,
            "ERROR: drmModeAddFB failed (errno=%d, %s)\n"
            "  width=%u height=%u pitch=%u handle=%u\n",
            errno, strerror(errno),
            bo_width, bo_height, pitch, handle);
        exit(1);
    }
    printf("Created framebuffer fb_id=%u (%ux%u, pitch=%u)\n",
           fb_id, bo_width, bo_height, pitch);

    // Dump topology for debugging
    dump_drm_topology(drm_fd, res);

    // Try to become DRM master (fails harmlessly if we already are)
    if (drmSetMaster(drm_fd) < 0) {
        printf("Warning: drmSetMaster failed (errno=%d, %s). "
               "If another display server is running, drmModeSetCrtc will likely fail.\n",
               errno, strerror(errno));
    }

    // Disable the CRTC first to release the old framebuffer.
    // Some drivers reject drmModeSetCrtc if the CRTC already owns a FB
    // with an incompatible format.
    ret = drmModeSetCrtc(drm_fd, crtc_id, 0, 0, 0, NULL, 0, NULL);
    if (ret < 0) {
        printf("Note: drmModeSetCrtc(disable) returned errno=%d, %s\n",
               errno, strerror(errno));
    } else {
        printf("Disabled CRTC %u prior to setting new framebuffer.\n", crtc_id);
    }

    ret = drmModeSetCrtc(drm_fd, crtc_id, fb_id, 0, 0,
                          &conn->connector_id, 1, mode);
    if (ret < 0) {
        fprintf(stderr,
            "ERROR: drmModeSetCrtc failed (errno=%d, %s)\n"
            "  crtc_id=%u  fb_id=%u  connector_id=%u\n"
            "  mode=%dx%d@%d\n",
            errno, strerror(errno),
            crtc_id, fb_id, conn->connector_id,
            mode->hdisplay, mode->vdisplay, mode->vrefresh);
        exit(1);
    }

    printf("Displaying red screen for 5 seconds...\n");
    sleep(5);

    // --- Cleanup: restore CRTC, release buffers, destroy surfaces ---
    if (saved_crtc) {
        drmModeSetCrtc(drm_fd, saved_crtc->crtc_id, saved_crtc->buffer_id,
                       saved_crtc->x, saved_crtc->y,
                       &conn->connector_id, 1, &saved_crtc->mode);
        drmModeFreeCrtc(saved_crtc);
    }

    drmModeRmFB(drm_fd, fb_id);
    gbm_surface_release_buffer(gbm_surf, bo);
    gbm_surface_destroy(gbm_surf);
    eglDestroySurface(egl_dpy, egl_surf);
    eglDestroyContext(egl_dpy, egl_ctx);
    eglTerminate(egl_dpy);
    gbm_device_destroy(gbm_dev);

    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    close(drm_fd);

    return 0;
}
