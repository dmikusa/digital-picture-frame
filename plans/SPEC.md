# Photo Frame Manager — Specification & Design Decisions

## Project Overview

A Rust binary (`photo-frame-manager`) that runs on a Raspberry Pi Zero W2 under DietPi. It manages a photo collection, drives a separate DRM/GBM display application (`photo-frame-display.c`) via a Unix domain socket, and imports photos from USB drives. The design prioritizes low CPU/memory usage, minimal SD card writes, and simplicity.

---

## 1. Core Features

### 1.1 Display Thread
- Opens a CSV index file (list of photo paths) in streaming mode — **never reads the entire file into memory**.
- Starts at a **random line** on startup, streams line-by-line, wraps to beginning at EOF.
- Sends `IMG <path>\n` to the display app via Unix domain socket.
- Handles backpressure naturally: the display app stops reading when its buffers are full, the kernel socket buffer fills, and our `send()` blocks until space frees up.
- Watches the index file for changes (additions). On change, reopens the file and seeks to the previous line offset (since additions are append-only, offsets remain stable).
- If the index is empty at startup, blocks and waits for entries.

### 1.2 USB Import Thread
- Detects USB drive mounts via `inotify` watching `/media` (works with any auto-mount solution).
- Scans mounted drives for image files (JPEG, HEIF/HEIC) recursively.
- For each image:
  - Computes a fast non-cryptographic hash (first 32KB + file size) for duplicate detection.
  - Checks against in-memory deduplication set (built from CSV on startup).
  - Converts to configured native resolution using ImageMagick (shell out).
  - Copies to `photos_dir/YYYY/MM/DD/DDDDD_original_name.jpg`.
  - Appends a CSV record to the index.
- Streams imports one-at-a-time (read one, convert/copy one, repeat). If drive is yanked, stops gracefully. Re-inserting the drive will re-scan; duplicates are skipped.

### 1.3 Storage Rotation
- Photos stored on a **dedicated ext4 partition** on the SD card.
- When partition is full (write returns `ENOSPC`):
  - Delete the oldest `batch_delete_size` photos (oldest = first valid lines in the CSV).
  - Compaction: when "ghost" entries (deleted photos still in CSV) exceed 50% of the file, rewrite the CSV to strip them. This happens on startup to avoid runtime pauses.

### 1.4 Configuration
- TOML config file, path passed as command-line argument.
- Fields:
  - `photos_dir`: path to photo storage
  - `socket_path`: Unix domain socket for display app
  - `native_resolution`: e.g., `"1920x1080"`
  - `aspect_ratio_mode`: `"fit"` (letterbox/pillarbox) or `"fill"` (crop to center). Default: `"fit"`.
  - `batch_delete_size`: number of photos to delete per rotation cycle. Default: 20.
  - `log_max_size`: max log file size in bytes before rotation. Default: 262144 (256KB).
  - `log_max_files`: number of retained old log files. Default: 2.

### 1.5 Logging
- Uses the standard Rust `log` crate facade.
- Custom logger: writes to `/tmp/photo-frame.log` (tmpfs, in-memory, no SD card wear).
- When log file hits `log_max_size`, rotate it (`.1`, `.2`, etc.), compress old ones, delete excess.
- Format: `YYYY-MM-DDTHH:MM:SSZ <level> <message>`.

### 1.6 Graceful Shutdown
- Handles `SIGTERM`/`SIGINT`.
- Immediately closes the display socket and exits. Does **not** attempt to finish an in-flight `send()` to avoid blocking on a full kernel buffer.
- The display app (`photo-frame-display.c`) handles disconnects gracefully — it prints a message and continues its render loop with already-loaded images.

### 1.7 Display App Environment Variables
The C display app (`photo-frame-display.c`) reads these optional environment variables on startup:
- `PHOTO_FRAME_FADE_DURATION`: cross-fade duration in seconds between images. Default: 1.5. Set to 0 for instant cut (no fade).
- `PHOTO_FRAME_SKIP_FRAMES`: skip N frames during each fade to reduce CPU. 0 = render every frame (default), 1 = render every 2nd frame, 2 = render every 3rd frame.

---

## 2. Decision Points & Rationale

### 2.1 Display App Protocol — Blocking Send with Write Timeout

