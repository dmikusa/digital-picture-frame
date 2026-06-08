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

#ifndef DISPLAY_LOGIC_H
#define DISPLAY_LOGIC_H

#include <stddef.h>

#define DEFAULT_FADE_DURATION  1.5f
#define DEFAULT_SKIP_FRAMES    0

struct display_config {
    float fade_duration;
    int skip_frames;
};

struct display_config read_display_config(void);
void build_quad(float img_aspect, float screen_aspect, float *v);

/* Returns: 0 = slot 0, 1 = slot 1, 2 = pending, 3 = drop */
int select_image_destination(int slot0_occupied, int slot1_occupied, int has_pending);

/* Protocol parser callback. Returns 1 if accepted, 0 if rejected (backpressure). */
typedef int (*protocol_cmd_handler)(const char *path, void *ctx);

/* Parse newline-delimited protocol buffer.
 * Returns bytes consumed. Sets *paused = 1 if handler returned 0.
 * Caller must memmove remaining bytes. */
size_t parse_protocol_buffer(const char *data, size_t len,
    protocol_cmd_handler handler, void *ctx,
    int *paused);

#endif
