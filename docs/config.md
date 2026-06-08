# Config

## Manager config (`config.toml`)

The manager reads `config.toml` at startup. All paths are resolved to absolute paths.

```toml
# Required: directory where photos are stored and imported.
# Must exist and be a directory. The manager will canonicalize the path.
# Example: "/var/lib/photo-frame/photos"
photos_dir = "/var/lib/photo-frame/photos"

# Required: path to the Unix domain socket used to communicate with the C display app.
# The C display app creates the socket. The directory should be owned by the service user
# with mode 0700 so only the service can connect (e.g., /run/photo-frame/ with systemd
# RuntimeDirectoryMode=0700). Both apps must run as the same user.
# Example: "/run/photo-frame/photo-frame.sock"
socket_path = "/run/photo-frame/photo-frame.sock"

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
| `socket_path` | Yes | — | Any valid absolute or relative path |
| `native_resolution` | Yes | — | `"WxH"` where W and H are positive integers (e.g., `"1920x1080"`) |
| `aspect_ratio_mode` | No | `"fit"` | `"fit"` or `"fill"` |
| `batch_delete_size` | No | `20` | Any positive integer (> 0) |
| `log_max_size` | No | `262144` | Any positive integer (bytes) |
| `log_max_files` | No | `2` | Any positive integer (>= 1) |

## Display app environment variables

The C display app reads these environment variables at startup. They are **not** in `config.toml`.

| Variable | Default | Description | Acceptable values |
|----------|---------|-------------|-------------------|
| `PHOTO_FRAME_FADE_DURATION` | `1.5` | Fade duration between photos in seconds. `0` = instant cut (no fade). | Any non-negative float (e.g., `0`, `1.5`, `3`) |
| `PHOTO_FRAME_SKIP_FRAMES` | `0` | Skip frames during fade to reduce CPU load. `0` = render every frame, `1` = render every 2nd frame, etc. | Any non-negative integer |

```bash
# Example: 2-second fade, skip every other frame during fade
PHOTO_FRAME_FADE_DURATION=2.0 PHOTO_FRAME_SKIP_FRAMES=1 ./c/photo-frame-display
```
