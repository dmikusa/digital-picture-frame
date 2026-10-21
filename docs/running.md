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
both services so logging never writes to the SD card; `journalctl` stays empty.
The manager logs to `/tmp/photo-frame.log`, the display app to
`/tmp/photo-frame-display.log` (see below). Both live on tmpfs and are lost on
reboot.

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

## Display app log

The display app writes diagnostics to `/tmp/photo-frame-display.log` rather
than stdout/stderr. The service intentionally keeps
`StandardOutput=null`/`StandardError=null` so logging never writes to the SD
card. `/tmp` is a tmpfs, so the log lives in memory and is lost on reboot.

```bash
tail -f /tmp/photo-frame-display.log
```

Lines are UTC and timestamped:

```text
2026-10-21T12:34:56Z INFO Control command: SHOW /tmp/photo-frame-qr.jpg
2026-10-21T12:34:56Z INFO SHOW displayed: /tmp/photo-frame-qr.jpg (fb=42)
```

The file is created with mode 0600 by the `photo-frame` service user and is
flushed after every line; nothing is fsynced. It is capped at 256 KiB: when the
next line would exceed that, the current file is renamed to
`/tmp/photo-frame-display.log.1` (one backup, overwritten on the next rotation)
and a fresh log starts. If the file cannot be opened, the app falls back to
stderr, which systemd discards.

The log covers startup (socket paths, DRM/EGL setup, fade/skip config), socket
lifecycle and pause/resume, every control command (`CLR`/`SHOW`/`RESUME` and
unknown commands), SHOW steps and failures, page-flip errors, fatal `CHECK`/EGL
failures, and shutdown. Normal `IMG` loads are `DEBUG`; load failures and drops
are `WARN`.
