# Running

For development, use the `Makefile` options listed in [building.md](building.md).

Here's what the make commands are doing:

```bash
# Start the display app first (on the Pi)
./c/photo-frame-display

# Then the manager
./photo-frame-manager /path/to/config.toml

# Or import from a local folder at startup (no USB needed)
./photo-frame-manager --import-dir /path/to/photos /path/to/config.toml
```
