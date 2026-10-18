# Config

## Manager config (`config.toml`)

The manager reads `config.toml` at startup. All paths are resolved to absolute paths.

```toml
# Required: directory where photos are stored and imported.
# Must exist and be a directory. The manager will canonicalize the path.
# Example: "/var/lib/photo-frame/photos"
photos_dir = "/var/lib/photo-frame/photos"

# Required: Unix socket the display app creates to receive image paths (IMG).
# The manager connects here to send photos to the screen. The display app reads
# the same path from DISPLAY_SOCKET_PATH in display.env, so the two must match.
# The directory should be owned by the service user with mode 0700 so only the
# service can connect (the display service sets RuntimeDirectoryMode=0700).
# Example: "/run/photo-frame/photo-frame.sock"
display_socket_path = "/run/photo-frame/photo-frame.sock"

# Optional: Unix socket the display app creates for control commands
# (CLR/SHOW/RESUME). The manager uses it in config mode to show the setup QR
# screen. The display app reads the same path from CONTROL_SOCKET_PATH in
# display.env, so the two must match.
# Default: "/run/photo-frame/control.sock"
control_socket_path = "/run/photo-frame/control.sock"

# Required: native resolution of the display in "WxH" format.
# Used by the import thread to resize photos via ImageMagick.
# Both width and height must be positive integers.
# Example: "1920x1080"
native_resolution = "1920x1080"

# Optional: how to handle photos with a different aspect ratio than the display.
#   - "fit" (default): resize to fit within the resolution, preserving aspect ratio. May leave black bars.
#   - "fill": resize to fill the resolution, cropping to center. No black bars.
# Acceptable values: "fit", "fill"
aspect_ratio_mode = "fit"

# Optional: number of oldest photos to delete when the disk is full during import.
# Must be greater than 0.
# Default: 20
batch_delete_size = 20

# Optional: maximum size in bytes of a single log file before rotation.
# Logs are written to tmpfs (RAM) to avoid SD card wear.
# Default: 262144 (256 KiB)
log_max_size = 262144

# Optional: number of rotated log files to retain.
# When the current log exceeds log_max_size, it is rotated and older files are purged.
# Default: 2
log_max_files = 2
```

### Config field reference

| Field | Required | Default | Acceptable values |
|-------|----------|---------|-------------------|
| `photos_dir` | Yes | — | Any valid absolute or relative path to an existing directory |
| `display_socket_path` | Yes | — | Any valid absolute or relative path. Legacy alias: `socket_path` |
| `control_socket_path` | No | `"/run/photo-frame/control.sock"` | Any valid absolute or relative path |
| `native_resolution` | Yes | — | `"WxH"` where W and H are positive integers (e.g., `"1920x1080"`) |
| `aspect_ratio_mode` | No | `"fit"` | `"fit"` or `"fill"` |
| `batch_delete_size` | No | `20` | Any positive integer (> 0) |
| `log_max_size` | No | `262144` | Any positive integer (bytes) |
| `log_max_files` | No | `2` | Any positive integer (>= 1) |

## Display app environment variables

The C display app reads these environment variables at startup. The two socket
paths are shared with the manager and must match the corresponding
`config.toml` values; the fade/skip settings are display-only.

| Variable | Default | Description | Acceptable values |
|----------|---------|-------------|-------------------|
| `PHOTO_FRAME_FADE_DURATION` | `1.5` | Fade duration between photos in seconds. `0` = instant cut (no fade). | Any non-negative float (e.g., `0`, `1.5`, `3`) |
| `PHOTO_FRAME_SKIP_FRAMES` | `0` | Skip frames during fade to reduce CPU load. `0` = render every frame, `1` = render every 2nd frame, etc. | Any non-negative integer |
| `DISPLAY_SOCKET_PATH` | `/run/photo-frame/photo-frame.sock` | Socket to listen on for image paths (`IMG`). Must match `display_socket_path`. | Any valid absolute path |
| `CONTROL_SOCKET_PATH` | `/run/photo-frame/control.sock` | Socket to listen on for control commands (`CLR`/`SHOW`/`RESUME`). Must match `control_socket_path`. | Any valid absolute path |

```bash
# Example: 2-second fade, skip every other frame during fade
PHOTO_FRAME_FADE_DURATION=2.0 PHOTO_FRAME_SKIP_FRAMES=1 ./c/photo-frame-display
```
