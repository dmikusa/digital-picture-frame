# Agent Instructions: photo-frame

## What Matters

A Rust manager + C display app for a Pi Zero W2. The Rust app streams photo paths to the C app over a Unix socket. The C app handles DRM/GBM/EGL rendering.

See `README.md` for build instructions, deployment, and external dependencies.

## Test

```bash
cargo test        # 20 unit tests, all must pass
cargo clippy      # must be clean
```

## Style

- Simple. No async, no tokio.
- Sync I/O + `std::thread::spawn`.
- `notify` crate is the only async-ish dependency (for `inotify`).
- Minimize SD card writes: logs to `/tmp` (tmpfs), photos and index only.

## Design Decisions You Must Not Break

1. **No PING/PONG.** The display app does not respond to `PING`. The Rust client does not send it. Backpressure is via kernel socket buffer only.
2. **No artificial sleeps in the display loop.** The Rust app sends `IMG` as fast as `write_all()` allows. The socket blocks naturally when the C app pauses reading.
3. **Display settings are env vars, not TOML.** `PHOTO_FRAME_FADE_DURATION` and `PHOTO_FRAME_SKIP_FRAMES` are read by `photo_frame.c`. Never add them to the Rust `Config` struct.
4. **Canonicalize paths early.** Both `Config::from_file` and `import_from_directory` call `.canonicalize()`. All downstream file ops rely on absolute paths.
5. **PID lock is stale-aware.** `/tmp/photo-frame.lock` contains a PID. On startup, if `kill(pid, 0)` fails, the lock file is stale — remove it and continue.

## File Map

```
src/
  main.rs      - CLI, signal handling, thread spawn, PID lock
  config.rs    - TOML parsing (manager settings only, no display settings)
  display.rs   - Unix socket client. Blocking write, 30s timeout, no PING
  app.rs       - Display loop: stream CSV, send IMG, watch index
  import.rs    - USB watcher, photo scan, ImageMagick shell-out
  index.rs     - CSV read/write/compaction, dedup hash scanning
  logger.rs    - tmpfs log with rotation
c/
  photo_frame.c - DRM/GBM/EGL display server (env vars for fade/skip)
```

## Common Tasks

### Add a config option

1. Add field to `Config` in `src/config.rs` with `#[serde(default = "...")]`.
2. Add validation in `Config::validate()`.
3. Write a test in `config::tests`.
4. Update `README.md` example config.

### Change display behavior (fade, frame skip)

1. Edit `c/photo_frame.c` — `read_display_config()` reads env vars.
2. Apply in the render/fade loop.
3. Document in `README.md` and `SPEC.md`.
4. **Do not add to Rust `Config`.**

### Change the index format

1. Update `PhotoRecord` in `src/index.rs`.
2. Update `parse_csv_line()`.
3. Remember: additions must be append-only to preserve line offsets.
4. Update tests.

## Testing Notes

- Tests use `tempfile::NamedTempFile` and `tempfile::tempdir()`.
- Logger tests call `logger.log(&record)` directly to avoid the once-per-process global logger limit.
- Socket tests use `std::os::unix::net::UnixListener` for mock display servers.
