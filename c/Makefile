# Makefile for C display app and example clients
#
# Targets:
#   make          - build photo_frame
#   make clean    - remove built binaries
#   make run      - build and run photo_frame

CC      := gcc
CFLAGS  := -Wall -Wextra -O2 -std=gnu11
LDFLAGS :=

# photo_frame requires DRM, EGL, GLES, GBM, and math
FRAME_LIBS := -lEGL -lGLESv2 -lgbm $(shell pkg-config --libs libdrm) -lm
FRAME_CFLAGS := $(CFLAGS) $(shell pkg-config --cflags libdrm)

# photo_frame_client has no special library dependencies
CLIENT_LIBS  :=

.PHONY: all clean run

all: photo_frame

photo_frame: photo_frame.c stb_image.h
	$(CC) $(FRAME_CFLAGS) -o $@ $< $(FRAME_LIBS) $(LDFLAGS)

photo_frame_client: photo_frame_client.c
	$(CC) $(CFLAGS) -o $@ $< $(CLIENT_LIBS) $(LDFLAGS)

# Build both the display app and the example client
all-with-client: photo_frame photo_frame_client

clean:
	rm -f photo_frame photo_frame_client

run: photo_frame
	./photo_frame
