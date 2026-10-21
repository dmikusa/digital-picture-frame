/*
 * Photo Frame Display — DRM/GBM/EGL digital photo frame.
 * Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program. If not, see <https://www.gnu.org/licenses/>.
 *
 * ---------------------------------------------------------------------------
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
 *     echo "IMG /path/to/photo1.jpg" | nc -U /run/photo-frame/photo-frame.sock
 *     echo "IMG /path/to/photo2.jpg" | nc -U /run/photo-frame/photo-frame.sock
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
#include <poll.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/stat.h>
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

#include "display_logic.h"
#include "logger.h"

#define EGL_PLATFORM_GBM_KHR 0x31D7

#ifndef GBM_FORMAT_ARGB8888
#define GBM_FORMAT_ARGB8888 GBM_BO_FORMAT_ARGB8888
#endif

#define SOCKET_PATH            "/run/photo-frame/photo-frame.sock"
#define CONTROL_SOCKET_PATH    "/run/photo-frame/control.sock"
/* tmpfs, so diagnostics never touch the SD card. */
#define DISPLAY_LOG_PATH       "/tmp/photo-frame-display.log"
#define HOLD_DURATION_SEC      5.0f
/* Upper bound on waiting for an in-flight page flip before a SHOW. */
#define FLIP_WAIT_TIMEOUT_MS   1000

#define CHECK(cond, ...) do { \
    if (!(cond)) { \
        log_fatal(__FILE__, __LINE__, __VA_ARGS__); \
        exit(1); /* unreachable; makes the exit path explicit to static analysis */ \
    } \
} while(0)

#define EGL_CHECK(val, ok, call) do { \
    if (!(ok)) { \
        (void)(val); \
        EGLint _egl_err = eglGetError(); \
        log_msg(LOG_ERROR, "Exiting at %s:%d: %s failed (eglGetError=0x%x)", \
                __FILE__, __LINE__, call, _egl_err); \
        fprintf(stderr, "EGL ERROR at %s:%d: %s failed (eglGetError=0x%x)\n", \
                __FILE__, __LINE__, call, _egl_err); \
        log_close(); \
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
    char                 sock_buf[4096];
    size_t               sock_len;

    /* Control socket */
    int                  control_listen_fd;
    int                  control_conn_fd;
    char                 control_buf[2048];
    size_t               control_len;
    int                  paused;
    int                  show_image;
    int                  show_held;      /* a SHOW image is on screen */
    char                 show_path[512];

    /* Fade state */
    int                  fading;
    float                fade_progress;
    struct timespec      fade_start;
    int                  fade_from, fade_to;

    /* Page-flip buffers */
    struct frame_buffer  scanout_fb;
    struct frame_buffer  pending_fb;
    volatile int         flip_done;
    volatile int         flip_pending;  /* page flip queued, not yet acked */

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
    volatile sig_atomic_t term_signal;
} g;

static void signal_handler(int sig)
{
    g.term_signal = sig;
    g.running = 0;
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
        log_msg(LOG_ERROR, "Exiting: shader compile error: %s", log);
        fprintf(stderr, "Shader compile error:\n%s\n", log);
        free(log);
        log_close();
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
        log_msg(LOG_ERROR, "Exiting: program link error: %s", log);
        fprintf(stderr, "Program link error:\n%s\n", log);
        free(log);
        log_close();
        exit(1);
    }
    return p;
}

static void page_flip_handler(int fd, unsigned int frame,
                              unsigned int sec, unsigned int usec,
                              void *data)
{
    (void)fd; (void)frame; (void)sec; (void)usec;
    *(volatile int *)data = 1;
    g.flip_pending = 0;
}

/* -------------------------------------------------------------------------- */
/* Image loading                                                              */
/* -------------------------------------------------------------------------- */

static int load_image_into_slot(int slot_idx, const char *path)
{
    if (g.slots[slot_idx].occupied) {
        log_msg(LOG_WARN, "Slot %d already occupied, overwriting", slot_idx);
    }

    int w, h, ch;
    unsigned char *data = stbi_load(path, &w, &h, &ch, 4);
    if (!data) {
        log_msg(LOG_WARN, "IMG load failed: %s: %s", path, stbi_failure_reason());
        fprintf(stderr, "Failed to load %s: %s\n", path, stbi_failure_reason());
        return 0;
    }
    log_msg(LOG_DEBUG, "Loaded %s -> slot %d (%dx%d)", path, slot_idx, w, h);

    glBindTexture(GL_TEXTURE_2D, g.slots[slot_idx].tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, data);
    stbi_image_free(data);

    g.slots[slot_idx].w = w;
    g.slots[slot_idx].h = h;
    g.slots[slot_idx].occupied = 1;
    return 1;
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
        log_msg(LOG_WARN, "IMG load failed (pending): %s: %s", path, stbi_failure_reason());
        fprintf(stderr, "Failed to load %s: %s\n", path, stbi_failure_reason());
        return;
    }
    log_msg(LOG_DEBUG, "Buffered %s as pending (%dx%d)", path, w, h);

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
    log_msg(LOG_DEBUG, "Uploaded pending image to slot %d", slot_idx);
}

