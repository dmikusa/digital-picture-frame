/*
 * photo-frame-display.c
 *
 * Production-quality DRM/GBM/EGL photo frame.
 *
 * Communicates with a management app via a Unix domain socket.
 * The display app holds 2 images: the current one and the next one.
 * When the current transition completes, it sends READY and the manager
 * may push the next image at any time during the hold.
 *
 * Build:
 *   gcc photo-frame-display.c -o photo-frame-display -lEGL -lGLESv2 -lgbm \
 *       $(pkg-config --cflags --libs libdrm) -lm
 *
 * Run:
 *   ./photo-frame-display
 *
 *   Then from another terminal (or your management app):
 *     echo "IMG /path/to/photo1.jpg" | nc -U /tmp/photo-frame.sock
 *     echo "IMG /path/to/photo2.jpg" | nc -U /tmp/photo-frame.sock
 */

#define _GNU_SOURCE

#define STB_IMAGE_IMPLEMENTATION
#include "stb_image.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <time.h>
#include <math.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <signal.h>
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

#define SOCKET_PATH            "/tmp/photo-frame.sock"
#define HOLD_DURATION_SEC      5.0f
#define DEFAULT_FADE_DURATION  1.5f
#define DEFAULT_SKIP_FRAMES  0

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

/* -------------------------------------------------------------------------- */
/* Data structures                                                            */
/* -------------------------------------------------------------------------- */

struct image_slot {
    GLuint tex;
    int    w, h;
    int    occupied;
};

struct frame_buffer {
    struct gbm_bo *bo;
    uint32_t       fb_id;
};

struct app_state {
    /* DRM / GBM / EGL */
    int                  drm_fd;
    struct gbm_device   *gbm_dev;
    struct gbm_surface  *gbm_surf;
    EGLDisplay           egl_dpy;
    EGLSurface           egl_surf;
    EGLContext           egl_ctx;
    uint32_t             crtc_id;
    drmModeCrtc         *saved_crtc;
    GLint                u_alpha_loc;

    /* Images */
    struct image_slot    slots[2];
    int                  current_slot;   /* 0 or 1 */

    /* Pending CPU-side image (decoded, waiting for a free GPU slot) */
    unsigned char       *pending_pixels;
    int                  pending_w, pending_h;

    /* Socket */
    int                  listen_fd;
    int                  conn_fd;
    int                  epoll_fd;
    int                  socket_paused;

    /* Fade state */
    int                  fading;
    float                fade_progress;
    struct timespec      fade_start;
    int                  fade_from, fade_to;

    /* Page-flip buffers */
    struct frame_buffer  scanout_fb;
    struct frame_buffer  pending_fb;
    volatile int         flip_done;

    /* Phase */
    enum {
        PHASE_WAITING,
        PHASE_HOLDING,
        PHASE_FADING
    } phase;
    struct timespec      hold_deadline;
    int                  hold_complete;

    /* Display geometry */
    float                screen_aspect;
    int                  mode_w, mode_h;

    /* Configurable fade */
    float                fade_duration;
    int                  skip_frames;
    int                  frame_counter;

    /* Graceful shutdown */
    volatile sig_atomic_t running;
} g;

static void signal_handler(int sig)
{
    (void)sig;
    g.running = 0;
}

/* Read display settings from environment variables */
static void read_display_config(void)
{
    g.fade_duration = DEFAULT_FADE_DURATION;
    g.skip_frames   = DEFAULT_SKIP_FRAMES;

    const char *env_fade = getenv("PHOTO_FRAME_FADE_DURATION");
    if (env_fade && env_fade[0] != '\0') {
        g.fade_duration = strtof(env_fade, NULL);
        if (g.fade_duration < 0.0f) g.fade_duration = 0.0f;
    }

    const char *env_skip = getenv("PHOTO_FRAME_SKIP_FRAMES");
    if (env_skip && env_skip[0] != '\0') {
        g.skip_frames = (int)strtol(env_skip, NULL, 10);
        if (g.skip_frames < 0) g.skip_frames = 0;
    }

    printf("Display config: fade=%.1fs skip=%d\n", g.fade_duration, g.skip_frames);
}

/* -------------------------------------------------------------------------- */
/* Helpers                                                                    */
/* -------------------------------------------------------------------------- */

