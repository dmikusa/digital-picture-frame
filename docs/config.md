# Config

## Manager config (`config.toml`)

```toml
photos_dir = "/mnt/photos"
socket_path = "/tmp/photo-frame.sock"
native_resolution = "1920x1080"
aspect_ratio_mode = "fit"   # or "fill"
batch_delete_size = 20
log_max_size = 262144       # 256 KiB
log_max_files = 2
```

## Display app environment variables

```bash
# Fade duration in seconds. 0 = instant cut.
PHOTO_FRAME_FADE_DURATION=1.5 ./c/photo-frame-display

# Skip frames during fade to reduce CPU. 0 = every frame, 1 = every 2nd, etc.
PHOTO_FRAME_SKIP_FRAMES=1 ./c/photo-frame-display
```
