// Photo Frame Display — DRM/GBM/EGL digital photo frame.
// Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "display_logic.h"

#define TEST_ASSERT(cond) \
    do { \
        if (!(cond)) { \
            fprintf(stderr, "FAIL: %s at %s:%d\n", #cond, __FILE__, __LINE__); \
            return 1; \
        } \
    } while(0)

static int test_build_quad(void)
{
    float v[16];

    // 16:9 image on 16:9 screen -> full quad
    build_quad(16.0f / 9.0f, 16.0f / 9.0f, v);
    TEST_ASSERT(v[0] == -1.0f);
    TEST_ASSERT(v[4] == 1.0f);
    TEST_ASSERT(v[1] == -1.0f);
    TEST_ASSERT(v[13] == 1.0f);

    // 4:3 image on 16:9 screen -> letterbox
    build_quad(4.0f / 3.0f, 16.0f / 9.0f, v);
    TEST_ASSERT(v[0] > -1.0f && v[0] < 0.0f);
    TEST_ASSERT(v[4] < 1.0f && v[4] > 0.0f);
    TEST_ASSERT(v[1] == -1.0f);
    TEST_ASSERT(v[13] == 1.0f);

    // 21:9 image on 16:9 screen -> pillarbox
    build_quad(21.0f / 9.0f, 16.0f / 9.0f, v);
    TEST_ASSERT(v[0] == -1.0f);
    TEST_ASSERT(v[4] == 1.0f);
    TEST_ASSERT(v[1] > -1.0f && v[1] < 0.0f);
    TEST_ASSERT(v[13] < 1.0f && v[13] > 0.0f);

    printf("PASS: build_quad\n");
    return 0;
}

static int test_read_display_config(void)
{
    // Save and clear env vars
    const char *old_fade = getenv("PHOTO_FRAME_FADE_DURATION");
    const char *old_skip = getenv("PHOTO_FRAME_SKIP_FRAMES");
    if (old_fade) unsetenv("PHOTO_FRAME_FADE_DURATION");
    if (old_skip) unsetenv("PHOTO_FRAME_SKIP_FRAMES");

    struct display_config cfg = read_display_config();
    TEST_ASSERT(cfg.fade_duration == 1.5f);
    TEST_ASSERT(cfg.skip_frames == 0);

    setenv("PHOTO_FRAME_FADE_DURATION", "3.0", 1);
    setenv("PHOTO_FRAME_SKIP_FRAMES", "2", 1);
    cfg = read_display_config();
    TEST_ASSERT(cfg.fade_duration == 3.0f);
    TEST_ASSERT(cfg.skip_frames == 2);

    setenv("PHOTO_FRAME_FADE_DURATION", "-1.0", 1);
    setenv("PHOTO_FRAME_SKIP_FRAMES", "-5", 1);
    cfg = read_display_config();
    TEST_ASSERT(cfg.fade_duration == 0.0f);
    TEST_ASSERT(cfg.skip_frames == 0);

    // Restore env vars
    if (old_fade) setenv("PHOTO_FRAME_FADE_DURATION", old_fade, 1);
    else unsetenv("PHOTO_FRAME_FADE_DURATION");
    if (old_skip) setenv("PHOTO_FRAME_SKIP_FRAMES", old_skip, 1);
    else unsetenv("PHOTO_FRAME_SKIP_FRAMES");

    printf("PASS: read_display_config\n");
    return 0;
}

static int test_select_image_destination(void)
{
    TEST_ASSERT(select_image_destination(0, 0, 0) == 0);
    TEST_ASSERT(select_image_destination(1, 0, 0) == 1);
    TEST_ASSERT(select_image_destination(1, 1, 0) == 2);
    TEST_ASSERT(select_image_destination(1, 1, 1) == 3);
    printf("PASS: select_image_destination\n");
    return 0;
}

struct parse_test_ctx {
    int cmd_count;
    char paths[4][256];
};

static int test_handler(const char *path, void *ctx)
{
    struct parse_test_ctx *t = ctx;
    if (t->cmd_count < 4) {
        strncpy(t->paths[t->cmd_count], path, sizeof(t->paths[0]) - 1);
        t->paths[t->cmd_count][sizeof(t->paths[0]) - 1] = '\0';
    }
    t->cmd_count++;
    return 1;
}

static int test_handler_backpressure(const char *path, void *ctx)
{
    (void)path;
    struct parse_test_ctx *t = ctx;
    t->cmd_count++;
    // Reject after 2 commands
    return t->cmd_count < 2;
}

static int test_parse_protocol_buffer(void)
{
    struct parse_test_ctx ctx = {0};
    int paused = 0;

    // Single command
    const char *buf1 = "IMG /path/to/photo.jpg\n";
    size_t consumed = parse_protocol_buffer(buf1, strlen(buf1), test_handler, &ctx, &paused);
    TEST_ASSERT(consumed == strlen(buf1));
    TEST_ASSERT(ctx.cmd_count == 1);
    TEST_ASSERT(strcmp(ctx.paths[0], "/path/to/photo.jpg") == 0);
    TEST_ASSERT(paused == 0);

    // Multiple commands
    memset(&ctx, 0, sizeof(ctx));
    const char *buf2 = "IMG /a.jpg\nIMG /b.jpg\n";
    consumed = parse_protocol_buffer(buf2, strlen(buf2), test_handler, &ctx, &paused);
    TEST_ASSERT(consumed == strlen(buf2));
    TEST_ASSERT(ctx.cmd_count == 2);
    TEST_ASSERT(strcmp(ctx.paths[0], "/a.jpg") == 0);
    TEST_ASSERT(strcmp(ctx.paths[1], "/b.jpg") == 0);

    // Partial command (no newline)
    memset(&ctx, 0, sizeof(ctx));
    const char *buf3 = "IMG /partial";
    consumed = parse_protocol_buffer(buf3, strlen(buf3), test_handler, &ctx, &paused);
    TEST_ASSERT(consumed == 0);
    TEST_ASSERT(ctx.cmd_count == 0);

    // Unknown command
    memset(&ctx, 0, sizeof(ctx));
    const char *buf4 = "READY\nIMG /c.jpg\n";
    consumed = parse_protocol_buffer(buf4, strlen(buf4), test_handler, &ctx, &paused);
    TEST_ASSERT(consumed == strlen(buf4));
    TEST_ASSERT(ctx.cmd_count == 1);
    TEST_ASSERT(strcmp(ctx.paths[0], "/c.jpg") == 0);

    // Backpressure
    memset(&ctx, 0, sizeof(ctx));
    const char *buf5 = "IMG /1.jpg\nIMG /2.jpg\nIMG /3.jpg\n";
    consumed = parse_protocol_buffer(buf5, strlen(buf5), test_handler_backpressure, &ctx, &paused);
    TEST_ASSERT(consumed == strlen("IMG /1.jpg\nIMG /2.jpg\n"));
    TEST_ASSERT(ctx.cmd_count == 2);
    TEST_ASSERT(paused == 1);

    printf("PASS: parse_protocol_buffer\n");
    return 0;
}

int main(void)
{
    int failures = 0;
    failures += test_build_quad();
    failures += test_read_display_config();
    failures += test_select_image_destination();
    failures += test_parse_protocol_buffer();
    if (failures == 0) {
        printf("\nAll tests passed.\n");
    } else {
        printf("\n%d test(s) failed.\n", failures);
    }
    return failures;
}