static GLuint compile_shader(GLenum type, const char *src)
{
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        GLint len;
        glGetShaderiv(s, GL_INFO_LOG_LENGTH, &len);
        char *log = malloc(len);
        glGetShaderInfoLog(s, len, NULL, log);
        fprintf(stderr, "Shader compile error:\n%s\n", log);
        free(log);
        exit(1);
    }
    return s;
}

static GLuint link_program(GLuint vs, GLuint fs)
{
    GLuint p = glCreateProgram();
    glAttachShader(p, vs);
    glAttachShader(p, fs);
    glLinkProgram(p);
    GLint ok;
    glGetProgramiv(p, GL_LINK_STATUS, &ok);
    if (!ok) {
        GLint len;
        glGetProgramiv(p, GL_INFO_LOG_LENGTH, &len);
        char *log = malloc(len);
        glGetProgramInfoLog(p, len, NULL, log);
        fprintf(stderr, "Program link error:\n%s\n", log);
        free(log);
        exit(1);
    }
    return p;
}

static void build_quad(float img_aspect, float screen_aspect, GLfloat *v)
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
    /* V coords flipped because stb_image row 0 is top */
    v[0]  = x0; v[1]  = y0; v[2]  = 0.0f; v[3]  = 1.0f;
    v[4]  = x1; v[5]  = y0; v[6]  = 1.0f; v[7]  = 1.0f;
    v[8]  = x0; v[9]  = y1; v[10] = 0.0f; v[11] = 0.0f;
    v[12] = x1; v[13] = y1; v[14] = 1.0f; v[15] = 0.0f;
}

static void page_flip_handler(int fd, unsigned int frame,
                              unsigned int sec, unsigned int usec,
                              void *data)
{
    (void)fd; (void)frame; (void)sec; (void)usec;
    *(volatile int *)data = 1;
}

/* -------------------------------------------------------------------------- */
/* Image loading                                                              */
/* -------------------------------------------------------------------------- */

static void load_image_into_slot(int slot_idx, const char *path)
{
    if (g.slots[slot_idx].occupied) {
        fprintf(stderr, "Warning: slot %d already occupied, overwriting\n", slot_idx);
    }

    int w, h, ch;
    unsigned char *data = stbi_load(path, &w, &h, &ch, 4);
    if (!data) {
        fprintf(stderr, "Failed to load %s: %s\n", path, stbi_failure_reason());
        return;
    }
    printf("Loaded %s -> slot %d (%dx%d)\n", path, slot_idx, w, h);

    glBindTexture(GL_TEXTURE_2D, g.slots[slot_idx].tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, data);
    stbi_image_free(data);

    g.slots[slot_idx].w = w;
    g.slots[slot_idx].h = h;
    g.slots[slot_idx].occupied = 1;
}

static void store_pending_image(const char *path)
{
    if (g.pending_pixels) {
        stbi_image_free(g.pending_pixels);
        g.pending_pixels = NULL;
    }

    int w, h, ch;
    unsigned char *data = stbi_load(path, &w, &h, &ch, 4);
    if (!data) {
        fprintf(stderr, "Failed to load %s: %s\n", path, stbi_failure_reason());
        return;
    }
    printf("Buffered %s as pending (%dx%d)\n", path, w, h);

    g.pending_pixels = data;
    g.pending_w = w;
    g.pending_h = h;
}

static void upload_pending_to_slot(int slot_idx)
{
    if (!g.pending_pixels) return;

    glBindTexture(GL_TEXTURE_2D, g.slots[slot_idx].tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA,
                 g.pending_w, g.pending_h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, g.pending_pixels);

    g.slots[slot_idx].w = g.pending_w;
    g.slots[slot_idx].h = g.pending_h;
    g.slots[slot_idx].occupied = 1;

    stbi_image_free(g.pending_pixels);
    g.pending_pixels = NULL;
    printf("Uploaded pending image to slot %d\n", slot_idx);
}

/* -------------------------------------------------------------------------- */
/* Socket protocol                                                            */
/* -------------------------------------------------------------------------- */