/* -------------------------------------------------------------------------- */
/* Socket protocol                                                            */
/* -------------------------------------------------------------------------- */

/* Close the manager connection and drop any partial command so a reconnect
 * cannot inherit bytes from the previous session. */
static void close_manager_conn(const char *reason)
{
    if (g.conn_fd < 0) return;
    log_msg(LOG_INFO, "Closing manager connection (fd=%d): %s", g.conn_fd, reason);
    close(g.conn_fd);
    g.conn_fd = -1;
    g.socket_paused = 0;
    g.sock_len = 0;
    g.sock_buf[0] = '\0';
}

static void send_ready(void)
{
    if (g.conn_fd < 0) return;
    const char msg[] = "READY\n";
    ssize_t n = write(g.conn_fd, msg, sizeof(msg) - 1);
    if (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EPIPE) {
        log_msg(LOG_ERROR, "write READY failed: %s", strerror(errno));
        fprintf(stderr, "write READY: %s\n", strerror(errno));
        close_manager_conn("READY write error");
    } else if (n > 0) {
        log_msg(LOG_DEBUG, "Sent READY");
    } else if (n < 0 && errno == EPIPE) {
        log_msg(LOG_INFO, "Manager disconnected before READY could be sent");
        close_manager_conn("EPIPE while sending READY");
    }
}

static void handle_img_command(const char *path)
{
    log_msg(LOG_DEBUG, "Received IMG: %s", path);
    int dest = select_image_destination(
        g.slots[0].occupied, g.slots[1].occupied, g.pending_pixels != NULL);
    switch (dest) {
        case 0: load_image_into_slot(0, path); break;
        case 1: load_image_into_slot(1, path); break;
        case 2: store_pending_image(path); break;
        case 3:
            log_msg(LOG_WARN, "Both slots and pending buffer full, dropping %s", path);
            break;
    }
}

static int handle_img_cmd_wrapper(const char *path, void *ctx)
{
    (void)ctx;
    handle_img_command(path);
    return !(g.slots[0].occupied && g.slots[1].occupied && g.pending_pixels);
}

/* Advance declarations: the control-loss path resumes the display socket,
 * which is defined with the rest of the socket handling below. */
static void handle_socket_data(void);
static void resume_display_socket(const char *reason);

/* Re-add the display socket to epoll after it was paused for backpressure.
 * Needed when a control-mode hold ends: CLR frees the slots, but no fade will
 * complete to trigger the usual resume. */
static void resume_display_socket(const char *reason)
{
    if (!g.socket_paused || g.conn_fd < 0) return;

    struct epoll_event ev;
    ev.events = EPOLLIN;
    ev.data.fd = g.conn_fd;
    int ret = epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.conn_fd, &ev);
    if (ret < 0 && errno != EEXIST) {
        log_msg(LOG_ERROR, "epoll_ctl ADD (resume: %s) failed: %s",
                reason, strerror(errno));
        return;
    }
    g.socket_paused = 0;
    log_msg(LOG_INFO, "Display socket resumed (%s)", reason);
    handle_socket_data();
}

/* Drop a control-mode hold when the control connection goes away. The manager
 * may have crashed or restarted, so never leave the frame paused on a stale
 * QR/status image. Intentional shutdown does not auto-resume. */
static void auto_resume_control_hold(const char *reason)
{
    if (!control_loss_should_resume(g.running, g.paused, g.show_image, g.show_held))
        return;

    log_msg(LOG_WARN,
            "Control connection lost while %s; auto-resuming slideshow (%s)",
            g.show_held ? "holding a SHOW image" :
            (g.paused ? "paused" : "a SHOW command was pending"), reason);

    g.paused = 0;
    g.show_image = 0;
    if (g.show_held) {
        g.show_held = 0;
        /* Forget the held QR/status texture so the next photos refill the slot
         * instead of replaying the stale image. The buffer on screen is owned
         * by scanout_fb and is untouched. */
        if (g.phase == PHASE_WAITING) {
            g.slots[0].occupied = 0;
        }
    }
    resume_display_socket("control connection lost");
}

/* Close the control connection and drop any partial command. epoll removes
 * closed descriptors automatically, but callers that replace a live connection
 * delete it explicitly first. */
static void close_control_conn(const char *reason)
{
    if (g.control_conn_fd < 0) return;
    log_msg(LOG_INFO, "Closing control connection (fd=%d): %s",
            g.control_conn_fd, reason);
    close(g.control_conn_fd);
    g.control_conn_fd = -1;
    g.control_len = 0;
    g.control_buf[0] = '\0';
    auto_resume_control_hold(reason);
}

