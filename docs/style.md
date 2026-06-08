# Style

- Simple. No async, no tokio.
- Sync I/O + `std::thread::spawn`.
- `notify` crate is the only async-ish dependency (for `inotify`).
- Minimize SD card writes: logs to `/tmp` (tmpfs), photos and index only.
