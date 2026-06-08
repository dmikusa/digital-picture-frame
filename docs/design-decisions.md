# Design Decisions You Must Not Break

1. **No PING/PONG.** The display app does not respond to `PING`. The Rust client does not send it. Backpressure is via kernel socket buffer only.
2. **No artificial sleeps in the display loop.** The Rust app sends `IMG` as fast as `write_all()` allows. The socket blocks naturally when the C app pauses reading.
3. **Display settings are env vars, not TOML.** `PHOTO_FRAME_FADE_DURATION` and `PHOTO_FRAME_SKIP_FRAMES` are read by `photo-frame-display.c`. Never add them to the Rust `Config` struct.
4. **Canonicalize paths early.** Both `Config::from_file` and `import_from_directory` call `.canonicalize()`. All downstream file ops rely on absolute paths.
5. **PID lock is stale-aware.** `/tmp/photo-frame.lock` contains a PID. On startup, if `kill(pid, 0)` fails, the lock file is stale — remove it and continue.
