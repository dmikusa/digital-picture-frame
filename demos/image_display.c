#define STB_IMAGE_IMPLEMENTATION
#include "stb_image.h"

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

static GLuint compile_shader(GLenum type, const char *src) {
    GLuint shader = glCreateShader(type);
    glShaderSource(shader, 1, &src, NULL);
    glCompileShader(shader);
    GLint compiled;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &compiled);
    if (!compiled) {
        GLint len;
        glGetShaderiv(shader, GL_INFO_LOG_LENGTH, &len);
        char *log = malloc(len);
        glGetShaderInfoLog(shader, len, NULL, log);
        fprintf(stderr, "Shader compile error:\n%s\n", log);
        free(log);
        exit(1);
    }
    return shader;
}

static GLuint link_program(GLuint vs, GLuint fs) {
    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    GLint linked;
    glGetProgramiv(prog, GL_LINK_STATUS, &linked);
    if (!linked) {
        GLint len;
        glGetProgramiv(prog, GL_INFO_LOG_LENGTH, &len);
        char *log = malloc(len);
        glGetProgramInfoLog(prog, len, NULL, log);
        fprintf(stderr, "Program link error:\n%s\n", log);
        free(log);
        exit(1);
    }
    return prog;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "Usage: %s <image_file>\n", argv[0]);
        return 1;
    }

    // =====================================================================
    // 1. Load the image
    // =====================================================================
    int img_w, img_h, channels;
    unsigned char *img_data = stbi_load(argv[1], &img_w, &img_h, &channels, 4);
    CHECK(img_data, "stbi_load(%s) failed: %s", argv[1], stbi_failure_reason());
    printf("Loaded image: %dx%d (requested 4 channels, file had %d)\n", img_w, img_h, channels);

    // =====================================================================
    // 2. Open DRM device
    // =====================================================================
    int drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(drm_fd >= 0, "open /dev/dri/card0");

    // =====================================================================
    // 3. Initialize GBM
    // =====================================================================
    struct gbm_device *gbm_dev = gbm_create_device(drm_fd);
    CHECK(gbm_dev, "gbm_create_device");

    // =====================================================================
    // 4. Initialize EGL
    // =====================================================================
    PFNEGLGETPLATFORMDISPLAYEXTPROC eglGetPlatformDisplayEXT =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    CHECK(eglGetPlatformDisplayEXT, "eglGetProcAddress(eglGetPlatformDisplayEXT)");

    EGLDisplay egl_dpy = eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR, gbm_dev, NULL);
    EGL_CHECK(egl_dpy, egl_dpy != EGL_NO_DISPLAY, "eglGetPlatformDisplayEXT");

    EGLBoolean ok = eglInitialize(egl_dpy, NULL, NULL);
    EGL_CHECK(ok, ok, "eglInitialize");

    ok = eglBindAPI(EGL_OPENGL_ES_API);
    EGL_CHECK(ok, ok, "eglBindAPI");

    // =====================================================================
    // 5. Choose EGL config
    // =====================================================================
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

    // =====================================================================
    // 6. Query DRM display topology
    // =====================================================================
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

    drmModeCrtc *saved_crtc = drmModeGetCrtc(drm_fd, crtc_id);
    if (saved_crtc) {
        printf("Saved CRTC %u state: buffer=%u, mode=%dx%d@%d\n",
               saved_crtc->crtc_id, saved_crtc->buffer_id,
               saved_crtc->mode.hdisplay, saved_crtc->mode.vdisplay,
               saved_crtc->mode.vrefresh);
    }

    // =====================================================================
    // 7. Create GBM + EGL surface
    // =====================================================================
    struct gbm_surface *gbm_surf = gbm_surface_create(
        gbm_dev, mode->hdisplay, mode->vdisplay,
        GBM_FORMAT_ARGB8888,
        GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
    CHECK(gbm_surf, "gbm_surface_create");

    EGLSurface egl_surf = eglCreateWindowSurface(egl_dpy, config, (EGLNativeWindowType)gbm_surf, NULL);
    EGL_CHECK(egl_surf, egl_surf != EGL_NO_SURFACE, "eglCreateWindowSurface");

    // =====================================================================
    // 8. Create OpenGL ES context
    // =====================================================================
    const EGLint ctx_attribs[] = {
        EGL_CONTEXT_CLIENT_VERSION, 2,
        EGL_NONE
    };
    EGLContext egl_ctx = eglCreateContext(egl_dpy, config, EGL_NO_CONTEXT, ctx_attribs);
    EGL_CHECK(egl_ctx, egl_ctx != EGL_NO_CONTEXT, "eglCreateContext");

    ok = eglMakeCurrent(egl_dpy, egl_surf, egl_surf, egl_ctx);
    EGL_CHECK(ok, ok, "eglMakeCurrent");

    // =====================================================================
    // 9. Compile shaders and upload texture
    // =====================================================================
    const char *vert_src =
        "attribute vec2 a_pos;\n"
        "attribute vec2 a_tex;\n"
        "varying vec2 v_tex;\n"
        "void main() {\n"
        "    gl_Position = vec4(a_pos, 0.0, 1.0);\n"
        "    v_tex = a_tex;\n"
        "}\n";

    const char *frag_src =
        "precision mediump float;\n"
        "varying vec2 v_tex;\n"
        "uniform sampler2D u_tex;\n"
        "void main() {\n"
        "    gl_FragColor = texture2D(u_tex, v_tex);\n"
        "}\n";

    GLuint vs = compile_shader(GL_VERTEX_SHADER, vert_src);
    GLuint fs = compile_shader(GL_FRAGMENT_SHADER, frag_src);
    GLuint prog = link_program(vs, fs);
    glUseProgram(prog);

    // Upload image as texture
    GLuint tex;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, img_w, img_h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, img_data);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

    // =====================================================================
    // 10. Build aspect-ratio-preserving quad
    // =====================================================================
    float screen_aspect = (float)mode->hdisplay / (float)mode->vdisplay;
    float img_aspect = (float)img_w / (float)img_h;

    float x0, x1, y0, y1;
    if (img_aspect > screen_aspect) {
        // Image is relatively wider: fit to screen width, letterbox top/bottom
        x0 = -1.0f; x1 = 1.0f;
        float half_h = screen_aspect / img_aspect;
        y0 = -half_h; y1 = half_h;
    } else {
        // Image is relatively taller: fit to screen height, letterbox left/right
        float half_w = img_aspect / screen_aspect;
        x0 = -half_w; x1 = half_w;
        y0 = -1.0f; y1 = 1.0f;
    }
    printf("Quad NDC: x=[%.3f, %.3f] y=[%.3f, %.3f] (screen aspect %.3f, image aspect %.3f)\n",
           x0, x1, y0, y1, screen_aspect, img_aspect);

    // Texture coords are flipped vertically because stb_image gives us
    // row 0 as the top of the image, but OpenGL's v=0 is the bottom.
    GLfloat vertices[] = {
        // x,    y,    u,    v
        x0,  y0,  0.0f, 1.0f,   // bottom-left  -> top-left of image
        x1,  y0,  1.0f, 1.0f,   // bottom-right -> top-right of image
        x0,  y1,  0.0f, 0.0f,   // top-left     -> bottom-left of image
        x1,  y1,  1.0f, 0.0f,   // top-right    -> bottom-right of image
    };

    GLuint buf;
    glGenBuffers(1, &buf);
    glBindBuffer(GL_ARRAY_BUFFER, buf);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);

    GLint pos_loc = glGetAttribLocation(prog, "a_pos");
    GLint tex_loc = glGetAttribLocation(prog, "a_tex");

    glEnableVertexAttribArray(pos_loc);
    glVertexAttribPointer(pos_loc, 2, GL_FLOAT, GL_FALSE,
                          4 * sizeof(GLfloat), (void *)0);
    glEnableVertexAttribArray(tex_loc);
    glVertexAttribPointer(tex_loc, 2, GL_FLOAT, GL_FALSE,
                          4 * sizeof(GLfloat), (void *)(2 * sizeof(GLfloat)));

    // =====================================================================
    // 11. Render
    // =====================================================================
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);  // black letterbox bars
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    ok = eglSwapBuffers(egl_dpy, egl_surf);
    EGL_CHECK(ok, ok, "eglSwapBuffers");

    // =====================================================================
    // 12. Commit buffer to display
    // =====================================================================
    struct gbm_bo *bo = gbm_surface_lock_front_buffer(gbm_surf);
    CHECK(bo, "gbm_surface_lock_front_buffer");

    uint32_t handle = gbm_bo_get_handle(bo).u32;
    uint32_t pitch = gbm_bo_get_stride(bo);
    uint32_t bo_width = gbm_bo_get_width(bo);
    uint32_t bo_height = gbm_bo_get_height(bo);

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

    dump_drm_topology(drm_fd, res);

    if (drmSetMaster(drm_fd) < 0) {
        printf("Warning: drmSetMaster failed (errno=%d, %s). "
               "If another display server is running, drmModeSetCrtc will likely fail.\n",
               errno, strerror(errno));
    }

    // Disable the old CRTC first (blank) so we can attach our new framebuffer
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

    printf("Displaying image for 5 seconds...\n");
    sleep(5);

    // =====================================================================
    // 13. Cleanup
    // =====================================================================
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

    stbi_image_free(img_data);

    return 0;
}