**Decision:** The client sends `IMG` commands continuously without waiting for `READY`. Backpressure is handled by the kernel socket buffer filling and blocking `send()`. A 30-second `SO_SNDTIMEO` is set on the socket; on timeout, treat as dead connection and reconnect with backoff.

**Why:** The display app already implements backpressure by pausing `read()` when its image slots and pending buffer are full. Relying on kernel-level blocking is simpler than coordinating `READY`/`IMG` round-trips. The timeout prevents indefinite hangs if the display app crashes without closing the connection.

### 2.2 Index Format — CSV with Circular Buffer via Filename

**Decision:** Use a simple CSV (not JSONL) where each line is `path,original_name,hash`. The filename encodes metadata: `index-<start_line>-<valid_count>.csv`. "Deleting" old photos means incrementing `start_line` (logical deletion). Compaction rewrites the file without ghost entries when the ghost ratio exceeds 50%, done on startup.

**Why:** CSV is cheaper to parse than JSON. Appending is atomic and cheap. The filename-as-metadata approach avoids a sidecar file and leverages atomic `rename()`. The display thread skips lines before `start_line`. Since we need to scan the entire CSV at startup anyway for deduplication hashes, the "start from previous offset" requirement was relaxed — the thread picks a random line on each startup.

### 2.3 Duplicate Detection — Fast Hash of First 32KB + File Size

**Decision:** On startup, scan the entire CSV into an in-memory `HashSet<u64>`. During USB import, hash the first 32KB of each candidate file plus its total size using a fast non-cryptographic hasher (e.g., `crc32fast` or `twox-hash`). Skip if hash matches.

**Why:** Full-file hashing on a Pi Zero W2 is CPU-intensive. First 32KB is enough to catch identical files while being a single `read()` syscall. File size prevents false positives from tiny collisions. The in-memory set is ~8 bytes per photo, negligible RAM.

### 2.4 USB Mount Detection — `inotify` on `/media`

**Decision:** The Rust app uses `inotify` (via the `notify` crate) to watch `/media` for directory creation/deletion events. Any auto-mount solution that mounts USB drives under `/media` works — `usbmount`, DietPi's `dietpi-drive_manager`, `udisks2`, manual `fstab` entries, etc.

**Why:** `inotify` is kernel-driven with zero CPU overhead — the app blocks until something happens, no polling needed. This keeps the app agnostic to how drives get mounted.

### 2.5 Image Conversion — Shell Out to ImageMagick

**Decision:** For each import, shell out to ImageMagick (`magick` command, fallback to `convert`). Command pattern: `magick input.jpg -resize <W>x<H>^ -gravity center -extent <W>x<H> output.jpg` for fill mode, or just `-resize <W>x<H>` for fit mode.

**Why:** On Debian Trixie, ImageMagick 7 (`magick`) is available. Shelling out avoids pulling a heavy Rust image crate into the binary, keeps memory low, and offloads CPU-intensive resize work to a well-optimized external tool. The Pi Zero W2 is slow; this is acceptable because imports are infrequent (not real-time).

### 2.6 Photo Storage Partition — Manual Post-Flash Setup

**Decision:** Do not use DietPi's builder to create a second partition (it doesn't support it). Instead:
1. Flash standard DietPi image.
2. Before first boot, mount the root partition and delete the auto-resize symlink.
3. Boot — rootfs stays at its original ~900MB size.
4. Manually create a second ext4 partition in the remaining space, mount to `/mnt/photos`, add to `/etc/fstab`.

**Why:** DietPi's `dietpi-build` only creates boot and root partitions. The resize service is enabled via a simple symlink; removing it before first boot is trivial. This is a one-time setup step, far simpler than forking the build scripts.

### 2.7 Storage Rotation — Delete Oldest Batch on `ENOSPC`

**Decision:** Since photos are on their own partition, we write until `ENOSPC`. Then:
1. Read the first `batch_delete_size` valid lines from the CSV.
2. `unlink()` those files.
3. Increment `start_line` in the index filename (logical delete).
4. Retry the write.

**Why:** A dedicated partition means "full" is unambiguous (the partition itself, not competing with OS files). The CSV is ordered by insertion time, so the oldest photos are always at the start (after `start_line`). No need to walk the filesystem or track sizes manually.

### 2.8 Logging — Custom Rotation Logger on Tmpfs

