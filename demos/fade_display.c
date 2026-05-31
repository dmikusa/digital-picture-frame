#define STB_IMAGE_IMPLEMENTATION
#include "stb_image.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <time.h>
#include <sys/epoll.h>
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

#define FADE_DURATION_SEC 1.5f
#define HOLD_FIRST_SEC    1.0f
#define HOLD_SECOND_SEC   5.0f

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

static volatile int g_flip_done = 0;
static int g_epoll_fd = -1;

/* -------------------------------------------------------------------------- */
/* DRM event handling                                                         */
/* -------------------------------------------------------------------------- */

static void page_flip_handler(int fd, unsigned int frame,
                              unsigned int sec, unsigned int usec,
                              void *data)
{
    (void)fd; (void)frame; (void)sec; (void)usec;
    *(volatile int *)data = 1;
}

static void wait_for_flip(int drm_fd)
{
    drmEventContext evctx = {
        .version = 2,
        .page_flip_handler = page_flip_handler,
    };

    while (!g_flip_done) {
        struct epoll_event events[1];
        int n = epoll_wait(g_epoll_fd, events, 1, -1);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            perror("epoll_wait");
            exit(1);
        }
        if (n > 0) {
            drmHandleEvent(drm_fd, &evctx);
        }
    }
    g_flip_done = 0;
}

/* -------------------------------------------------------------------------- */
/* Shader helpers                                                             */
/* -------------------------------------------------------------------------- */