static void send_ready(void)
{
    if (g.conn_fd < 0) return;
    const char msg[] = "READY\n";
    ssize_t n = write(g.conn_fd, msg, sizeof(msg) - 1);
    if (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EPIPE) {
        perror("write READY");
        close(g.conn_fd);
        g.conn_fd = -1;
    } else if (n > 0) {
        printf("Sent READY\n");
    } else if (n < 0 && errno == EPIPE) {
        printf("Manager disconnected before READY could be sent.\n");
        close(g.conn_fd);
        g.conn_fd = -1;
    }
}

static void handle_img_command(const char *path)
{
    printf("Received IMG: %s\n", path);
    /* Fill the first empty slot.  During normal operation exactly one
       slot is free (the one we just faded away from).  At startup both
       are empty, so this naturally fills slot 0 first, then slot 1. */
    if (!g.slots[0].occupied) {
        load_image_into_slot(0, path);
    } else if (!g.slots[1].occupied) {
        load_image_into_slot(1, path);
    } else if (!g.pending_pixels) {
        store_pending_image(path);
    } else {
        printf("Warning: both slots and pending buffer full, dropping %s\n", path);
    }
}

static void handle_socket_data(void)
{
    static char buf[4096];
    static size_t len = 0;

    ssize_t n = read(g.conn_fd, buf + len, sizeof(buf) - len);
    if (n < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK)
            return;
        perror("read socket");
        printf("Manager disconnected (error).\n");
        close(g.conn_fd);
        g.conn_fd = -1;
        g.socket_paused = 0;
        len = 0;
        return;
    }
    if (n == 0) {
        printf("Manager disconnected (EOF).\n");
        close(g.conn_fd);
        g.conn_fd = -1;
        g.socket_paused = 0;
        len = 0;
        return;
    }
    len += n;

    char *start = buf;
    char *end   = buf + len;
    char *nl;
    while ((nl = memchr(start, '\n', end - start)) != NULL) {
        *nl = '\0';
        if (strncmp(start, "IMG ", 4) == 0) {
            handle_img_command(start + 4);
        } else {
            printf("Unknown command: %s\n", start);
        }
        start = nl + 1;

        /* Backpressure: if full, stop consuming and pause socket reads */
        if (g.slots[0].occupied && g.slots[1].occupied && g.pending_pixels) {
            size_t remain = end - start;
            if (remain > 0)
                memmove(buf, start, remain);
            len = remain;

            if (!g.socket_paused) {
                g.socket_paused = 1;
                epoll_ctl(g.epoll_fd, EPOLL_CTL_DEL, g.conn_fd, NULL);
                printf("Socket paused (backpressure).\n");
            }
            return;
        }
    }

    /* No more complete commands in buffer */
    size_t remain = end - start;
    if (remain > 0)
        memmove(buf, start, remain);
    len = remain;
}

/* -------------------------------------------------------------------------- */
/* Fade / render                                                              */
/* -------------------------------------------------------------------------- */

static void render_frame(float mix, int from_slot, int to_slot)
{
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);

    GLfloat verts[16];

    /* From image */
    build_quad((float)g.slots[from_slot].w / (float)g.slots[from_slot].h,
               g.screen_aspect, verts);
    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
    glBindTexture(GL_TEXTURE_2D, g.slots[from_slot].tex);
    glUniform1f(g.u_alpha_loc, 1.0f - mix);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    /* To image */
    build_quad((float)g.slots[to_slot].w / (float)g.slots[to_slot].h,
               g.screen_aspect, verts);
    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
    glBindTexture(GL_TEXTURE_2D, g.slots[to_slot].tex);
    glUniform1f(g.u_alpha_loc, mix);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    glDisable(GL_BLEND);
}

