# Top-level Makefile for the photo-frame project
#
# This project contains two components:
#   - c/          : C display app (DRM/GBM/EGL photo frame)
#   - src/        : Rust manager app (USB import, socket client, index management)
#
# Targets:
#   make              - build both C display app and Rust manager (native)
#   make c            - build only the C display app
#   make rust         - build only the Rust manager (native)
#   make pi           - cross-compile Rust manager for Raspberry Pi (aarch64)
#   make pi-armv7     - cross-compile Rust manager for Raspberry Pi (armv7)
#   make pi-install   - copy cross-compiled binary to Pi (set PI_HOST)
#   make test         - run Rust tests
#   make clean        - clean both C and Rust build artifacts
#   make install      - install binaries to /usr/local/bin (requires sudo)
#   make run-display  - build and run the C display app
#
# Cross-compilation requires a linker. On macOS, the easiest options are:
#   1. cargo-zigbuild:  cargo install cargo-zigbuild  (uses Zig as linker)
#   2. cross:         cargo install cross             (uses Docker)
#
# Example with cargo-zigbuild:
#   rustup target add aarch64-unknown-linux-gnu
#   make pi

PI_HOST ?= dietpi
PI_TARGET_AARCH64 ?= aarch64-unknown-linux-gnu
PI_TARGET_ARMV7 ?= armv7-unknown-linux-gnueabihf

CARGO := cargo
# Use cargo-zigbuild if available, otherwise fall back to cargo
ifneq (, $(shell which cargo-zigbuild 2>/dev/null))
  CARGO := cargo-zigbuild
endif

.PHONY: all c rust pi pi-armv7 pi-install test clean install run-display

all: c rust

c:
	$(MAKE) -C c

rust:
	cargo build --release

pi:
	@echo "Cross-compiling Rust manager for Pi ($(PI_TARGET_AARCH64)) using $(CARGO)..."
	$(CARGO) build --release --target $(PI_TARGET_AARCH64)
	@echo "Binary: target/$(PI_TARGET_AARCH64)/release/photo-frame"

pi-armv7:
	@echo "Cross-compiling Rust manager for Pi ($(PI_TARGET_ARMV7)) using $(CARGO)..."
	$(CARGO) build --release --target $(PI_TARGET_ARMV7)
	@echo "Binary: target/$(PI_TARGET_ARMV7)/release/photo-frame"

pi-install: pi
	@echo "Copying binary to $(PI_HOST)..."
	scp target/$(PI_TARGET_AARCH64)/release/photo-frame $(PI_HOST):/tmp/photo-frame
	ssh $(PI_HOST) "sudo install -Dm755 /tmp/photo-frame /usr/local/bin/photo-frame && rm /tmp/photo-frame"

test:
	cargo test

clean:
	$(MAKE) -C c clean
	cargo clean

install: all
	install -Dm755 c/photo_frame /usr/local/bin/photo_frame
	install -Dm755 target/release/photo-frame /usr/local/bin/photo-frame-manager

run-display: c
	cd c && ./photo_frame
