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
#   make              - build both C display app and Rust manager (native)
#   make c            - build only the C display app
#   make rust         - build only the Rust manager (native)
#   make deb          - build Debian package (requires cargo-deb)
#   make test         - run Rust tests
#   make clean        - clean both C and Rust build artifacts
#   make install      - install binaries to /usr/local/bin (requires sudo)
#   make run-display  - build and run the C display app
#   make run-manager  - build and run the Rust manager app
#   make setup-debian - install build/runtime dependencies on Debian
#   make setup-cargo  - install required cargo plugins (cargo-deb)

.PHONY: all c rust deb test clean install run-display run-manager setup-debian setup-cargo

all: c rust

c:
	$(MAKE) -C c

rust:
	cargo build --release

deb:
	cargo deb

test:
	cargo test

clean:
	$(MAKE) -C c clean
	cargo clean

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
