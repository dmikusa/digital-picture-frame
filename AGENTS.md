# Agent Instructions: photo-frame

## Project Overview

A Rust binary that manages photos for a digital photo frame on a Raspberry Pi Zero W2 running DietPi. It drives a separate DRM/GBM display app (`photo_frame.c`) via a Unix domain socket and imports photos from USB drives.

## Build

### Quick Build (top-level Makefile)

```bash
make              # Builds C display app + Rust manager (native)
make c            # C display app only (Pi/Linux only)
make rust         # Rust manager only (native)
make pi           # Cross-compile Rust manager for Pi (aarch64)
make pi-armv7     # Cross-compile Rust manager for Pi (32-bit)
make pi-install   # Build + copy binary to Pi (set PI_HOST env var)
make test         # Run Rust tests
make clean        # Clean everything
```

### Cross-Compilation for Pi

The Rust manager can be cross-compiled from macOS or Linux. The Makefile auto-detects `cargo-zigbuild` (recommended on macOS) and falls back to plain `cargo`.

```bash
# Install once
cargo install cargo-zigbuild
rustup target add aarch64-unknown-linux-gnu

# Build
make pi

# Copy to Pi
export PI_HOST=192.168.1.100
make pi-install
```

### Manual Build

**C display app** (build on Pi):
```bash
cd c/
make
```

**Rust manager** (native):
```bash
cargo build --release
```

Binary outputs:
- `c/photo_frame` — C display server
- `target/release/photo-frame` — Rust manager (~950KB)
- `target/aarch64-unknown-linux-gnu/release/photo-frame` — Cross-compiled for Pi

## Test

```bash
cargo test
```

All 21 unit tests should pass. Tests cover config parsing, CSV index read/write/compaction, deduplication hash scanning, socket client (PING/PONG protocol), USB photo scanning, logger rotation, and storage deletion.

## Code Style

- Keep it simple. Avoid unnecessary abstractions.
- Optimize for low CPU/memory (Pi Zero W2 target).
- Prefer sync I/O with threads over async where possible, except for file watching which uses `notify`.
- Minimize SD card writes: logs go to `/tmp` (tmpfs), photos and index only.
- Use `std::sync` primitives for inter-thread communication.

## Key Design Decisions

1. **CSV over JSONL**: Cheaper to parse, append-only, simple.
2. **Filename as metadata**: `index-<start>-<count>.csv` encodes logical deletion state. No sidecar file needed.
3. **Shell out to ImageMagick**: Avoids heavy Rust image crates; `magick` (IM7) or `convert` (IM6).
4. **Blocking socket send with timeout**: Kernel handles backpressure. 30s `SO_SNDTIMEO` prevents hangs.
5. **Immediate socket close on shutdown**: Safest option; display app handles disconnects.
6. **Fast hash dedup**: `crc32fast` on first 32KB + file size. In-memory `HashSet<u64>`.
7. **Dedicated photo partition**: Manual post-flash setup on DietPi. Rotate oldest batch on `ENOSPC`.

## File Structure

```
c/
  Makefile              - C-specific build rules
  photo_frame.c         - DRM/GBM/EGL display server
  photo_frame_client.c  - Example C socket client
  photo_frame_client.py - Example Python socket client
  stb_image.h           - stb_image single-header library
demos/
  fade_display.c        - Fade demo
  image_display.c       - Image display demo
  main.c                - Entry demo
src/
  main.rs      - Entry point, signal handling, thread spawning
  config.rs    - TOML config parsing
  logger.rs    - Custom log::Log backed by tmpfs with rotation
  index.rs     - CSV index reader, writer, compaction, dedup
  display.rs   - Unix socket client (PING/PONG + IMG protocol)
  import.rs    - USB mount watch, photo scan, convert, append
  app.rs       - Display loop: stream index, send to socket, watch for changes
```

## External Dependencies

- **System**: `usbmount`, `imagemagick`
- **Rust crates**: `notify`, `serde`, `toml`, `log`, `crc32fast`, `signal-hook`, `chrono`, `flate2`, `libc`

## Common Tasks

### Add a new config option

1. Add field to `Config` struct in `src/config.rs` with `#[serde(default = "...")]` if needed.
2. Add validation in `Config::validate()`.
3. Update `Display` impl to include it.
4. Write a test in `config::tests`.
5. Update `README.md` example config.

### Change index file format

1. Update `PhotoRecord` struct in `src/index.rs`.
2. Update `parse_csv_line()` to match new column count/order.
3. Update tests.
4. Remember: additions must be append-only to preserve line offsets.

### Add new import source (beyond USB)

1. Create a new watcher function in `src/import.rs` (e.g., `watch_network_source`).
2. Spawn it from `main.rs` alongside `watch_usb_mounts`.
3. Reuse `import_single_photo()` for the actual import pipeline.

## Testing Notes

- Tests use `tempfile::NamedTempFile` and `tempfile::tempdir()` for isolation.
- Logger tests avoid setting the global logger (once-per-process limit) by calling `logger.log(&record)` directly.
- Socket tests use `std::os::unix::net::UnixListener` for mock display servers.
- Compaction and deletion tests verify filesystem state after operations.

## Deployment Notes

1. Flash DietPi image.
2. Before first boot: remove `dietpi-fs_partition_resize.service` symlink from root partition.
3. Boot, create second ext4 partition, mount at `/mnt/photos`, add to `/etc/fstab`.
4. Install packages: `usbmount`, `imagemagick`.
5. Build C display app on the Pi: `cd c/ && make`
6. Cross-compile Rust manager from dev machine: `make pi`
7. Copy to Pi: `make pi-install` (or `scp` manually)
8. Run the display app: `./photo_frame`
9. Run the manager: `./photo-frame /path/to/config.toml`