static GLuint compile_shader(GLenum type, const char *src)
{
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

static GLuint link_program(GLuint vs, GLuint fs)
{
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

/* -------------------------------------------------------------------------- */
/* Rendering helpers                                                          */
/* -------------------------------------------------------------------------- */

static void build_quad(float img_aspect, float screen_aspect, GLfloat *out)
{
    float x0, x1, y0, y1;
    if (img_aspect > screen_aspect) {
        x0 = -1.0f; x1 = 1.0f;
        float h = screen_aspect / img_aspect;
        y0 = -h; y1 = h;
    } else {
        float w = img_aspect / screen_aspect;
        x0 = -w; x1 = w;
        y0 = -1.0f; y1 = 1.0f;
    }

    /* V texture coords are flipped: stb_image row 0 is top, GL v=0 is bottom */
    out[0]  = x0; out[1]  = y0; out[2]  = 0.0f; out[3]  = 1.0f;
    out[4]  = x1; out[5]  = y0; out[6]  = 1.0f; out[7]  = 1.0f;
    out[8]  = x0; out[9]  = y1; out[10] = 0.0f; out[11] = 0.0f;
    out[12] = x1; out[13] = y1; out[14] = 1.0f; out[15] = 0.0f;
}

struct frame_buffer {
    struct gbm_bo *bo;
    uint32_t fb_id;
};

/* -------------------------------------------------------------------------- */
/* Main                                                                       */
/* -------------------------------------------------------------------------- */

int main(int argc, char **argv)
{
    if (argc < 3) {
        fprintf(stderr, "Usage: %s <image1> <image2>\n", argv[0]);
        return 1;
    }

    /* ---- Load images ---------------------------------------------------- */
    int img0_w, img0_h, ch0;
    int img1_w, img1_h, ch1;
    unsigned char *img0 = stbi_load(argv[1], &img0_w, &img0_h, &ch0, 4);
    CHECK(img0, "stbi_load(%s) failed: %s", argv[1], stbi_failure_reason());
    unsigned char *img1 = stbi_load(argv[2], &img1_w, &img1_h, &ch1, 4);
    CHECK(img1, "stbi_load(%s) failed: %s", argv[2], stbi_failure_reason());
    printf("Loaded %s: %dx%d\n", argv[1], img0_w, img0_h);
    printf("Loaded %s: %dx%d\n", argv[2], img1_w, img1_h);

    /* ---- DRM / GBM / EGL setup ---------------------------------------- */
    int drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(drm_fd >= 0, "open /dev/dri/card0");

    struct gbm_device *gbm_dev = gbm_create_device(drm_fd);
    CHECK(gbm_dev, "gbm_create_device");

    PFNEGLGETPLATFORMDISPLAYEXTPROC eglGetPlatformDisplayEXT =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    CHECK(eglGetPlatformDisplayEXT, "eglGetProcAddress(eglGetPlatformDisplayEXT)");

    EGLDisplay egl_dpy = eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR, gbm_dev, NULL);
    EGL_CHECK(egl_dpy, egl_dpy != EGL_NO_DISPLAY, "eglGetPlatformDisplayEXT");

    EGLBoolean ok = eglInitialize(egl_dpy, NULL, NULL);
    EGL_CHECK(ok, ok, "eglInitialize");
    ok = eglBindAPI(EGL_OPENGL_ES_API);
    EGL_CHECK(ok, ok, "eglBindAPI");

    EGLConfig config;
    EGLint num_configs;
    const EGLint config_attribs[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_NONE
    };
    ok = eglChooseConfig(egl_dpy, config_attribs, &config, 1, &num_configs);
    EGL_CHECK(ok, ok && num_configs > 0, "eglChooseConfig");

    drmModeRes *res = drmModeGetResources(drm_fd);
    CHECK(res, "drmModeGetResources");
    CHECK(res->count_connectors > 0, "no connectors found");

    drmModeConnector *conn = NULL;
    for (int i = 0; i < res->count_connectors; ++i) {
        conn = drmModeGetConnector(drm_fd, res->connectors[i]);
        if (conn && conn->connection == DRM_MODE_CONNECTED && conn->count_modes > 0)
            break;
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
    if (!mode) mode = &conn->modes[0];
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

    struct gbm_surface *gbm_surf = gbm_surface_create(
        gbm_dev, mode->hdisplay, mode->vdisplay,
        GBM_FORMAT_ARGB8888,
        GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
    CHECK(gbm_surf, "gbm_surface_create");

    EGLSurface egl_surf = eglCreateWindowSurface(egl_dpy, config,
                                                  (EGLNativeWindowType)gbm_surf, NULL);
    EGL_CHECK(egl_surf, egl_surf != EGL_NO_SURFACE, "eglCreateWindowSurface");

    const EGLint ctx_attribs[] = {
        EGL_CONTEXT_CLIENT_VERSION, 2,
        EGL_NONE
    };
    EGLContext egl_ctx = eglCreateContext(egl_dpy, config, EGL_NO_CONTEXT, ctx_attribs);
    EGL_CHECK(egl_ctx, egl_ctx != EGL_NO_CONTEXT, "eglCreateContext");

    ok = eglMakeCurrent(egl_dpy, egl_surf, egl_surf, egl_ctx);
    EGL_CHECK(ok, ok, "eglMakeCurrent");

    /* ---- Compile shaders ------------------------------------------------ */
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
        "uniform float u_alpha;\n"
        "void main() {\n"
        "    gl_FragColor = texture2D(u_tex, v_tex) * u_alpha;\n"
        "}\n";

    GLuint vs = compile_shader(GL_VERTEX_SHADER, vert_src);
    GLuint fs = compile_shader(GL_FRAGMENT_SHADER, frag_src);
    GLuint prog = link_program(vs, fs);
    glUseProgram(prog);

    GLint u_tex_loc   = glGetUniformLocation(prog, "u_tex");
    GLint u_alpha_loc = glGetUniformLocation(prog, "u_alpha");
    glUniform1i(u_tex_loc, 0);

    /* ---- Upload textures ------------------------------------------------ */
    GLuint tex[2];
    glGenTextures(2, tex);
    for (int i = 0; i < 2; ++i) {
        glBindTexture(GL_TEXTURE_2D, tex[i]);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA,
                     i == 0 ? img0_w : img1_w,
                     i == 0 ? img0_h : img1_h,
                     0, GL_RGBA, GL_UNSIGNED_BYTE,
                     i == 0 ? img0 : img1);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }

    /* ---- Vertex buffer -------------------------------------------------- */
    GLuint buf;
    glGenBuffers(1, &buf);
    glBindBuffer(GL_ARRAY_BUFFER, buf);
    glBufferData(GL_ARRAY_BUFFER, 16 * sizeof(GLfloat), NULL, GL_DYNAMIC_DRAW);

    GLint pos_loc = glGetAttribLocation(prog, "a_pos");
    GLint tex_loc = glGetAttribLocation(prog, "a_tex");
    glEnableVertexAttribArray(pos_loc);
    glVertexAttribPointer(pos_loc, 2, GL_FLOAT, GL_FALSE,
                          4 * sizeof(GLfloat), (void *)0);
    glEnableVertexAttribArray(tex_loc);
    glVertexAttribPointer(tex_loc, 2, GL_FLOAT, GL_FALSE,
                          4 * sizeof(GLfloat),
                          (void *)(2 * sizeof(GLfloat)));

    /* ---- Aspect ratios -------------------------------------------------- */
    float screen_aspect = (float)mode->hdisplay / (float)mode->vdisplay;
    float aspect[2] = {
        (float)img0_w / (float)img0_h,
        (float)img1_w / (float)img1_h
    };

    /* ---- Render first image and commit via SetCrtc -------------------- */
    GLfloat verts[16];
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    build_quad(aspect[0], screen_aspect, verts);
    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
    glBindTexture(GL_TEXTURE_2D, tex[0]);
    glUniform1f(u_alpha_loc, 1.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    ok = eglSwapBuffers(egl_dpy, egl_surf);
    EGL_CHECK(ok, ok, "eglSwapBuffers");

    struct frame_buffer prev = {0};
    prev.bo = gbm_surface_lock_front_buffer(gbm_surf);
    CHECK(prev.bo, "gbm_surface_lock_front_buffer (first frame)");

    uint32_t handle = gbm_bo_get_handle(prev.bo).u32;
    uint32_t pitch  = gbm_bo_get_stride(prev.bo);
    uint32_t bo_w   = gbm_bo_get_width(prev.bo);
    uint32_t bo_h   = gbm_bo_get_height(prev.bo);

    int ret = drmModeAddFB(drm_fd, bo_w, bo_h, 24, 32,
                           pitch, handle, &prev.fb_id);
    CHECK(ret == 0, "drmModeAddFB (first frame)");

    if (drmSetMaster(drm_fd) < 0) {
        printf("Warning: drmSetMaster failed (errno=%d, %s)\n", errno, strerror(errno));
    }

    ret = drmModeSetCrtc(drm_fd, crtc_id, 0, 0, 0, NULL, 0, NULL);
    if (ret < 0) {
        printf("Note: disable CRTC returned %s\n", strerror(errno));
    } else {
        printf("Disabled old CRTC.\n");
    }

    ret = drmModeSetCrtc(drm_fd, crtc_id, prev.fb_id, 0, 0,
                          &conn->connector_id, 1, mode);
    CHECK(ret == 0, "drmModeSetCrtc (first frame)");
    printf("First frame committed.\n");

    /* ---- Setup epoll for page-flip events ----------------------------- */
    g_epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    CHECK(g_epoll_fd >= 0, "epoll_create1");
    struct epoll_event ev = { .events = EPOLLIN, .data = {0} };
    ret = epoll_ctl(g_epoll_fd, EPOLL_CTL_ADD, drm_fd, &ev);
    CHECK(ret == 0, "epoll_ctl");

    /* ---- Hold first image --------------------------------------------- */
    printf("Holding first image for %.1f seconds...\n", HOLD_FIRST_SEC);
    sleep((unsigned int)HOLD_FIRST_SEC);

    /* ---- Fade animation ------------------------------------------------ */
    printf("Starting fade (%.1f seconds)...\n", FADE_DURATION_SEC);

    struct timespec fade_start, now;
    clock_gettime(CLOCK_MONOTONIC, &fade_start);

    struct frame_buffer curr = {0};

    while (1) {
        clock_gettime(CLOCK_MONOTONIC, &now);
        float elapsed = (float)(now.tv_sec - fade_start.tv_sec)
                      + (float)(now.tv_nsec - fade_start.tv_nsec) / 1e9f;
        float mix = elapsed / FADE_DURATION_SEC;
        if (mix > 1.0f) mix = 1.0f;

        /* Render cross-fade */
        glClear(GL_COLOR_BUFFER_BIT);
        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);

        /* Image 0 */
        build_quad(aspect[0], screen_aspect, verts);
        glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
        glBindTexture(GL_TEXTURE_2D, tex[0]);
        glUniform1f(u_alpha_loc, 1.0f - mix);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

        /* Image 1 */
        build_quad(aspect[1], screen_aspect, verts);
        glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
        glBindTexture(GL_TEXTURE_2D, tex[1]);
        glUniform1f(u_alpha_loc, mix);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

        glDisable(GL_BLEND);

        /* Swap and lock new front buffer */
        ok = eglSwapBuffers(egl_dpy, egl_surf);
        EGL_CHECK(ok, ok, "eglSwapBuffers");

        curr.bo = gbm_surface_lock_front_buffer(gbm_surf);
        CHECK(curr.bo, "gbm_surface_lock_front_buffer");

        handle = gbm_bo_get_handle(curr.bo).u32;
        pitch  = gbm_bo_get_stride(curr.bo);
        bo_w   = gbm_bo_get_width(curr.bo);
        bo_h   = gbm_bo_get_height(curr.bo);

        ret = drmModeAddFB(drm_fd, bo_w, bo_h, 24, 32,
                           pitch, handle, &curr.fb_id);
        CHECK(ret == 0, "drmModeAddFB");

        /* Async page flip */
        g_flip_done = 0;
        ret = drmModePageFlip(drm_fd, crtc_id, curr.fb_id,
                              DRM_MODE_PAGE_FLIP_EVENT,
                              (void *)&g_flip_done);
        CHECK(ret == 0, "drmModePageFlip");

        wait_for_flip(drm_fd);

        /* Previous buffer is no longer scanned out; recycle it */
        if (prev.bo) {
            drmModeRmFB(drm_fd, prev.fb_id);
            gbm_surface_release_buffer(gbm_surf, prev.bo);
        }
        prev = curr;
        curr.bo = NULL;
        curr.fb_id = 0;

        if (mix >= 1.0f) break;
    }

    printf("Fade complete.\n");

    /* ---- Hold second image -------------------------------------------- */
    printf("Holding second image for %.1f seconds...\n", HOLD_SECOND_SEC);
    sleep((unsigned int)HOLD_SECOND_SEC);

    /* ---- Cleanup ------------------------------------------------------ */
    if (prev.bo) {
        drmModeRmFB(drm_fd, prev.fb_id);
        gbm_surface_release_buffer(gbm_surf, prev.bo);
    }

    if (saved_crtc) {
        drmModeSetCrtc(drm_fd, saved_crtc->crtc_id, saved_crtc->buffer_id,
                       saved_crtc->x, saved_crtc->y,
                       &conn->connector_id, 1, &saved_crtc->mode);
        drmModeFreeCrtc(saved_crtc);
    }

    eglDestroySurface(egl_dpy, egl_surf);
    eglDestroyContext(egl_dpy, egl_ctx);
    eglTerminate(egl_dpy);
    gbm_surface_destroy(gbm_surf);
    gbm_device_destroy(gbm_dev);

    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    close(drm_fd);
    close(g_epoll_fd);

    stbi_image_free(img0);
    stbi_image_free(img1);

    return 0;
}