static void handle_control_data(void)
{
    if (g.control_conn_fd < 0) return;

    /* A single command longer than the buffer has no newline yet. Discarding
     * is safer than calling read() with a zero-length buffer, which returns 0
     * and used to be mistaken for EOF (closing a healthy connection). */
    if (g.control_len >= sizeof(g.control_buf) - 1) {
        log_msg(LOG_WARN, "Control command too long (%zu bytes), discarding buffer",
                g.control_len);
        g.control_len = 0;
        g.control_buf[0] = '\0';
    }

    size_t space = sizeof(g.control_buf) - g.control_len - 1;
    ssize_t n = read(g.control_conn_fd, g.control_buf + g.control_len, space);
    if (n < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK)
            return;
        log_msg(LOG_ERROR, "Control read failed: %s", strerror(errno));
        close_control_conn("read error");
        return;
    }
    if (n == 0) {
        close_control_conn("client closed connection (EOF)");
        return;
    }
    g.control_len += (size_t)n;
    g.control_buf[g.control_len] = '\0';

    char *line_start = g.control_buf;
    while (1) {
        char *nl = strchr(line_start, '\n');
        if (!nl) break;

        *nl = '\0';
        char *cmd = line_start;
        line_start = nl + 1;

        if (strcmp(cmd, "CLR") == 0) {
            log_msg(LOG_INFO, "Control command: CLR (clear queue, pause)");
            g.paused = 1;
            g.show_image = 0;
            g.show_held = 0;
            if (g.slots[0].occupied) { g.slots[0].occupied = 0; }
            if (g.slots[1].occupied) { g.slots[1].occupied = 0; }
            if (g.pending_pixels) {
                stbi_image_free(g.pending_pixels);
                g.pending_pixels = NULL;
            }
            if (g.fading) {
                g.fading = 0;
            }
            g.phase = PHASE_WAITING;
        } else if (strncmp(cmd, "SHOW ", 5) == 0) {
            log_msg(LOG_INFO, "Control command: SHOW %s", cmd + 5);
            size_t path_len = strlen(cmd + 5);
            if (path_len >= sizeof(g.show_path)) {
                log_msg(LOG_WARN, "SHOW path truncated from %zu to %zu bytes",
                        path_len, sizeof(g.show_path) - 1);
            }
            strncpy(g.show_path, cmd + 5, sizeof(g.show_path) - 1);
            g.show_path[sizeof(g.show_path) - 1] = '\0';
            g.show_image = 1;
        } else if (strcmp(cmd, "RESUME") == 0) {
            log_msg(LOG_INFO, "Control command: RESUME");
            g.paused = 0;
            g.show_image = 0;
            g.show_held = 0;
            /* CLR freed the slots while the socket was paused; re-add it so
             * queued IMG commands can refill them. */
            resume_display_socket("RESUME");
        } else {
            log_msg(LOG_WARN, "Unknown control command: '%s'", cmd);
        }
    }

    size_t remaining = g.control_len - (size_t)(line_start - g.control_buf);
    if (remaining > 0) {
        memmove(g.control_buf, line_start, remaining);
    }
    g.control_len = remaining;
    g.control_buf[g.control_len] = '\0';
}

static void handle_socket_data(void)
{
    if (g.conn_fd < 0) return;

    /* A partial IMG line larger than the buffer: drop it instead of issuing a
     * zero-length read, which returns 0 and looks like EOF. */
    if (g.sock_len >= sizeof(g.sock_buf) - 1) {
        log_msg(LOG_WARN, "Display socket line too long (%zu bytes), discarding buffer",
                g.sock_len);
        g.sock_len = 0;
        g.sock_buf[0] = '\0';
    }

    ssize_t n = read(g.conn_fd, g.sock_buf + g.sock_len,
                     sizeof(g.sock_buf) - g.sock_len - 1);
    if (n < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK)
            return;
        log_msg(LOG_ERROR, "Display socket read failed: %s", strerror(errno));
        fprintf(stderr, "read socket: %s\n", strerror(errno));
        close_manager_conn("display socket read error");
        return;
    }
    if (n == 0) {
        close_manager_conn("manager closed display socket (EOF)");
        return;
    }
    g.sock_len += (size_t)n;

    int paused = 0;
    size_t consumed = parse_protocol_buffer(g.sock_buf, g.sock_len,
                                            handle_img_cmd_wrapper, NULL, &paused);
    if (consumed > 0) {
        memmove(g.sock_buf, g.sock_buf + consumed, g.sock_len - consumed);
        g.sock_len -= consumed;
    }
    if (paused && !g.socket_paused) {
        g.socket_paused = 1;
        epoll_ctl(g.epoll_fd, EPOLL_CTL_DEL, g.conn_fd, NULL);
        log_msg(LOG_INFO, "Display socket paused (backpressure)");
    }
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
    build_quad(image_aspect_ratio(g.slots[from_slot].w, g.slots[from_slot].h),
               g.screen_aspect, verts);
    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
    glBindTexture(GL_TEXTURE_2D, g.slots[from_slot].tex);
    glUniform1f(g.u_alpha_loc, 1.0f - mix);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    /* To image */
    build_quad(image_aspect_ratio(g.slots[to_slot].w, g.slots[to_slot].h),
               g.screen_aspect, verts);
    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
    glBindTexture(GL_TEXTURE_2D, g.slots[to_slot].tex);
    glUniform1f(g.u_alpha_loc, mix);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    glDisable(GL_BLEND);
}

