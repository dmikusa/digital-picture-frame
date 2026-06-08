# Photo Frame Manager — Implementation Plan

## Phase 1: Project Bootstrap & Core Infrastructure

### Task 1.1: Create Rust Project
- `cargo init --name photo-frame-manager`
- Set up `Cargo.toml` with dependencies:
  - `tokio = { version = "1", features = ["full"] }`
  - `notify = "6"`
  - `csv = "1"`
  - `serde = { version = "1", features = ["derive"] }`
  - `toml = "0.8"`
  - `log = "0.4"`
  - `crc32fast = "1.3"`
  - `signal-hook = "0.3"`
  - `signal-hook-tokio = { version = "0.3", features = ["futures-v0_3"] }`
- Configure `edition = "2021"`, optimize for size in release (`opt-level = "z"`, `lto = true`).

### Task 1.2: Configuration Module
- Define `Config` struct with `serde` deserialization from TOML.
- Validate fields: resolution format (`WxH`), directory paths exist and are accessible.
- Implement `Display` for logging config at startup.
- Write unit tests for config parsing (valid, invalid resolution, missing fields).

### Task 1.3: Custom Logger
- Implement `struct TmpfsLogger` implementing `log::Log`.
- Features:
  - Append to `/tmp/photo-frame.log`.
  - Track file size; rotate when `log_max_size` exceeded.
  - Rename old logs: `.1`, `.2` etc., gzip compress, keep `log_max_files`.
  - Thread-safe via `std::sync::Mutex`.
- Unit tests: rotation logic, size tracking, compression.

---

## Phase 2: Index Management

### Task 2.1: CSV Index Parser
- Implement `IndexReader` that opens the CSV and streams lines.
- On startup: glob `index-*-*.csv`, parse `start_line` and `valid_count` from filename.
- `seek_to(line_offset)` method.
- `next_line()` returns the next valid record (skipping lines before `start_line`).
- Unit tests: parsing filename, seeking, wrapping at EOF, skipping ghosts.

### Task 2.2: CSV Index Writer
- Implement `IndexWriter` that appends new records.
- Generate `DDDDD` sequence number from global line number.
- Atomic append using `std::fs::OpenOptions` with `append = true`.
- Unit tests: appending, file creation, concurrent reads.

### Task 2.3: Compaction
- Implement `compact_index()`.
- Trigger condition: `ghost_count / total_lines > 0.5` (checked on startup).
- Reads old CSV, writes new CSV with only valid lines.
- Atomically renames new file to `index-0-<new_count>.csv`.
- Unit tests: compaction logic, ghost ratio calculation, rename atomicity.

### Task 2.4: Deduplication Hash Scanner
- On startup, scan the entire CSV and populate `HashSet<u64>` of hashes.
- `Arc<Mutex<HashSet<u64>>>` for sharing with import thread.
- Unit tests: hash extraction from CSV, set population.

---

## Phase 3: Display Thread

### Task 3.1: Display Socket Client
- Unix domain socket stream connection to `socket_path`.
- Set `SO_SNDTIMEO` to 30 seconds.
- `send_img(path: &str)` method that writes `IMG <path>\n`.
- Reconnect with 5-second backoff on timeout or disconnect.
- Unit tests: mock Unix socket server, send/receive, timeout simulation.

### Task 3.2: Display Loop
- Combines `IndexReader` + socket client.
- Pick random start line on startup.
- Loop: `next_line()` → `send_img()` → continue.
- On EOF, seek to `start_line` of file (not 0, to respect `start_line` from filename).
- On index file change event (`notify`), reopen file, seek to current offset.
- Integration test: simulate CSV with 3 photos, verify all 3 sent in order, wrapping.

---

## Phase 4: USB Import Thread

### Task 4.1: USB Mount Watcher
- `notify` watcher on `/media` for `Create`/`Remove` events on directories.
- Maintain set of currently mounted USB paths.
- Spawn an async task per new mount.
- Unit tests: mock inotify events, mount set tracking.

