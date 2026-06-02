# Digital Photo Frame

A lightweight Rust binary that manages a photo collection for a digital photo frame running on a Raspberry Pi Zero W2 with DietPi.

## Features

- **Display Thread**: Streams photos from a CSV index and sends them to a companion display app via a Unix domain socket. Handles backpressure naturally through kernel socket buffers.
- **USB Import**: Automatically detects USB drive mounts, scans for JPEG files, deduplicates them, converts them to a configured native resolution, and copies them into an organized directory structure.
- **Storage Rotation**: When the dedicated photo partition is full, automatically deletes the oldest photos in configurable batches.
- **Circular Index**: The CSV photo index uses logical deletion with an encoded filename (`index-<start>-<count>.csv`). Compaction rewrites the index when ghost entries exceed 50%.
- **Low Resource Usage**: Optimized for the Pi Zero W2 with minimal CPU and memory usage. Release binary is under 1MB.
- **In-Memory Logging**: Logs are written to `/tmp` (tmpfs) with size-based rotation and gzip compression. No SD card wear from logging.

## Architecture

The project is a Rust binary with the following modules:

- `config.rs`: TOML configuration parsing and validation.
- `logger.rs`: Custom `log` crate implementation backed by tmpfs with rotation.
- `index.rs`: CSV index file reader, writer, compaction, deduplication hash scanner.
- `display.rs`: Unix domain socket client for sending `IMG` commands to the display app.
- `import.rs`: USB mount detection, photo scanning, ImageMagick conversion, and CSV appending.
- `app.rs`: Display loop that streams the index, handles wrapping, and watches for index changes.
- `main.rs`: Entry point, signal handling, thread spawning.

## Project Structure

```
photo-frame/
├── c/                      # C display app (DRM/GBM/EGL)
│   ├── Makefile
│   ├── photo_frame.c       # The display server
│   ├── photo_frame_client.c# Example C client
│   ├── photo_frame_client.py # Example Python client
│   └── stb_image.h
├── demos/                  # Standalone demo programs
├── src/                    # Rust manager source
│   ├── main.rs
│   ├── config.rs
│   ├── display.rs          # Socket client (PING/PONG + IMG protocol)
│   ├── import.rs           # USB mount watcher & photo pipeline
│   ├── index.rs            # CSV index read/write/compaction
│   ├── logger.rs           # Tmpfs log with rotation
│   └── app.rs              # Display loop
├── Cargo.toml
├── Makefile                # Top-level: builds both C and Rust
└── README.md
```

## Building

### Top-level (builds everything)

```bash
make              # Build C display app + Rust manager (native)
make c            # Build only C display app
make rust         # Build only Rust manager (native)
make test         # Run Rust tests
make clean        # Clean all build artifacts
```

### Cross-Compile for Raspberry Pi (from macOS or Linux)

The Rust manager can be cross-compiled from your development machine. The C display app must be built directly on the Pi (or in a Linux container) because it needs ARM Linux graphics libraries.

**Option 1: cargo-zigbuild (recommended for macOS)**

```bash
# Install once
cargo install cargo-zigbuild
rustup target add aarch64-unknown-linux-gnu

# Build for Pi Zero 2 W / Pi 3 / Pi 4 / Pi 5 (64-bit)
make pi

# Or build for 32-bit ARM (older Pi models)
make pi-armv7
```

**Option 2: cross (uses Docker)**

```bash
# Install once
cargo install cross

# Build
make pi   # uses cargo automatically if cargo-zigbuild is not found
```

**Copy binary to Pi**

```bash
# Set your Pi's hostname or IP
export PI_HOST=192.168.1.100

# Build and install in one step
make pi-install
```

### C Display App (build on Pi)

```bash
# On the Pi
cd c/
make              # Build photo_frame
make run          # Build and run
make clean
```

Requires: `gcc`, `libdrm-dev`, `libegl1-mesa-dev`, `libgbm-dev`

### Rust Manager (native build)

```bash
cargo build --release
```

The release binary will be at `target/release/photo-frame`.

## Running

```bash
# Start the display app first (on the Pi)
./c/photo_frame

# Then start the manager (on the Pi, or copy cross-compiled binary first)
./photo-frame /path/to/config.toml

# Import photos from a local folder at startup (no USB needed)
./photo-frame --import-dir /path/to/photos /path/to/config.toml
```

### Example Configuration (`config.toml`)

```toml
photos_dir = "/mnt/photos"
socket_path = "/tmp/photo-frame.sock"
native_resolution = "1920x1080"
aspect_ratio_mode = "fit"  # or "fill"
batch_delete_size = 20
log_max_size = 262144      # 256 KiB
log_max_files = 2
```

### Display App Environment Variables

The C display app reads these optional environment variables on startup:

```bash
# Fade duration between images (seconds). 0 = instant cut (no fade).
PHOTO_FRAME_FADE_DURATION=1.5 ./c/photo_frame

# Skip frames during fade to reduce CPU. 0 = render every frame,
# 1 = render every 2nd frame, 2 = render every 3rd frame, etc.
PHOTO_FRAME_SKIP_FRAMES=1 ./c/photo_frame
```

## DietPi Setup

### 1. Prevent RootFS Auto-Expansion

Before first boot, mount the SD card's ext4 partition on another computer and delete the resize service symlink:

```bash
rm <mount>/etc/systemd/system/local-fs.target.wants/dietpi-fs_partition_resize.service
```

This keeps the root partition at its original ~900MB size, leaving the rest of the SD card free for a photos partition.

### 2. Create the Photos Partition

After first boot:

```bash
sudo parted /dev/mmcblk0
# (parted) mkpart primary ext4 <start> 100%
# (parted) quit

sudo mkfs.ext4 /dev/mmcblk0p3
sudo mkdir /mnt/photos
sudo mount /dev/mmcblk0p3 /mnt/photos

# Add to /etc/fstab:
# PARTUUID=<uuid> /mnt/photos ext4 noatime,lazytime 0 2
```

### 3. Install Required Packages

```bash
sudo apt update
sudo apt install -y usbmount imagemagick
```

### 4. USB Auto-Mount

`usbmount` should handle auto-mounting USB drives to `/media/usb0`, `/media/usb1`, etc. You can also enable DietPi's USB auto-mount via `dietpi-drive_manager`.

### 5. Companion Display App

This manager binary works with a separate display application (`photo_frame.c`) that handles the actual DRM/GBM rendering. See `photo_frame.c` in this repository for the display app source.

## Storage Rotation

Photos are stored on their own partition. When the partition fills up (`ENOSPC`), the app automatically deletes the oldest `batch_delete_size` photos (the oldest entries in the CSV index). The index uses logical deletion: entries are skipped but remain in the file until compaction. Compaction happens on startup when ghost entries exceed 50% of the total.

## Graceful Shutdown

The app handles `SIGTERM` and `SIGINT`. On shutdown, it immediately closes the display socket and exits. It does not attempt to finish an in-flight image send, as the display app handles disconnects gracefully.

## License

[Add your license here]