/* Release the old scanout buffer and make the just-presented pending buffer
 * the one currently on screen. Safe to call when there is no pending buffer. */
static void promote_pending_fb(void)
{
    if (!g.pending_fb.bo) return;

    if (g.scanout_fb.bo) {
        drmModeRmFB(g.drm_fd, g.scanout_fb.fb_id);
        gbm_surface_release_buffer(g.gbm_surf, g.scanout_fb.bo);
    }
    g.scanout_fb = g.pending_fb;
    g.pending_fb.bo    = NULL;
    g.pending_fb.fb_id = 0;
}

/* Wait (bounded) for an in-flight page flip to be acknowledged by the kernel.
 * Returns 0 when no flip is pending, -1 on timeout or error. */
static int wait_for_pending_flip(int timeout_ms)
{
    struct timespec start, now;
    clock_gettime(CLOCK_MONOTONIC, &start);

    drmEventContext evctx = {
        .version = 2,
        .page_flip_handler = page_flip_handler,
    };

    while (g.flip_pending) {
        clock_gettime(CLOCK_MONOTONIC, &now);
        long long elapsed_ms = (now.tv_sec - start.tv_sec) * 1000LL
                             + (now.tv_nsec - start.tv_nsec) / 1000000LL;
        int remaining = timeout_ms - (int)elapsed_ms;
        if (remaining <= 0) break;

        struct pollfd pfd = { .fd = g.drm_fd, .events = POLLIN, .revents = 0 };
        int pr = poll(&pfd, 1, remaining);
        if (pr < 0) {
            if (errno == EINTR) {
                if (!g.running) return -1;
                continue;
            }
            log_msg(LOG_ERROR, "poll during flip wait failed: %s", strerror(errno));
            fprintf(stderr, "poll (flip wait): %s\n", strerror(errno));
            return -1;
        }
        if (pr == 0) break;

        if (drmHandleEvent(g.drm_fd, &evctx) != 0) {
            log_msg(LOG_ERROR, "drmHandleEvent during flip wait failed: %s",
                    strerror(errno));
            fprintf(stderr, "drmHandleEvent (flip wait): %s\n", strerror(errno));
            return -1;
        }
    }

    if (g.flip_pending) {
        log_msg(LOG_WARN, "Page flip still pending after %d ms", timeout_ms);
        fprintf(stderr, "Warning: page flip still pending after %d ms\n", timeout_ms);
        return -1;
    }
    log_msg(LOG_DEBUG, "Pending page flip drained");
    return 0;
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
    g.flip_pending = 1;
}

static void start_fade(int from_slot, int to_slot)
{
    log_msg(LOG_DEBUG, "Starting fade %d -> %d", from_slot, to_slot);
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
    promote_pending_fb();

    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    float elapsed = (float)(now.tv_sec - g.fade_start.tv_sec)
                  + (float)(now.tv_nsec - g.fade_start.tv_nsec) / 1e9f;
    g.fade_progress = elapsed / g.fade_duration;
    if (g.fade_progress > 1.0f) g.fade_progress = 1.0f;

    if (g.fade_progress >= 1.0f) {
        /* Fade complete */
        log_msg(LOG_DEBUG, "Fade complete. Now showing slot %d", g.fade_to);

        int old_slot = g.current_slot;
        g.current_slot = g.fade_to;
        g.slots[old_slot].occupied = 0;

        /* Upload any pending CPU buffer into the freed slot */
        if (g.pending_pixels) {
            upload_pending_to_slot(old_slot);
        }

        /* Resume socket reads if we had backpressure */
        resume_display_socket("fade complete");

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
        g.flip_pending = 1;
        return;
    }

    /* Render next frame */
    render_frame(g.fade_progress, g.fade_from, g.fade_to);
    request_page_flip();
}

/* -------------------------------------------------------------------------- */
/* Control-mode SHOW                                                          */
/* -------------------------------------------------------------------------- */

/* Draw the held image and put it on screen. Every failure is logged: the old
 * scanout stays live and the process keeps running. Returns 0 on success. */
