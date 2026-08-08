# Photo Frame Manager — DRM/GBM/EGL digital photo frame.
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

# Top-level Makefile for the photo-frame project
#
# This project contains two components:
#   - c/          : C display app (DRM/GBM/EGL photo frame)
#   - src/        : Rust manager app (USB import, socket client, index management)
#
# Targets:
#   make                   - build both C display app and Rust manager (native)
#   make c                 - build only the C display app
#   make rust              - build only the Rust manager (native)
#   make deb               - build Debian package (requires cargo-deb)
#   make test              - run all tests (Rust + C in container)
#   make test-rust         - run Rust tests only
#   make test-c            - run C build + lint in container (Podman/Docker)
#   make build-c-container - build the container image for C testing
#   make clean             - clean both C and Rust build artifacts
#   make install           - install binaries to /usr/local/bin (requires sudo)
#   make run-display       - build and run the C display app
#   make run-manager       - build and run the Rust manager app
#   make setup-debian      - install build/runtime dependencies on Debian
#   make setup-cargo       - install required cargo plugins (cargo-deb)

FONT_DIR := fonts
FONT_FILE := $(FONT_DIR)/DejaVuSans.ttf
FONT_URL := https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-sans-ttf-2.37.zip
FONT_SHA256 := 5c6e497a2f36552cb5ffb112c413a6af39c0f3c47653662b90b4fa6499822fd7
FONT_ZIP := $(FONT_DIR)/dejavu-sans.zip

# Use podman by default, override with: make CONTAINER=docker
CONTAINER := $(shell which podman 2>/dev/null || which docker 2>/dev/null)
CONTAINER_IMAGE := photo-frame-c-build

.PHONY: all c rust deb test test-rust test-c build-c-container clean install run-display run-manager setup-debian setup-cargo font

all: font c rust

font: $(FONT_FILE)

$(FONT_FILE): $(FONT_ZIP)
	@mkdir -p $(FONT_DIR)
	@rm -f $@
	unzip -p $< "dejavu-sans-ttf-2.37/ttf/DejaVuSans.ttf" > $@.tmp && mv $@.tmp $@
	@head -c4 $@ | od -An -tx1 | grep -q '00 01 00 00' || { echo "ERROR: Extracted font has invalid TTF magic bytes"; rm -f $@; exit 1; }

$(FONT_ZIP):
	@mkdir -p $(FONT_DIR)
	curl -fsSL -o $@ $(FONT_URL)
	@echo "$(FONT_SHA256)  $@" | shasum -a 256 -c - > /dev/null 2>&1 || { echo "ERROR: Font download hash mismatch. Expected $(FONT_SHA256)"; exit 1; }

c:
	$(MAKE) -C c

rust: font
	cargo build --release

deb: font c
	cargo deb

test: test-rust test-c

test-rust: font
	cargo test

test-c: build-c-container
	@echo "Running C tests + build + lint in container ($(CONTAINER))..."
	$(CONTAINER) run --rm -v $(PWD)/c:/src:Z $(CONTAINER_IMAGE) \
		bash -c "cd /src && make clean && make test && make && cppcheck --enable=all --error-exitcode=1 --suppress=*:stb_image.h --suppress=ctuArrayIndex --suppress=missingIncludeSystem --suppress=toomanyconfigs ."

build-c-container:
	$(CONTAINER) build -t $(CONTAINER_IMAGE) -f c/Containerfile c

clean:
	$(MAKE) -C c clean
	cargo clean
	-$(CONTAINER) rmi $(CONTAINER_IMAGE) 2>/dev/null || true

install: all
	install -Dm755 c/photo-frame-display /usr/local/bin/photo-frame-display
	install -Dm755 target/release/photo-frame-manager /usr/local/bin/photo-frame-manager

run-manager: rust
	./target/release/photo-frame-manager --import-dir "$(IMPORT_DIR)" config.toml

run-display: c
	cd c && ./photo-frame-display

setup-debian:
	apt-get update
	apt-get install -y --no-install-recommends \
		build-essential \
		ca-certificates \
		curl \
		git \
		imagemagick \
		libdrm-dev \
		libegl1-mesa-dev \
		libgbm-dev \
		pkg-config

setup-cargo:
	cargo install cargo-deb
