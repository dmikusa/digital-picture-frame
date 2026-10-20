# Agent Instructions: photo-frame

## What Matters

A Rust manager + C display app for a Pi Zero W2. The Rust app streams photo paths to the C app over a Unix socket. The C app handles DRM/GBM/EGL rendering.

## File Map

```
src/
  main.rs          - CLI, signal handling, worker thread spawn, PID lock
  config.rs        - TOML parsing (manager settings only, no display settings)
  display.rs       - Unix socket client. Blocking write, 30s timeout, no PING
  app.rs           - Display loop: stream CSV, send IMG, watch index
  control.rs       - Control socket client (CLR/SHOW/RESUME)
  config_mode.rs   - Config-mode state machine, QR screens, staged config saves
  admin_server.rs  - Config-mode HTTP server (tiny_http_fork) and admin UI
  qr.rs            - QR/status screen rendering and password generation
  index.rs         - CSV read/write/compaction, dedup hash scanning
  logger.rs        - tmpfs log with rotation
  import/
    mod.rs         - Re-exports USB + remote import APIs
    usb.rs         - /media watcher, photo scan, ImageMagick, import lock
    remote.rs      - Sync scheduler, cursor/status files, download pipeline
    dropbox.rs     - Dropbox source
    gdrive.rs      - Google Drive source (API key, OAuth, service account)
    http.rs        - Shared blocking HTTP client, retries, streaming downloads
c/
  photo-frame-display.c - DRM/GBM/EGL display server (env vars for fade/skip)
```

## License

AGPL-3.0-or-later. See `docs/adding-new-files.md` for header templates.

## Reference Docs

- `plans/SPEC.md` — Living specification: what features exist and how they operate.
- `docs/style.md` — Code style and constraints.
- `docs/testing.md` — Test commands and methodology.
- `docs/design-decisions.md` — Non-negotiable architectural rules.
- `docs/common-tasks.md` — Recipes for common changes.