static int handle_show_command(drmModeConnector *conn, drmModeModeInfo *mode)
{
    log_msg(LOG_INFO, "SHOW begin: %s", g.show_path);

    /* A slideshow page flip may still be in flight. drmModeSetCrtc returns
     * EBUSY until the kernel acknowledges it, so drain the event first. */
    if (g.flip_pending) {
        log_msg(LOG_DEBUG, "SHOW: waiting up to %d ms for pending page flip",
                FLIP_WAIT_TIMEOUT_MS);
        if (wait_for_pending_flip(FLIP_WAIT_TIMEOUT_MS) < 0) {
            log_msg(LOG_WARN, "SHOW aborted: page flip still pending for %s",
                    g.show_path);
            fprintf(stderr, "Warning: skipping SHOW %s, page flip still pending\n",
                    g.show_path);
            return -1;
        }
    }
    g.flip_done = 0;

    /* Fold a completed flip into scanout so both buffers agree. */
    promote_pending_fb();

    if (!load_image_into_slot(0, g.show_path)) {
        log_msg(LOG_WARN, "SHOW aborted: cannot load %s, keeping current frame",
                g.show_path);
        fprintf(stderr, "Warning: cannot show %s, keeping current frame\n",
                g.show_path);
        return -1;
    }
    g.current_slot = 0;
    g.slots[1].occupied = 0;
    log_msg(LOG_DEBUG, "SHOW: loaded %s (%dx%d)",
            g.show_path, g.slots[0].w, g.slots[0].h);

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindTexture(GL_TEXTURE_2D, g.slots[0].tex);
    glUniform1f(g.u_alpha_loc, 1.0f);
    GLfloat verts[16];
    build_quad(image_aspect_ratio(g.slots[0].w, g.slots[0].h),
               g.screen_aspect, verts);
    glBufferSubData(GL_ARRAY_BUFFER, 0, sizeof(verts), verts);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);

    if (!eglSwapBuffers(g.egl_dpy, g.egl_surf)) {
        EGLint egl_err = eglGetError();
        log_msg(LOG_ERROR, "SHOW: eglSwapBuffers failed (eglGetError=0x%x)", egl_err);
        fprintf(stderr, "Warning: eglSwapBuffers failed for SHOW (eglGetError=0x%x)\n",
                egl_err);
        return -1;
    }
    log_msg(LOG_DEBUG, "SHOW: eglSwapBuffers ok");

    struct gbm_bo *new_bo = gbm_surface_lock_front_buffer(g.gbm_surf);
    if (!new_bo) {
        log_msg(LOG_ERROR, "SHOW: gbm_surface_lock_front_buffer failed");
        fprintf(stderr, "Warning: gbm_surface_lock_front_buffer failed for SHOW\n");
        return -1;
    }
    log_msg(LOG_DEBUG, "SHOW: locked front buffer (%ux%u)",
            gbm_bo_get_width(new_bo), gbm_bo_get_height(new_bo));

    uint32_t new_fb = 0;
    int ret = drmModeAddFB(g.drm_fd,
                           gbm_bo_get_width(new_bo), gbm_bo_get_height(new_bo),
                           24, 32, gbm_bo_get_stride(new_bo),
                           gbm_bo_get_handle(new_bo).u32, &new_fb);
    if (ret != 0) {
        log_msg(LOG_ERROR, "SHOW: drmModeAddFB failed: %s", strerror(errno));
        fprintf(stderr, "Warning: drmModeAddFB failed for SHOW: %s\n",
                strerror(errno));
        gbm_surface_release_buffer(g.gbm_surf, new_bo);
        log_msg(LOG_DEBUG, "SHOW: released new buffer after AddFB failure");
        return -1;
    }
    log_msg(LOG_DEBUG, "SHOW: drmModeAddFB ok (fb=%u)", new_fb);

    ret = drmModeSetCrtc(g.drm_fd, g.crtc_id, new_fb, 0, 0,
                         &conn->connector_id, 1, mode);
    if (ret != 0) {
        log_msg(LOG_ERROR, "SHOW: drmModeSetCrtc failed: %s", strerror(errno));
        fprintf(stderr, "Warning: drmModeSetCrtc failed for SHOW: %s\n",
                strerror(errno));
        drmModeRmFB(g.drm_fd, new_fb);
        gbm_surface_release_buffer(g.gbm_surf, new_bo);
        log_msg(LOG_DEBUG, "SHOW: cleaned up new fb/buffer, old scanout kept");
        return -1;
    }

    /* The new framebuffer is live; only now release the old scanout buffer. */
    if (g.scanout_fb.bo) {
        log_msg(LOG_DEBUG, "SHOW: releasing old scanout fb=%u",
                g.scanout_fb.fb_id);
        drmModeRmFB(g.drm_fd, g.scanout_fb.fb_id);
        gbm_surface_release_buffer(g.gbm_surf, g.scanout_fb.bo);
    }
    g.scanout_fb.bo    = new_bo;
    g.scanout_fb.fb_id = new_fb;
    g.show_held = 1;

    log_msg(LOG_INFO, "SHOW displayed: %s (fb=%u)", g.show_path, new_fb);
    return 0;
}

/* -------------------------------------------------------------------------- */
/* Main                                                                       */
/* -------------------------------------------------------------------------- */

