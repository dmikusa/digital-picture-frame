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

Both services run as the `photo-frame` user. The systemd units use `RuntimeDirectory=photo-frame` with `RuntimeDirectoryMode=0700` and `UMask=0077`, so the `/run/photo-frame/` directory and the Unix socket inside it are only accessible to the owner. The C display app also sets `umask(077)` before `bind()` as defense in depth.

If you change the socket path via config, ensure the directory is restricted to the service user (e.g., `chmod 700 /run/my-socket-dir`). Both services must run as the same user for the manager to connect to the display socket.

### Configuration

Edit `/etc/photo-frame/config.toml` to change manager settings (photos directory, socket path, resolution, etc.).

Edit `/etc/photo-frame/display.env` to change display settings (fade duration, frame skip).
Both files are marked as `conffiles`, so `dpkg` will preserve your changes on package upgrades.

### Building the Package Locally

If you have `cargo-deb` installed:

```bash
make deb
```

This produces `target/debian/photo-frame_*.deb`.