**Decision:** Implement a custom `log::Log` target that writes to `/tmp/photo-frame.log` with size-based rotation and gzip compression of old logs.

**Why:** Standard Rust logging crates don't provide size-based rotation out of the box, and full `tracing` is overkill for this project. `/tmp` on DietPi is already mounted as tmpfs (in-memory). This avoids any SD card writes for logs, satisfying the "avoid unnecessary disk writes" constraint.

### 2.9 Graceful Shutdown — Immediate Socket Close

**Decision:** On `SIGTERM`/`SIGINT`, immediately close the display socket and exit. Do not attempt to flush or complete an in-flight `IMG` send.

**Why:** If the kernel socket buffer is full (backpressure), trying to finish the send would block indefinitely. The display app handles disconnects gracefully (it prints a message and continues). This is the safest and simplest option.

---

## 3. Technical Architecture

### 3.1 Threads
- **Main thread:** Spawns workers, handles signals, owns logger.
- **Display thread:** Streams CSV, sends `IMG` to display app socket, watches for index changes.
- **USB watcher thread:** Blocks on `inotify` for `/media` changes, spawns import tasks.
- **Import task (per mount):** Scans drive, converts/copies photos one-at-a-time, updates CSV.

### 3.2 Concurrency
- The CSV file is append-only. Multiple threads may append (import) and one thread reads (display). Appends are naturally atomic at the line level if using `writeln!` with line buffering.
- The display thread uses `notify` to watch the CSV file. On `modify` events, it reopens the file and seeks to the current offset.
- Deduplication uses an `Arc<Mutex<HashSet<u64>>>` shared between the startup scanner and import tasks.

### 3.3 Error Handling Philosophy
- **Fatal:** Config parse failure, socket path missing, photos directory not accessible on startup.
- **Retry with backoff:** Display app socket disconnected.
- **Skip and continue:** Individual file read/convert/copy errors during import, duplicate detected.
- **Wait:** Empty index on startup, display app not yet running.

### 3.4 File Formats
- **Config:** TOML.
- **Index:** CSV, one line per photo. Fields: `path,original_name,hash`.
- **Index metadata:** Encoded in filename: `index-<start_line>-<valid_count>.csv`.
- **Photos:** `photos_dir/YYYY/MM/DD/DDDDD_<original_name>.jpg` where `DDDDD` is a zero-padded sequence number from the CSV line index.

### 3.5 External Dependencies
- **System packages:** `imagemagick` (or `imagemagick-7` on Trixie). USB auto-mounting is required but can be provided by `usbmount`, `dietpi-drive_manager`, `udisks2`, or manual `fstab` entries.
- **Rust crates:** `tokio` (async runtime, file watcher, socket), `notify` (inotify wrapper), `csv` (parsing), `serde` + `toml` (config), `log` (logging facade), `crc32fast` or `twox-hash` (fast hashing), `signal-hook` (SIGTERM/SIGINT handling).

---

## 4. DietPi Setup Notes

### 4.1 Preventing RootFS Auto-Expansion
1. Flash DietPi image to SD card.
2. Mount the ext4 root partition (e.g., `/dev/sdX2` on Linux).
3. Run: `rm <mount>/etc/systemd/system/local-fs.target.wants/dietpi-fs_partition_resize.service`
4. Unmount and boot.

### 4.2 Creating the Photos Partition
After first boot, as root:
```bash
parted /dev/mmcblk0
# (parted) mkpart primary ext4 <start> 100%
# (parted) quit
mkfs.ext4 /dev/mmcblk0p3
mkdir /mnt/photos
mount /dev/mmcblk0p3 /mnt/photos
# Add to /etc/fstab:
# PARTUUID=<uuid> /mnt/photos ext4 noatime,lazytime 0 2
```

### 4.3 Required Packages
```bash
apt update
apt install -y imagemagick
```

### 4.4 USB Auto-Mount
Any solution that mounts USB drives under `/media` works:
- **Debian/Raspberry Pi OS:** `usbmount` (auto-mounts to `/media/usb0`, `/media/usb1`, etc.)
- **DietPi:** Enable auto-mount in `dietpi-drive_manager`
- **Manual:** Add entries to `/etc/fstab` or use `systemd` mount units

---

*End of Specification Document*