int main(void)
{
    memset(&g, 0, sizeof(g));
    g.running = 1;

    /* stdout/stderr are suppressed by systemd; diagnostics go to tmpfs. */
    if (log_open(DISPLAY_LOG_PATH) != 0) {
        fprintf(stderr, "Warning: cannot open %s, logging to stderr\n",
                DISPLAY_LOG_PATH);
    }
    log_msg(LOG_INFO, "Starting photo-frame-display (log %s)", DISPLAY_LOG_PATH);

    struct display_config cfg = read_display_config();
    g.fade_duration = cfg.fade_duration;
    g.skip_frames = cfg.skip_frames;
    log_msg(LOG_INFO, "Display config: fade=%.1fs skip_frames=%d",
            cfg.fade_duration, cfg.skip_frames);

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = signal_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0; /* do NOT restart syscalls */
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    log_msg(LOG_DEBUG, "Signal handlers installed for SIGTERM/SIGINT");

    /* ---- DRM/GBM/EGL setup --------------------------------------------- */
    g.drm_fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(g.drm_fd >= 0, "open /dev/dri/card0");
    log_msg(LOG_INFO, "Opened /dev/dri/card0 (fd=%d)", g.drm_fd);

    g.gbm_dev = gbm_create_device(g.drm_fd);
    CHECK(g.gbm_dev, "gbm_create_device");
    log_msg(LOG_DEBUG, "GBM device created");

    PFNEGLGETPLATFORMDISPLAYEXTPROC eglGetPlatformDisplayEXT =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    CHECK(eglGetPlatformDisplayEXT, "eglGetProcAddress");

    g.egl_dpy = eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR, g.gbm_dev, NULL);
    EGL_CHECK(g.egl_dpy, g.egl_dpy != EGL_NO_DISPLAY, "eglGetPlatformDisplayEXT");

    EGLBoolean ok = eglInitialize(g.egl_dpy, NULL, NULL);
    EGL_CHECK(ok, ok, "eglInitialize");
    ok = eglBindAPI(EGL_OPENGL_ES_API);
    EGL_CHECK(ok, ok, "eglBindAPI");
    log_msg(LOG_INFO, "EGL initialized: %s",
            eglQueryString(g.egl_dpy, EGL_VERSION) ?
                eglQueryString(g.egl_dpy, EGL_VERSION) : "unknown");

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
    g.screen_aspect = image_aspect_ratio(g.mode_w, g.mode_h);
    log_msg(LOG_INFO, "Mode: %dx%d (aspect %.3f)", g.mode_w, g.mode_h, g.screen_aspect);

    const char *sock_path_env = getenv("DISPLAY_SOCKET_PATH");
    const char *sock_path = sock_path_env ? sock_path_env : SOCKET_PATH;
    const char *ctrl_sock_path_env = getenv("CONTROL_SOCKET_PATH");
    const char *ctrl_sock_path = ctrl_sock_path_env ? ctrl_sock_path_env : CONTROL_SOCKET_PATH;
    log_msg(LOG_INFO, "Display socket path: %s", sock_path);
    log_msg(LOG_INFO, "Control socket path: %s", ctrl_sock_path);

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
    log_msg(LOG_INFO, "Using CRTC %u", g.crtc_id);

    g.saved_crtc = drmModeGetCrtc(g.drm_fd, g.crtc_id);
    if (!g.saved_crtc) {
        log_msg(LOG_WARN, "drmModeGetCrtc failed: %s", strerror(errno));
    }

    g.gbm_surf = gbm_surface_create(g.gbm_dev, g.mode_w, g.mode_h,
                                    GBM_FORMAT_ARGB8888,
                                    GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
    CHECK(g.gbm_surf, "gbm_surface_create");
    log_msg(LOG_DEBUG, "GBM surface created (%dx%d)", g.mode_w, g.mode_h);

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
    unlink(sock_path);
    g.listen_fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    CHECK(g.listen_fd >= 0, "socket");

    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    strncpy(addr.sun_path, sock_path, sizeof(addr.sun_path) - 1);

    /* Restrict socket permissions so only the owner can connect */
    mode_t old_umask = umask(077);
    int ret = bind(g.listen_fd, (struct sockaddr *)&addr, sizeof(addr));
    umask(old_umask);
    CHECK(ret == 0, "bind %s", sock_path);
    ret = listen(g.listen_fd, 1);
    CHECK(ret == 0, "listen");
    log_msg(LOG_INFO, "Display socket listening on %s", sock_path);

    int flags = fcntl(g.listen_fd, F_GETFL, 0);
    fcntl(g.listen_fd, F_SETFL, flags | O_NONBLOCK);

    /* ---- Control socket setup ------------------------------------------ */
    unlink(ctrl_sock_path);
    g.control_listen_fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    CHECK(g.control_listen_fd >= 0, "control socket");

    struct sockaddr_un ctrl_addr = { .sun_family = AF_UNIX };
    strncpy(ctrl_addr.sun_path, ctrl_sock_path, sizeof(ctrl_addr.sun_path) - 1);

    old_umask = umask(077);
    ret = bind(g.control_listen_fd, (struct sockaddr *)&ctrl_addr, sizeof(ctrl_addr));
    umask(old_umask);
    CHECK(ret == 0, "bind %s", ctrl_sock_path);
    ret = listen(g.control_listen_fd, 1);
    CHECK(ret == 0, "control listen");
    log_msg(LOG_INFO, "Control socket listening on %s", ctrl_sock_path);

    flags = fcntl(g.control_listen_fd, F_GETFL, 0);
    fcntl(g.control_listen_fd, F_SETFL, flags | O_NONBLOCK);

    /* ---- epoll ---------------------------------------------------------- */
    g.epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    CHECK(g.epoll_fd >= 0, "epoll_create1");

    struct epoll_event ev;
    ev.events = EPOLLIN;
    ev.data.fd = g.drm_fd;
    if (epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.drm_fd, &ev) != 0) {
        log_msg(LOG_ERROR, "epoll_ctl ADD drm fd failed: %s", strerror(errno));
    }

    ev.data.fd = g.listen_fd;
    if (epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.listen_fd, &ev) != 0) {
        log_msg(LOG_ERROR, "epoll_ctl ADD display socket failed: %s", strerror(errno));
    }

    ev.data.fd = g.control_listen_fd;
    if (epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.control_listen_fd, &ev) != 0) {
        log_msg(LOG_ERROR, "epoll_ctl ADD control socket failed: %s", strerror(errno));
    }

    /* ---- Main event loop ----------------------------------------------- */
    drmEventContext evctx = {
        .version = 2,
        .page_flip_handler = page_flip_handler,
    };

    signal(SIGPIPE, SIG_IGN);
    log_msg(LOG_INFO, "Waiting for 2 images via %s", sock_path);

    while (1) {
        if (!g.running) break;

        /* Handle SHOW command from control socket (connector is accessible here) */
        if (g.show_image && g.paused) {
            g.show_image = 0;
            handle_show_command(conn, mode);
        }

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

        struct epoll_event events[6];
        int n = epoll_wait(g.epoll_fd, events, 6, timeout);
        if (n < 0) {
            if (errno == EINTR) {
                if (!g.running) break;
                continue;
            }
            log_msg(LOG_ERROR, "epoll_wait failed: %s", strerror(errno));
            fprintf(stderr, "epoll_wait: %s\n", strerror(errno));
            break;
        }

        for (int i = 0; i < n; ++i) {
            int fd = events[i].data.fd;
            if (fd == g.drm_fd) {
                if (drmHandleEvent(g.drm_fd, &evctx) != 0) {
                    log_msg(LOG_ERROR, "drmHandleEvent failed: %s", strerror(errno));
                }
            } else if (fd == g.listen_fd) {
                int c = accept4(g.listen_fd, NULL, NULL, SOCK_CLOEXEC);
                if (c < 0) {
                    if (errno != EAGAIN && errno != EWOULDBLOCK) {
                        log_msg(LOG_WARN, "accept4(display socket) failed: %s",
                                strerror(errno));
                    }
                } else {
                    if (g.conn_fd >= 0) {
                        epoll_ctl(g.epoll_fd, EPOLL_CTL_DEL, g.conn_fd, NULL);
                        close_manager_conn("replaced by new manager connection");
                    }
                    g.conn_fd = c;
                    g.sock_len = 0;
                    g.sock_buf[0] = '\0';
                    int f = fcntl(g.conn_fd, F_GETFL, 0);
                    if (f < 0) {
                        log_msg(LOG_WARN, "fcntl(GETFL) on manager fd failed: %s",
                                strerror(errno));
                    } else {
                        fcntl(g.conn_fd, F_SETFL, f | O_NONBLOCK);
                    }
                    ev.events = EPOLLIN;
                    ev.data.fd = g.conn_fd;
                    if (epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD, g.conn_fd, &ev) != 0) {
                        log_msg(LOG_ERROR, "epoll_ctl ADD manager fd failed: %s",
                                strerror(errno));
                        close_manager_conn("epoll add failed");
                    } else {
                        log_msg(LOG_INFO, "Manager connected (fd=%d)", g.conn_fd);
                    }
                }
            } else if (fd == g.control_listen_fd) {
                int cc = accept4(g.control_listen_fd, NULL, NULL, SOCK_CLOEXEC);
                if (cc < 0) {
                    if (errno != EAGAIN && errno != EWOULDBLOCK) {
                        log_msg(LOG_WARN, "accept4(control socket) failed: %s",
                                strerror(errno));
                    }
                } else {
                    if (g.control_conn_fd >= 0) {
                        epoll_ctl(g.epoll_fd, EPOLL_CTL_DEL, g.control_conn_fd, NULL);
                        close_control_conn("replaced by new control client");
                    }
                    g.control_conn_fd = cc;
                    g.control_len = 0;
                    g.control_buf[0] = '\0';
                    int f2 = fcntl(g.control_conn_fd, F_GETFL, 0);
                    if (f2 < 0) {
                        log_msg(LOG_WARN, "fcntl(GETFL) on control fd failed: %s",
                                strerror(errno));
                    } else {
                        fcntl(g.control_conn_fd, F_SETFL, f2 | O_NONBLOCK);
                    }
                    ev.events = EPOLLIN;
                    ev.data.fd = g.control_conn_fd;
                    if (epoll_ctl(g.epoll_fd, EPOLL_CTL_ADD,
                                  g.control_conn_fd, &ev) != 0) {
                        log_msg(LOG_ERROR, "epoll_ctl ADD control fd failed: %s",
                                strerror(errno));
                        close_control_conn("epoll add failed");
                    } else {
                        log_msg(LOG_INFO, "Control client connected (fd=%d)",
                                g.control_conn_fd);
                    }
                }
            } else if (fd == g.control_conn_fd) {
                handle_control_data();
            } else if (fd == g.conn_fd) {
                handle_socket_data();
            }
        }

        /* A flip that completed outside the fade path (for example after CLR
         * aborted a fade) still owns its pending buffer. Promote it so scanout
         * stays accurate and the pending framebuffer is not leaked. */
        if (g.phase != PHASE_FADING && g.flip_done) {
            g.flip_done = 0;
            promote_pending_fb();
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
            if (g.slots[0].occupied && g.slots[1].occupied && g.flip_pending) {
                /* A flip left over from an aborted fade is still in flight.
                 * drmModeSetCrtc would return EBUSY, so drain it first. */
                if (wait_for_pending_flip(FLIP_WAIT_TIMEOUT_MS) < 0) {
                    log_msg(LOG_WARN, "Deferring first frame: page flip still pending");
                    continue;
                }
                g.flip_done = 0;
                promote_pending_fb();
            }
            if (g.slots[0].occupied && g.slots[1].occupied) {
                log_msg(LOG_INFO, "Both slots filled, starting display");
                g.current_slot = 0;

                /* Commit first frame synchronously */
                GLfloat verts[16];
                build_quad(image_aspect_ratio(g.slots[0].w, g.slots[0].h),
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
                    log_msg(LOG_WARN, "drmSetMaster failed: %s", strerror(errno));
                }
                drmModeSetCrtc(g.drm_fd, g.crtc_id, 0, 0, 0, NULL, 0, NULL);
                ret = drmModeSetCrtc(g.drm_fd, g.crtc_id, g.scanout_fb.fb_id,
                                     0, 0, &conn->connector_id, 1, mode);
                CHECK(ret == 0, "drmModeSetCrtc (init)");
                log_msg(LOG_INFO, "First frame committed (fb=%u)",
                        g.scanout_fb.fb_id);

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

        /* Holding -> start fade if we have a next image (unless paused) */
        if (g.phase == PHASE_HOLDING && g.hold_complete && !g.paused) {
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
    if (g.term_signal == SIGTERM) {
        log_msg(LOG_INFO, "Received SIGTERM, shutting down");
    } else if (g.term_signal == SIGINT) {
        log_msg(LOG_INFO, "Received SIGINT, shutting down");
    } else {
        log_msg(LOG_INFO, "Display loop exited, shutting down");
    }

    /* Prevent control-connection teardown below from auto-resuming into GL
     * code that has already been destroyed. */
    g.running = 0;

    if (g.pending_pixels) stbi_image_free(g.pending_pixels);
    if (g.pending_fb.bo) {
        log_msg(LOG_DEBUG, "Releasing pending fb=%u", g.pending_fb.fb_id);
        drmModeRmFB(g.drm_fd, g.pending_fb.fb_id);
        gbm_surface_release_buffer(g.gbm_surf, g.pending_fb.bo);
    }
    if (g.scanout_fb.bo) {
        log_msg(LOG_DEBUG, "Releasing scanout fb=%u", g.scanout_fb.fb_id);
        drmModeRmFB(g.drm_fd, g.scanout_fb.fb_id);
        gbm_surface_release_buffer(g.gbm_surf, g.scanout_fb.bo);
    }
    if (g.saved_crtc) {
        drmModeSetCrtc(g.drm_fd, g.saved_crtc->crtc_id, g.saved_crtc->buffer_id,
                       g.saved_crtc->x, g.saved_crtc->y,
                       &conn->connector_id, 1, &g.saved_crtc->mode);
        drmModeFreeCrtc(g.saved_crtc);
        log_msg(LOG_DEBUG, "Restored saved CRTC");
    }
    eglDestroySurface(g.egl_dpy, g.egl_surf);
    eglDestroyContext(g.egl_dpy, g.egl_ctx);
    eglTerminate(g.egl_dpy);
    gbm_surface_destroy(g.gbm_surf);
    gbm_device_destroy(g.gbm_dev);
    drmModeFreeConnector(conn);
    drmModeFreeResources(res);
    close(g.drm_fd);
    close_manager_conn("shutdown");
    close(g.listen_fd);
    close_control_conn("shutdown");
    close(g.control_listen_fd);
    close(g.epoll_fd);
    unlink(sock_path);
    unlink(ctrl_sock_path);
    log_msg(LOG_INFO, "Clean shutdown complete");
    log_close();
    return 0;
}