### Task 4.2: Photo Scanner
- Given a mount point, recursively find `.jpg`, `.jpeg`, `.JPG`, `.JPEG` files.
- Yield paths one at a time (streaming, not batching).
- Unit tests: temp directory with nested files, filter non-JPEG.

### Task 4.3: Import Pipeline (per file)
- Compute hash: read first 32KB + file size, hash with `crc32fast`.
- Check deduplication set; skip if present.
- Generate destination path: `photos_dir/YYYY/MM/DD/DDDDD_<name>.jpg` using file `mtime`.
- Shell out to ImageMagick for resize:
  - Detect `magick` vs `convert`.
  - Build command based on `aspect_ratio_mode`.
  - Capture stderr on failure; log and skip file.
- On success:
  - Append to CSV.
  - Add hash to deduplication set.
- Unit tests: mock ImageMagick command, verify path generation, hash computation.

### Task 4.4: Storage Rotation
- Before each copy, check available space on `photos_dir` filesystem.
- If `ENOSPC` on write:
  - Read first `batch_delete_size` valid lines from CSV.
  - Delete those files.
  - Update index filename to reflect new `start_line`.
  - Retry the import.
- Unit tests: mock full filesystem, verify deletion and filename update.

---

## Phase 5: Integration & Main

### Task 5.1: Signal Handling
- `signal-hook` + `tokio` to catch `SIGTERM`/`SIGINT`.
- Broadcast shutdown to all threads via `tokio::sync::broadcast`.
- Display thread: close socket on shutdown signal.
- Import thread: abort current import, clean up partial file if any.

### Task 5.2: Main Function
- Parse CLI args (config file path).
- Initialize logger.
- Load config, validate.
- Build deduplication set from CSV.
- Spawn display thread.
- Spawn USB watcher thread.
- Block main on signal, then initiate shutdown.

### Task 5.3: End-to-End Tests
- Create temp directories for `photos_dir`, CSV, config, socket.
- Spawn a mock display app (Unix socket server that reads `IMG` commands).
- Simulate USB mount by creating files in a temp `/media/usb0`.
- Verify:
  - Photos are imported and resized.
  - Display thread sends paths to mock app.
  - Duplicate re-import is skipped.
  - Storage rotation deletes oldest when simulated full.

---

## Phase 6: Documentation & Polish

### Task 6.1: README
- Build instructions (`cargo build --release`).
- DietPi setup steps (partition, packages, config).
- How to run (`./photo-frame-manager /path/to/config.toml`).
- Troubleshooting (socket permissions, USB not mounting).

### Task 6.2: `AGENTS.md` Update
- Document build steps, test commands, project conventions.
- Note the `photo-frame-display.c` companion binary.

### Task 6.3: Final Review
- Run `cargo clippy`, fix warnings.
- Run all tests (`cargo test`).
- Verify release binary size is reasonable for Pi Zero W2 (< 5MB target).

---

## Task Summary Table

| Task | Description | Est. Effort |
|------|-------------|-------------|
| 1.1 | Project setup & dependencies | Small |
| 1.2 | Config module + tests | Small |
| 1.3 | Custom logger + tests | Medium |
| 2.1 | CSV index reader + tests | Medium |
| 2.2 | CSV index writer + tests | Small |
| 2.3 | Compaction logic + tests | Medium |
| 2.4 | Dedup hash scanner + tests | Small |
| 3.1 | Socket client + tests | Medium |
| 3.2 | Display loop + integration test | Medium |
| 4.1 | USB mount watcher + tests | Small |
| 4.2 | Photo scanner + tests | Small |
| 4.3 | Import pipeline + tests | Medium |
| 4.4 | Storage rotation + tests | Medium |
| 5.1 | Signal handling | Small |
| 5.2 | Main function + wiring | Small |
| 5.3 | End-to-end tests | Medium |
| 6.1–6.3 | Documentation & polish | Small |

---

*End of Implementation Plan*
