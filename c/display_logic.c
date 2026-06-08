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

#include "display_logic.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct display_config read_display_config(void)
{
    struct display_config cfg = {
        .fade_duration = DEFAULT_FADE_DURATION,
        .skip_frames = DEFAULT_SKIP_FRAMES,
    };

    const char *env_fade = getenv("PHOTO_FRAME_FADE_DURATION");
    if (env_fade && env_fade[0] != '\0') {
        cfg.fade_duration = strtof(env_fade, NULL);
        if (cfg.fade_duration < 0.0f) cfg.fade_duration = 0.0f;
    }

    const char *env_skip = getenv("PHOTO_FRAME_SKIP_FRAMES");
    if (env_skip && env_skip[0] != '\0') {
        cfg.skip_frames = (int)strtol(env_skip, NULL, 10);
        if (cfg.skip_frames < 0) cfg.skip_frames = 0;
    }

    printf("Display config: fade=%.1fs skip=%d\n", cfg.fade_duration, cfg.skip_frames);
    return cfg;
}

void build_quad(float img_aspect, float screen_aspect, float *v)
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
    v[0]  = x0; v[1]  = y0; v[2]  = 0.0f; v[3]  = 1.0f;
    v[4]  = x1; v[5]  = y0; v[6]  = 1.0f; v[7]  = 1.0f;
    v[8]  = x0; v[9]  = y1; v[10] = 0.0f; v[11] = 0.0f;
    v[12] = x1; v[13] = y1; v[14] = 1.0f; v[15] = 0.0f;
}

int select_image_destination(int slot0_occupied, int slot1_occupied, int has_pending)
{
    if (!slot0_occupied) return 0;
    if (!slot1_occupied) return 1;
    if (!has_pending) return 2;
    return 3;
}

size_t parse_protocol_buffer(const char *data, size_t len,
    protocol_cmd_handler handler, void *ctx,
    int *paused)
{
    const char *start = data;
    const char *end = data + len;
    const char *nl;

    *paused = 0;

    while ((nl = memchr(start, '\n', end - start)) != NULL) {
        size_t cmd_len = nl - start;
        if (cmd_len >= 4 && strncmp(start, "IMG ", 4) == 0) {
            char path[4096];
            if (cmd_len - 4 < sizeof(path)) {
                memcpy(path, start + 4, cmd_len - 4);
                path[cmd_len - 4] = '\0';
                if (!handler(path, ctx)) {
                    *paused = 1;
                    return nl + 1 - data;
                }
            }
        }
        start = nl + 1;
    }

    return start - data;
}
