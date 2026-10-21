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

## Logs

All application logs are written to `/tmp/photo-frame.log` by the built-in
tmpfs logger. This is an in-memory filesystem — no SD card wear from logging.

```bash
tail -f /tmp/photo-frame.log
```

Rotated logs are gzip-compressed at `/tmp/photo-frame.log.1.gz`,
`/tmp/photo-frame.log.2.gz`, etc.

**Note:** systemd stdout/stderr is suppressed (`StandardOutput=null`) for
both services. `journalctl -u photo-frame-manager` will be empty. All
intentional log output goes to the tmpfs log file above.

## Config mode fallback

If the display app cannot show the setup QR screen, the manager keeps the admin
server running. The URL, mDNS alternative, username (`photo-frame`), and the
current rotating password are written to `/tmp/photo-frame-setup.txt` (mode
0600) for as long as config mode is active.

```bash
cat /tmp/photo-frame-setup.txt
```

The file is removed when config mode exits: after **Finish**, on USB removal, on
the 10-minute idle timeout, and on shutdown. See
[config.md](config.md#setup-fallback-file).