static void request_page_flip(void)
{
    EGLBoolean ok = eglSwapBuffers(g.egl_dpy, g.egl_surf);
    EGL_CHECK(ok, ok, "eglSwapBuffers");

    g.pending_fb.bo = gbm_surface_lock_front_buffer(g.gbm_surf);
    CHECK(g.pending_fb.bo, "gbm_surface_lock_front_buffer");

    uint32_t handle = gbm_bo_get_handle(g.pending_fb.bo).u32;
    uint32_t pitch  = gbm_bo_get_stride(g.pending_fb.bo);
    uint32_t bw     = gbm_bo_get_width(g.pending_fb.bo);
    uint32_t bh     = gbm_bo_get_height(g.pending_fb.bo);

    int ret = drmModeAddFB(g.drm_fd, bw, bh, 24, 32,
                           pitch, handle, &g.pending_fb.fb_id);
    CHECK(ret == 0, "drmModeAddFB");

    g.flip_done = 0;
    ret = drmModePageFlip(g.drm_fd, g.crtc_id, g.pending_fb.fb_id,
                          DRM_MODE_PAGE_FLIP_EVENT,
                          (void *)&g.flip_done);
    CHECK(ret == 0, "drmModePageFlip");
}

static void start_fade(int from_slot, int to_slot)
{
    printf("Starting fade %d -> %d\n", from_slot, to_slot);
    g.fading        = 1;
    g.fade_from     = from_slot;
    g.fade_to       = to_slot;
    g.fade_progress = 0.0f;
    g.frame_counter = 0;
    clock_gettime(CLOCK_MONOTONIC, &g.fade_start);

    render_frame(0.0f, from_slot, to_slot);
    request_page_flip();
}

static void advance_fade(void)
{
    /* Promote pending framebuffer to scanout on every flip completion */
    if (g.pending_fb.bo) {
        if (g.scanout_fb.bo) {
            drmModeRmFB(g.drm_fd, g.scanout_fb.fb_id);
            gbm_surface_release_buffer(g.gbm_surf, g.scanout_fb.bo);
        }
        g.scanout_fb = g.pending_fb;
        g.pending_fb.bo    = NULL;
        g.pending_fb.fb_id = 0;
    }

    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    float elapsed = (float)(now.tv_sec - g.fade_start.tv_sec)
                  + (float)(now.tv_nsec - g.fade_start.tv_nsec) / 1e9f;
    g.fade_progress = elapsed / g.fade_duration;
    if (g.fade_progress > 1.0f) g.fade_progress = 1.0f;

    if (g.fade_progress >= 1.0f) {
        /* Fade complete */
        printf("Fade complete. Now showing slot %d\n", g.fade_to);

        int old_slot = g.current_slot;
        g.current_slot = g.fade_to;
        g.slots[old_slot].occupied = 0;

        /* Upload any pending CPU buffer into the freed slot */
        if (g.pending_pixels) {
            upload_pending_to_slot(old_slot);
        }

        /* Resume socket reads if we had backpressure */
        if (g.socket_paused && g.conn_fd >= 0) {
            g.socket_paused = 0;
            struct epoll_event ev;
            ev.events = EPOLLIN;
            ev.data.fd = g.conn_fd;
            int ret = epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.conn_fd, &ev);
            if (ret < 0 && errno != EEXIST) {
                perror("epoll_ctl ADD (resume)");
            } else {
                printf("Socket resumed.\n");
                handle_socket_data();
            }
        }

        g.fading = 0;
        g.phase  = PHASE_HOLDING;

        send_ready();

        clock_gettime(CLOCK_MONOTONIC, &g.hold_deadline);
        g.hold_deadline.tv_sec += (time_t)HOLD_DURATION_SEC;
        g.hold_deadline.tv_nsec += (long)((HOLD_DURATION_SEC - (int)HOLD_DURATION_SEC) * 1e9);
        if (g.hold_deadline.tv_nsec >= 1000000000L) {
            g.hold_deadline.tv_sec++;
            g.hold_deadline.tv_nsec -= 1000000000L;
        }
        g.hold_complete = 0;
        return;
    }

    g.frame_counter++;
    if (g.skip_frames > 0 && (g.frame_counter % (g.skip_frames + 1)) != 0) {
        /* Skip rendering this frame: re-flip to the same buffer */
        g.flip_done = 0;
        int ret = drmModePageFlip(g.drm_fd, g.crtc_id, g.scanout_fb.fb_id,
                                  DRM_MODE_PAGE_FLIP_EVENT,
                                  (void *)&g.flip_done);
        CHECK(ret == 0, "drmModePageFlip (skip)");
        return;
    }

    /* Render next frame */
    render_frame(g.fade_progress, g.fade_from, g.fade_to);
    request_page_flip();
}

