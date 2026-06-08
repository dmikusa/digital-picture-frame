# Common Tasks

### Add a config option

1. Add field to `Config` in `src/config.rs` with `#[serde(default = "...")]`.
2. Add validation in `Config::validate()`.
3. Write a test in `config::tests`.
4. Update `README.md` example config.

### Change display behavior (fade, frame skip)

1. Edit `c/photo-frame-display.c` — `read_display_config()` reads env vars.
2. Apply in the render/fade loop.
3. Document in `README.md` and `SPEC.md`.
4. **Do not add to Rust `Config`.**

### Change the index format

1. Update `PhotoRecord` in `src/index.rs`.
2. Update `parse_csv_line()`.
3. Remember: additions must be append-only to preserve line offsets.
4. Update tests.
