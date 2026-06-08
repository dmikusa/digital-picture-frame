# Photo Frame Display — DRM/GBM/EGL digital photo frame.
# Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this program. If not, see <https://www.gnu.org/licenses/>.

# Makefile for C display app and example clients
#
# Targets:
#   make          - build photo-frame-display
#   make clean    - remove built binaries
#   make run      - build and run photo-frame-display

CC      := gcc
CFLAGS  := -Wall -Wextra -O2 -std=gnu11
LDFLAGS :=

# photo-frame-display requires DRM, EGL, GLES, GBM, and math
FRAME_LIBS := -lEGL -lGLESv2 -lgbm $(shell pkg-config --libs libdrm) -lm
FRAME_CFLAGS := $(CFLAGS) $(shell pkg-config --cflags libdrm)

# photo-frame-display-client has no special library dependencies
CLIENT_LIBS  :=

.PHONY: all clean run

all: photo-frame-display

photo-frame-display: photo-frame-display.c stb_image.h
	$(CC) $(FRAME_CFLAGS) -o $@ $< $(FRAME_LIBS) $(LDFLAGS)

photo-frame-display-client: photo-frame-display-client.c
	$(CC) $(CFLAGS) -o $@ $< $(CLIENT_LIBS) $(LDFLAGS)

# Build both the display app and the example client
all-with-client: photo-frame-display photo-frame-display-client

clean:
	rm -f photo-frame-display photo-frame-display-client

run: photo-frame-display
	./photo-frame-display