/* -------------------------------------------------------------------------- */
/* Main                                                                       */
/* -------------------------------------------------------------------------- */

int main(void)
{
    memset(&g, 0, sizeof(g));
    g.running = 1;
    read_display_config();

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = signal_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0; /* do NOT restart syscalls */
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);

    /* ---- DRM/GBM/EGL setup --------------------------------------------- */
    g.drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(g.drm_fd >= 0, "open /dev/dri/card0");

    g.gbm_dev = gbm_create_device(g.drm_fd);
    CHECK(g.gbm_dev, "gbm_create_device");

    PFNEGLGETPLATFORMDISPLAYEXTPROC eglGetPlatformDisplayEXT =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    CHECK(eglGetPlatformDisplayEXT, "eglGetProcAddress");

    g.egl_dpy = eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR, g.gbm_dev, NULL);
    EGL_CHECK(g.egl_dpy, g.egl_dpy != EGL_NO_DISPLAY, "eglGetPlatformDisplayEXT");

    EGLBoolean ok = eglInitialize(g.egl_dpy, NULL, NULL);
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
    ok = eglChooseConfig(g.egl_dpy, config_attribs, &config, 1, &num_configs);
    EGL_CHECK(ok, ok && num_configs > 0, "eglChooseConfig");

    drmModeRes *res = drmModeGetResources(g.drm_fd);
    CHECK(res, "drmModeGetResources");
    CHECK(res->count_connectors > 0, "no connectors");

    drmModeConnector *conn = NULL;
    for (int i = 0; i < res->count_connectors; ++i) {
        conn = drmModeGetConnector(g.drm_fd, res->connectors[i]);
        if (conn && conn->connection == DRM_MODE_CONNECTED && conn->count_modes > 0)
            break;
        drmModeFreeConnector(conn);
        conn = NULL;
    }
    CHECK(conn, "no connected connector");

    drmModeModeInfo *mode = NULL;
    for (int i = 0; i < conn->count_modes; ++i) {
        if (conn->modes[i].type & DRM_MODE_TYPE_PREFERRED) {
            mode = &conn->modes[i];
            break;
        }
    }
    if (!mode) mode = &conn->modes[0];
    g.mode_w = mode->hdisplay;
    g.mode_h = mode->vdisplay;
    g.screen_aspect = (float)g.mode_w / (float)g.mode_h;
    printf("Mode: %dx%d (aspect %.3f)\n", g.mode_w, g.mode_h, g.screen_aspect);

    uint32_t crtc_id = 0;
    drmModeEncoder *enc = drmModeGetEncoder(g.drm_fd, conn->encoder_id);
    if (enc) {
        crtc_id = enc->crtc_id;
        drmModeFreeEncoder(enc);
    } else {
        for (int i = 0; i < conn->count_encoders; ++i) {
            enc = drmModeGetEncoder(g.drm_fd, conn->encoders[i]);
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
    CHECK(crtc_id, "no suitable CRTC");
    g.crtc_id = crtc_id;
    printf("CRTC %u\n", g.crtc_id);

    g.saved_crtc = drmModeGetCrtc(g.drm_fd, g.crtc_id);

    g.gbm_surf = gbm_surface_create(g.gbm_dev, g.mode_w, g.mode_h,
                                    GBM_FORMAT_ARGB8888,
                                    GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
    CHECK(g.gbm_surf, "gbm_surface_create");

    g.egl_surf = eglCreateWindowSurface(g.egl_dpy, config,
                                         (EGLNativeWindowType)g.gbm_surf, NULL);
    EGL_CHECK(g.egl_surf, g.egl_surf != EGL_NO_SURFACE, "eglCreateWindowSurface");

    const EGLint ctx_attribs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    g.egl_ctx = eglCreateContext(g.egl_dpy, config, EGL_NO_CONTEXT, ctx_attribs);
    EGL_CHECK(g.egl_ctx, g.egl_ctx != EGL_NO_CONTEXT, "eglCreateContext");

    ok = eglMakeCurrent(g.egl_dpy, g.egl_surf, g.egl_surf, g.egl_ctx);
    EGL_CHECK(ok, ok, "eglMakeCurrent");

    /* ---- Shaders ------------------------------------------------------- */
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
    g.u_alpha_loc     = glGetUniformLocation(prog, "u_alpha");
    glUniform1i(u_tex_loc, 0);

    /* ---- Geometry buffer ----------------------------------------------- */
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

    /* ---- Texture slots ------------------------------------------------- */
    glGenTextures(2, &g.slots[0].tex);
    for (int i = 0; i < 2; ++i) {
        glBindTexture(GL_TEXTURE_2D, g.slots[i].tex);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }

    /* ---- Socket setup -------------------------------------------------- */
    unlink(SOCKET_PATH);
    g.listen_fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    CHECK(g.listen_fd >= 0, "socket");

    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    strncpy(addr.sun_path, SOCKET_PATH, sizeof(addr.sun_path) - 1);

    int ret = bind(g.listen_fd, (struct sockaddr *)&addr, sizeof(addr));
    CHECK(ret == 0, "bind %s", SOCKET_PATH);
    ret = listen(g.listen_fd, 1);
    CHECK(ret == 0, "listen");
    printf("Listening on %s\n", SOCKET_PATH);

    int flags = fcntl(g.listen_fd, F_GETFL, 0);
    fcntl(g.listen_fd, F_SETFL, flags | O_NONBLOCK);

    /* ---- epoll ---------------------------------------------------------- */
    g.epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    CHECK(g.epoll_fd >= 0, "epoll_create1");

    struct epoll_event ev;
    ev.events = EPOLLIN;
    ev.data.fd = g.drm_fd;
    epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.drm_fd, &ev);

    ev.data.fd = g.listen_fd;
    epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.listen_fd, &ev);

    /* ---- Main event loop ----------------------------------------------- */
    drmEventContext evctx = {
        .version = 2,
        .page_flip_handler = page_flip_handler,
    };

    signal(SIGPIPE, SIG_IGN);
    printf("Waiting for 2 images via %s...\n", SOCKET_PATH);

    while (1) {
        if (!g.running) break;

        int timeout = -1;
        if (g.phase == PHASE_HOLDING && !g.hold_complete) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            long long diff_ms =
                (g.hold_deadline.tv_sec - now.tv_sec) * 1000LL +
                (g.hold_deadline.tv_nsec - now.tv_nsec) / 1000000LL;
            if (diff_ms <= 0) {
                timeout = 0;
            } else if (diff_ms < INT_MAX) {
                timeout = (int)diff_ms;
            }
        }

        struct epoll_event events[4];
        int n = epoll_wait(g.epoll_fd, events, 4, timeout);
        if (n < 0) {
            if (errno == EINTR) {
                if (!g.running) break;
                continue;
            }
            perror("epoll_wait");
            break;
        }

        for (int i = 0; i < n; ++i) {
            int fd = events[i].data.fd;
            if (fd == g.drm_fd) {
                drmHandleEvent(g.drm_fd, &evctx);
            } else if (fd == g.listen_fd) {
                int c = accept4(g.listen_fd, NULL, NULL, SOCK_CLOEXEC);
                if (c >= 0) {
                    if (g.conn_fd >= 0) {
                        printf("New manager connection, closing old one\n");
                        close(g.conn_fd);
                        epoll_ctl(g.epoll_fd, EPOLL_CTL_DEL, g.conn_fd, NULL);
                    }
                    g.conn_fd = c;
                    int f = fcntl(g.conn_fd, F_GETFL, 0);
                    fcntl(g.conn_fd, F_SETFL, f | O_NONBLOCK);
                    ev.events = EPOLLIN;
                    ev.data.fd = g.conn_fd;
                    epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.conn_fd, &ev);
                    printf("Manager connected\n");
                }
            } else if (fd == g.conn_fd) {
                handle_socket_data();
            }
        }

        /* Check hold deadline */
        if (g.phase == PHASE_HOLDING && !g.hold_complete) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            if (now.tv_sec > g.hold_deadline.tv_sec ||
                (now.tv_sec == g.hold_deadline.tv_sec &&
                 now.tv_nsec >= g.hold_deadline.tv_nsec)) {
                g.hold_complete = 1;
            }
        }

        /* Waiting for initial images */
        if (g.phase == PHASE_WAITING) {
            if (g.slots[0].occupied && g.slots[1].occupied) {
                printf("Both slots filled. Starting display.\n");
                g.current_slot = 0;

                /* Commit first frame synchronously */
                GLfloat verts[16];
                build_quad((float)g.slots[0].w / g.slots[0].h,
                           g.screen_aspect, verts);
                glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
                glBindTexture(GL_TEXTURE_2D, g.slots[0].tex);
                glUniform1f(g.u_alpha_loc, 1.0f);
                glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

                ok = eglSwapBuffers(g.egl_dpy, g.egl_surf);
                EGL_CHECK(ok, ok, "eglSwapBuffers");

                g.scanout_fb.bo = gbm_surface_lock_front_buffer(g.gbm_surf);
                CHECK(g.scanout_fb.bo, "lock front buffer (init)");
                uint32_t hnd = gbm_bo_get_handle(g.scanout_fb.bo).u32;
                uint32_t pit = gbm_bo_get_stride(g.scanout_fb.bo);
                uint32_t bw  = gbm_bo_get_width(g.scanout_fb.bo);
                uint32_t bh  = gbm_bo_get_height(g.scanout_fb.bo);
                ret = drmModeAddFB(g.drm_fd, bw, bh, 24, 32,
                                   pit, hnd, &g.scanout_fb.fb_id);
                CHECK(ret == 0, "drmModeAddFB (init)");

                if (drmSetMaster(g.drm_fd) < 0) {
                    printf("Warning: drmSetMaster failed: %s\n", strerror(errno));
                }
                drmModeSetCrtc(g.drm_fd, g.crtc_id, 0, 0, 0, NULL, 0, NULL);
                ret = drmModeSetCrtc(g.drm_fd, g.crtc_id, g.scanout_fb.fb_id,
                                     0, 0, &conn->connector_id, 1, mode);
                CHECK(ret == 0, "drmModeSetCrtc (init)");
                printf("First frame committed.\n");

                g.phase = PHASE_HOLDING;
                send_ready();
                clock_gettime(CLOCK_MONOTONIC, &g.hold_deadline);
                g.hold_deadline.tv_sec += (time_t)HOLD_DURATION_SEC;
                g.hold_deadline.tv_nsec += (long)((HOLD_DURATION_SEC - (int)HOLD_DURATION_SEC) * 1e9);
                if (g.hold_deadline.tv_nsec >= 1000000000L) {
                    g.hold_deadline.tv_sec++;
                    g.hold_deadline.tv_nsec -= 1000000000L;
                }
                g.hold_complete = 0;
            }
            continue;
        }

        /* Holding -> start fade if we have a next image */
        if (g.phase == PHASE_HOLDING && g.hold_complete) {
            int next = 1 - g.current_slot;
            if (g.slots[next].occupied) {
                start_fade(g.current_slot, next);
                g.phase = PHASE_FADING;
            }
        }

        /* Advance fade when flip completes */
        if (g.phase == PHASE_FADING && g.flip_done) {
            g.flip_done = 0;
            advance_fade();
        }
    }

    /* ---- Cleanup ------------------------------------------------------- */
    if (g.pending_pixels) stbi_image_free(g.pending_pixels);
    if (g.scanout_fb.bo) {
        drmModeRmFB(g.drm_fd, g.scanout_fb.fb_id);
        gbm_surface_release_buffer(g.gbm_surf, g.scanout_fb.bo);
    }
    if (g.saved_crtc) {
        drmModeSetCrtc(g.drm_fd, g.saved_crtc->crtc_id, g.saved_crtc->buffer_id,
                       g.saved_crtc->x, g.saved_crtc->y,
                       &conn->connector_id, 1, &g.saved_crtc->mode);
        drmModeFreeCrtc(g.saved_crtc);
    }
    eglDestroySurface(g.egl_dpy, g.egl_surf);
    eglDestroyContext(g.egl_dpy, g.egl_ctx);
    eglTerminate(g.egl_dpy);
    gbm_surface_destroy(g.gbm_surf);
    gbm_device_destroy(g.gbm_dev);
    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    close(g.drm_fd);
    if (g.conn_fd >= 0) close(g.conn_fd);
    close(g.listen_fd);
    close(g.epoll_fd);
    unlink(SOCKET_PATH);
    return 0;
}
