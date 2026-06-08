# Agent Instructions: photo-frame

## What Matters

A Rust manager + C display app for a Pi Zero W2. The Rust app streams photo paths to the C app over a Unix socket. The C app handles DRM/GBM/EGL rendering.

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
  photo-frame-display.c - DRM/GBM/EGL display server (env vars for fade/skip)
```

## Reference Docs

- `plans/SPEC.md` — Living specification: what features exist and how they operate.
- `docs/style.md` — Code style and constraints.
- `docs/testing.md` — Test commands and methodology.
- `docs/design-decisions.md` — Non-negotiable architectural rules.
- `docs/common-tasks.md` — Recipes for common changes.
