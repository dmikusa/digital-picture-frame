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

# Use podman by default, override with: make CONTAINER=docker
CONTAINER := $(shell which podman 2>/dev/null || which docker 2>/dev/null)
CONTAINER_IMAGE := photo-frame-c-build

.PHONY: all c rust deb test test-rust test-c build-c-container clean install run-display run-manager setup-debian setup-cargo

all: c rust

c:
	$(MAKE) -C c

rust:
	cargo build --release

deb:
	cargo deb

test: test-rust test-c

test-rust:
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
