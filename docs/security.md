# Security

## User Configuration

The install deb creates a `photo-frame` system user and adds it to the `video` 
group. This grants the display app access to `/dev/dri/card0` on standard Debian-based
systems.

If your system has custom udev rules that prevent the `video` group from accessing the
DRM device, you can fall back to running the display app as `root`:

```bash
sudo systemctl edit photo-frame-display.service
```

Add:
```ini
[Service]
User=root
Group=root
```

Then:
```bash
sudo systemctl daemon-reload
sudo systemctl restart photo-frame-display
```

## Service Security

Both services run as the `photo-frame` user. The **display** service owns
`/run/photo-frame/` via `RuntimeDirectory=photo-frame` with
`RuntimeDirectoryMode=0700`, so the directory and the sockets inside it are only
accessible to the owner. The C display app also sets `umask(077)` before `bind()`
as defense in depth.

The manager service deliberately does **not** declare `RuntimeDirectory`. systemd
removes a `RuntimeDirectory` when its owning unit stops, and because the two
services share this path, restarting the manager would otherwise delete the
display app's socket files while it keeps running. Only the display service owns
the directory.

If you change either socket path, update both `display_socket_path` /
`control_socket_path` in `config.toml` and `DISPLAY_SOCKET_PATH` /
`CONTROL_SOCKET_PATH` in `display.env` so the two apps agree. Ensure the
directory is restricted to the service user (e.g., `chmod 700 /run/my-socket-dir`).
Both services must run as the same user for the manager to connect.

## Configuration Security

Configuration lives under `/etc/photo-frame` and is owned by `photo-frame:photo-frame`
with mode `0750`. The `config.toml` is owned by the service user with mode `0600`.

Config mode rewrites `config.toml` atomically (write a temp file in the same
directory, then rename), so the service user must be able to write both the directory
and the file. 

The config may contain credentials for remote photo sources, so keep it
private. Edit it as root, for example with `sudoedit /etc/photo-frame/config.toml`.
`display.env` remains root-owned with mode `0644`, and the `photo-frame` user can
still read it through the directory.
