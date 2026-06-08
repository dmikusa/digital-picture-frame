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
#   make test     - build and run unit tests for display logic
#   make clean    - remove built binaries
#   make run      - build and run photo-frame-display

CC      := gcc
CFLAGS  := -Wall -Wextra -O2 -std=gnu11
LDFLAGS :=

# Display logic (no GL/DRM dependencies)
LOGIC_SRC := display_logic.c
LOGIC_OBJ := display_logic.o

# photo-frame-display requires DRM, EGL, GLES, GBM, and math (stb_image uses pow)
FRAME_LIBS := -lEGL -lGLESv2 -lgbm $(shell pkg-config --libs libdrm) -lm
FRAME_CFLAGS := $(CFLAGS) $(shell pkg-config --cflags libdrm)

# photo-frame-display-client has no special library dependencies
CLIENT_LIBS  :=

.PHONY: all test clean run

all: photo-frame-display

$(LOGIC_OBJ): display_logic.c display_logic.h
	$(CC) $(CFLAGS) -c -o $@ $<

photo-frame-display: photo-frame-display.c $(LOGIC_OBJ) stb_image.h display_logic.h
	$(CC) $(FRAME_CFLAGS) -o $@ $< $(LOGIC_OBJ) $(FRAME_LIBS) $(LDFLAGS)

photo-frame-display-client: photo-frame-display-client.c
	$(CC) $(CFLAGS) -o $@ $< $(CLIENT_LIBS) $(LDFLAGS)

# Build both the display app and the example client
all-with-client: photo-frame-display photo-frame-display-client

# Test binary for display logic
TEST_BIN := test_display_logic

$(TEST_BIN): test_display_logic.c $(LOGIC_OBJ) display_logic.h
	$(CC) $(CFLAGS) -o $@ $< $(LOGIC_OBJ)

test: $(TEST_BIN)
	./$(TEST_BIN)

clean:
	rm -f photo-frame-display photo-frame-display-client $(TEST_BIN) $(LOGIC_OBJ)

run: photo-frame-display
	./photo-frame-display
