# Deployment

This document walks through the setup of a device. This process can and will vary depending on the device you're using. These instructions are meant to be somewhat generic though, so I hope they work on a wide variety of devices.

## Highlights

1. Download DietPi for your device & flash it to an SD card.
2. Setup the filesystem *optional*
3. Boot it up.
4. Install dependencies and the Photo Frame software
5. Setup WiFi *optional*
6. Load photos from USB or configure other photo locations

## Installation Details

### Download & Flash

You can download from the [DietPi Website](https://dietpi.com/#downloadinfo). Find the ISO for your device.

Use something like [Fedora Media Writer](https://fedoraproject.org/workstation/download/) or `dd` to flash the image to your SDcard.

DO NOT BOOT THE DEVICE YET!

### Before Booting

This step is for optional partitioning of the SDcard. You do not have to do this, but I recommend it. The Photo Manager is able to automatically clean up old files when you run out of space, if you follow this method. If you skip this, then the auto clean up feature does not work because by the time a single partition fills up, you will run into other problems.

It's important to do this before booting, because on first boot the DietPi scripts that run will expand your rootfs to consume the full SDcard.

To do this, mount the SD card's `ext4` partition on another computer and delete the resize symlink. This is easiest on a Linux system.

```bash
rm <mount>/etc/systemd/system/local-fs.target.wants/dietpi-fs_partition_resize.service
```

This keeps root at ~900MB and we'll expand that to 1600MB later, leaving the rest of the card for a photos partition.

Please note that doing this will cause an error to show up on initial login. DietPi sees that the resize script didn't run and will open an error dialog. You can ignore this.

Put your SDcard into the device & boot it now.

### Initial Setup

These are first-time only steps you need to run to setup the device.

1. Login as `root`. The default login is root/dietpi. Dietpi will display that in case it changes in the future.
2. As the warning message says, first thing you need to do is to run `tune2fs -j $G_ROOTFS_DEV`. This is something that the resize script would do, but we've opted not to do.
3. Run `dietpi-config` to configure the device.
  1. Change the password. Security Options -> User passwords
  2. Change the hostname. Network Options -> Misc
  2. Setup WiFi. Network Options -> Adapters -> wlan0 (or your device) -> Scan
4. Logout
5. Login as the `dietpi` user. It will run a one-time startup script. Let it do that. This includes updating `apt` and the software on the device, so it might take a little time.

If you removed the resize script, you have one more step which is to resize the root partition and create the partition for your photos.

```bash
sudo parted /dev/mmcblk0
# (parted) resizepart 2 1600MB
# (parted) mkpart primary ext4 1600MB 100%
# (parted) quit

sudo resize2fs /dev/sda1
sudo mkfs.ext4 /dev/mmcblk0p3
sudo mkdir -p /var/lib/photo-frame/photos
sudo mount /dev/mmcblk0p3 /var/lib/photo-frame/photos
```

Lastly, run `sudo blkid` to get the block id for the new partition. Run `nano /etc/fstab` and add this line.

```
UUID=<uuid> /var/lib/photo-frame/photos ext4 noatime,lazytime 0 2
```

### Enable USB auto-mount

The Photo Frame Manager watches `/media` for new directories. It does this to auto import files from the attached USB drive.

Any auto-mount solution that mounts USB drives there works:

- **DietPi:** Enable auto-mount in `dietpi-drive_manager`
- **Debian/Raspberry Pi OS:** `usbmount` (auto-mounts to `/media/usb0`, `/media/usb1`, etc.)
- **Manual:** Add entries to `/etc/fstab` or use `systemd` mount units

### Install Dependencies & Software

Fetch the latest version of the [Photo Frame software](https://github.com/dmikusa/digital-picture-frame/releases) from our Github Releases page.

Before installing, verify the `.deb` signature and checksum. Set these to the
release and architecture you are installing:

```bash
TAG=v0.2.0
VERSION=0.2.0-ffd3bcf
ARCH=arm64
BASE="https://github.com/dmikusa/digital-picture-frame/releases/download/${TAG}"
DEB="photo-frame_${VERSION}_${ARCH}.deb"

curl -fLO "${BASE}/${DEB}"
curl -fLO "${BASE}/${DEB}.sha256"
curl -fLO "${BASE}/${DEB}.sha256.sig"
curl -fsSLo 2026-10.pub \
  https://raw.githubusercontent.com/dmikusa/digital-picture-frame/main/packaging/update-keys/2026-10.pub
```

Verify the signature over the checksum, then the checksum over the package:

```bash
printf 'release %s\n' "$(cat 2026-10.pub)" > allowed_signers
ssh-keygen -Y verify \
  -f allowed_signers \
  -I release \
  -n photo-frame-update \
  -s "${DEB}.sha256.sig" \
  < "${DEB}.sha256"
sha256sum -c "${DEB}.sha256"
```

`ssh-keygen` should print `Good "photo-frame-update" signature`, and
`sha256sum` should report `OK`. Then install:

```bash
sudo apt update
sudo apt install -y imagemagick avahi-daemon
sudo apt install "./${DEB}"
```

This installs imagemagick which the tool uses for some photo conversion tasks & the Photo Frame software itself. The software installs the Photo Frame Manager and Photo Frame Display services.

### Load Some Photos!

When you start the software for the first time, there are no photos loaded. You have two options to load photos.

1. Plug in USB drive with photos on it. The Photo Manager will automatically load them to the device and begin displaying them.
2. Load photos from the Internet
  1. You must have configured WiFi for this option to work.
  2. When the device starts up, it will present instructions on the screen to access the Photo Manager configuration UI, which is a web site running on the device.
  3. Type in the URL from the screen or load the QR code on your phone.
  4. Use the UI to configure one of the options for loading photos like Google Drive or DropBox.

### Service Status & Restarts

The package installs and starts these systemd services:

- `photo-frame-display.service` — the C DRM display app
- `photo-frame-manager.service` — the Rust manager
- `photo-frame-update-guard.service` — oneshot boot guard for app updates.

The `photo-frame-update-guard` service is a no-op unless an update was activated and never confirmed, in which case it confirms the new release or reverts to the previous one.

### Updates

The `.deb` you originally installed is just the initial install path. It seeds `releases/<package-version>-seed` at sequence 0. After that, application updates use signed release bundles and the `photo-frame-update` tool, rather than `dpkg`. The tool verifies the signature and checksums, unpacks into
`/opt/photo-frame/releases/<version>`, flips the `current` symlink, and rolls back if the new version fails its 30-second health check. A later `.deb` upgrade stages its own release but does not move an update-managed `current` backwards.

The `photo-frame-update-guard.service` service runs after the app services on every boot and confirms or reverts a release that was activated but never confirmed. OS packages, the kernel, and firmware are handled separately with `apt`. You may do these at your convenience.

See [updates.md](updates.md) for the bundle format, commands, signing keys, the manual signing workflow, and the OS update policy. `config.toml` and `display.env` are never touched by an update.

### Configuration

There are two configuration files, which you may edit directly:

1. `/etc/photo-frame/config.toml` settings for the Photo Manager (photos directory, socket path, resolution, etc.).
2. `/etc/photo-frame/display.env` settins for the Display (fade duration, frame skip).

Both files are marked as `conffiles`, so `dpkg` will preserve your changes on package upgrades.
