# Deployment

## DietPi setup

### 1. Don't expand the rootfs

Before first boot, mount the SD card's ext4 partition on another computer and delete the resize symlink:

```bash
rm <mount>/etc/systemd/system/local-fs.target.wants/dietpi-fs_partition_resize.service
```

This keeps root at ~900MB, leaving the rest of the card for a photos partition.

### 2. Create the photos partition

After first boot:

```bash
sudo parted /dev/mmcblk0
# (parted) mkpart primary ext4 <start> 100%
# (parted) quit

sudo mkfs.ext4 /dev/mmcblk0p3
sudo mkdir -p /var/lib/photo-frame/photos
sudo mount /dev/mmcblk0p3 /var/lib/photo-frame/photos

# Add to /etc/fstab:
# PARTUUID=<uuid> /var/lib/photo-frame/photos ext4 noatime,lazytime 0 2
```

### 3. Install packages

```bash
sudo apt update
sudo apt install -y imagemagick
```

### 4. USB auto-mount

The app watches `/media` for new directories, so any auto-mount solution that mounts USB drives there works:

- **Debian/Raspberry Pi OS:** `usbmount` (auto-mounts to `/media/usb0`, `/media/usb1`, etc.)
- **DietPi:** Enable auto-mount in `dietpi-drive_manager`
- **Manual:** Add entries to `/etc/fstab` or use `systemd` mount units

### 5. Display app

The Rust manager needs the C display app (`photo-frame-display.c`) running alongside it. See `c/photo-frame-display.c` in this repo.

## Config mode

The frame shows a setup QR code when it has no photos and no remote sources. The
screen also lists the reachable LAN URL (`http://<ip>:<port>`), the rotating
8-character password, and an optional `photo-frame.local` URL.

1. Connect a phone or laptop to the same network as the frame.
2. Scan the QR code, or open the LAN URL shown on screen.
3. Sign in with the username `photo-frame` and the password shown under the QR.
4. Add, edit, or remove remote sources and press **Finish**.

The QR code itself embeds the LAN IP address and the password, so scanning it
works on networks without mDNS. `photo-frame.local` is only an alternative.

Flow details:

- **USB import**: inserting a drive shows "Importing photos..." and starts the
  import. When it finishes, the setup screen replaces it. Removing the drive
  cancels config mode and discards unsaved changes.
- **First remote sync**: if no photos exist but remote sources are configured,
  the screen shows "First remote sync in progress" instead. It switches back to
  the slideshow as soon as photos appear.
- **Re-entry**: inserting a drive during an existing session stops the old admin
  server first, then imports and starts a new session with a new port/password.
- **Staging**: the source list starts with the sources already in
  `config.toml`. Changes stay in memory until **Finish**. Finishing validates
  every entry (names, required credentials, check interval) and atomically
  rewrites `config.toml`. An empty list is allowed: it removes all remote
  sources. Clearing the list or abandoning the session writes nothing.
- **Status**: `GET /api/status` returns the staged sources plus the contents of
  `<photos_dir>/sync-status.json` when the scheduler has written it. A missing
  file yields an empty `sync` object.

If the display app has not created its control socket yet, or closes it, the QR
screen cannot be shown. That no longer ends config mode: the admin server and its
password stay live, the failure is logged, and the screen is retried. The same
URL, mDNS alternative, username, and password are in
`/tmp/photo-frame-setup.txt` (mode 0600) until config mode exits, so setup can be
completed from a shell or another device. Check `/tmp/photo-frame.log` if the
setup screen never appears.

### Optional: mDNS hostname

The IP-based URL works without any extra packages. To also use
`http://photo-frame.local:<port>`, install and run `avahi-daemon` and give the
frame a `photo-frame` hostname (or an alias):

```bash
sudo apt install -y avahi-daemon
sudo hostnamectl set-hostname photo-frame
# Some networks also need an /etc/hosts entry for the local address:
echo "127.0.1.1 photo-frame.local photo-frame" | sudo tee -a /etc/hosts
```

The `.deb` lists `avahi-daemon` under `Recommends`, not `Depends`, so it is not
required. If the optional URL does not resolve, use the numeric IP address
shown on the setup screen.

## Storage rotation

When the photos partition fills up (`ENOSPC`), the app deletes the oldest `batch_delete_size` photos. The CSV index keeps ghost entries until compaction, which happens on startup if ghosts exceed 50%.

## Shutdown

The app handles `SIGTERM` and `SIGINT`. It closes the socket immediately and exits. It won't finish sending a half-sent image, since the display app handles disconnects fine.

## Packaging & Installation

Pre-built `.deb` packages are available from GitHub Releases for both `amd64` and `arm64`.

### Install

1. Download the `.deb` for your architecture from the latest draft release.
2. Install it:
   ```bash
   sudo dpkg -i photo-frame_*.deb
   ```
3. The package installs and starts two systemd services:
   - `photo-frame-display.service` — the C DRM display app
   - `photo-frame-manager.service` — the Rust manager

### Dedicated User

The package creates a `photo-frame` system user and adds it to the `video` group.
This grants the display app access to `/dev/dri/card0` on standard Debian-based systems.

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

### Security

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

### Configuration

Edit `/etc/photo-frame/config.toml` to change manager settings (photos directory, socket path, resolution, etc.).

Edit `/etc/photo-frame/display.env` to change display settings (fade duration, frame skip).
Both files are marked as `conffiles`, so `dpkg` will preserve your changes on package upgrades.

`/etc/photo-frame` is owned by `photo-frame:photo-frame` with mode `0750`, and
`config.toml` is owned by the service user with mode `0600`. Config mode rewrites
`config.toml` atomically (write a temp file in the same directory, then rename),
so the service user must be able to write both the directory and the file. The
config may contain credentials for remote photo sources, so keep it private. Edit
it as root, for example with `sudoedit /etc/photo-frame/config.toml`.
`display.env` remains root-owned with mode `0644`, and the `photo-frame` user can
still read it through the directory.

### Building the Package Locally

If you have `cargo-deb` installed:

```bash
make deb
```

This produces `target/debian/photo-frame_*.deb`.
