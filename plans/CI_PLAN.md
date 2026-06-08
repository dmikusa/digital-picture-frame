# Packaging & CI Plan for photo-frame

## Goal

Build `.deb` packages for `amd64` and `arm64` on every push to `main`, attach them to a
draft GitHub release, and make the application installable on Debian-based systems with
systemd.

## Build Environment

We run the entire build inside a `debian:11-slim` Docker container on GitHub Actions.
This pins the build environment to glibc 2.31, the oldest glibc in our target matrix,
ensuring forward compatibility with:

- Debian 11+ (bullseye)
- Debian 12 (bookworm)
- Ubuntu 22.04+ (Jammy)
- Raspberry Pi OS (bookworm-based)
- DietPi (bookworm-based)

| Runner | Container | Target Arch |
|--------|-----------|-------------|
| `ubuntu-22.04` | `debian:11-slim` | amd64 |
| `ubuntu-22.04-arm64` | `debian:11-slim` | arm64 |

Both runners do a **native** build (no cross-compilation), avoiding linker and toolchain
complexity.

## Versioning Strategy

- The source of truth is `version` in `Cargo.toml` (currently `0.1.0`).
- You manually bump this when starting a new release cycle.
- CI appends the 7-character git commit hash: `0.1.0-abc1234`.
- The draft release tag is `v0.1.0-abc1234`.
- When you click **Publish** in the GitHub UI, the tag `v0.1.0-abc1234` is created
  automatically, pointing to that exact commit.
- CI uses `cargo deb --deb-version 0.1.0-abc1234` — no `Cargo.toml` mutation needed.
- Old drafts for the same base version are deleted before uploading the new one.
  Only one draft per version cycle survives.

## Workflows

### 1. `ci.yml`

**Triggers:** `pull_request`, `push` to `main`

**Jobs:**
- `cargo test`
- `cargo clippy`
- `make test` (which runs `cargo test`)

Runs on `ubuntu-latest` (amd64 only — sufficient for Rust tests).

### 2. `build.yml`

**Triggers:** `push` to `main` only

**Matrix:**
- Job A: `ubuntu-22.04` → amd64 `.deb`
- Job B: `ubuntu-22.04-arm64` → arm64 `.deb`

**Steps per job:**
1. Install system deps: `build-essential`, `pkg-config`, `libdrm-dev`,
   `libegl1-mesa-dev`, `libgbm-dev`, `curl`, `git`
2. Install Rust via rustup (stable toolchain)
3. Build C display app: `make c`
4. Build Rust manager: `cargo build --release --locked`
5. Install `cargo-deb`
6. Build `.deb`: `cargo deb --deb-version $FULL_VERSION`
7. Upload `.deb` as artifact

**Release job (depends on both matrix jobs):**
1. Download both `.deb` artifacts
2. Compute base version from `Cargo.toml`
3. Delete existing draft releases with tags matching `v${BASE_VERSION}-*`
4. Create new draft release: `gh release create v${FULL_VERSION} --draft ...`
5. Upload both `.deb` files to the draft

**Concurrency:** `group: release`, `cancel-in-progress: true` — prevents race conditions
when multiple commits land on `main` in quick succession.

## Packaging

### Tool: `cargo-deb`

We use `cargo-deb` because the Rust manager is the primary Cargo project. It generates
a standard Debian package with minimal boilerplate.

**`[package.metadata.deb]` in `Cargo.toml`:**
- `name`: `photo-frame-manager`
- `maintainer`: `Daniel Mikusa <dan@mikusa.com>`
- `depends`: `$auto, imagemagick, libdrm2, libegl1, libgbm1`
- `recommends`: `usbmount` (optional — any auto-mount solution that mounts under `/media` works)
- `assets`: Rust binary, C binary, systemd units, config files, env file
- `maintainer-scripts`: `packaging/deb-scripts/`

### Systemd Units

Two separate services:

**`photo-frame-display.service`** — C display app
- `Type=simple`
- `ExecStart=/usr/bin/photo-frame-display`
- `EnvironmentFile=/etc/photo-frame/display.env`
- `User=photo-frame`, `Group=video`
- `Restart=on-failure`

**`photo-frame-manager.service`** — Rust manager
- `Type=simple`
- `ExecStart=/usr/bin/photo-frame-manager /etc/photo-frame/config.toml`
- `User=photo-frame`
- `Restart=on-failure`
- `After=photo-frame-display.service`
- `Wants=photo-frame-display.service`

### Dedicated User

The `postinst` maintainer script creates a system user:

```bash
id -u photo-frame &>/dev/null || useradd -r -s /bin/false -G video photo-frame
```

The `video` group grants access to `/dev/dri/card0` on standard Debian-based systems.
If your system has custom udev rules, you can fall back to `User=root` in the display
service (documented in README).

The `postinst` also:
- Creates `/var/lib/photo-frame/photos` and `chown`s to `photo-frame:photo-frame`
- Runs `systemctl daemon-reload`
- Enables both services
- Starts both services (on fresh install)

### Default Config Files

**`/etc/photo-frame/config.toml`** (conffile):
```toml
# Required: directory where photos are stored and imported. Must exist.
photos_dir = "/var/lib/photo-frame/photos"

# Required: path to the Unix domain socket for the C display app.
socket_path = "/run/photo-frame/photo-frame.sock"

# Required: display resolution in "WxH" format. Used for import resizing.
native_resolution = "1920x1080"

# Optional: aspect ratio handling. "fit" (default) = letterbox, "fill" = crop to center.
aspect_ratio_mode = "fit"

# Optional: photos to delete per rotation cycle when disk is full. Default: 20.
batch_delete_size = 20

# Optional: max log file size in bytes before rotation. Default: 262144 (256 KiB).
log_max_size = 262144

# Optional: number of rotated log files to retain. Default: 2.
log_max_files = 2
```

**`/etc/photo-frame/display.env`** (conffile):
```bash
# Fade duration between photos in seconds. 0 = instant cut. Default: 1.5.
PHOTO_FRAME_FADE_DURATION=1.5

# Skip frames during fade to reduce CPU. 0 = every frame, 1 = every 2nd, etc. Default: 0.
PHOTO_FRAME_SKIP_FRAMES=0
```

Both are automatically marked as `conffiles` by `cargo-deb`, so `dpkg` will preserve
user modifications on package upgrades.

## Security

### External Actions Used (3, all official GitHub)

| Action | Owner | Purpose |
|--------|-------|---------|
| `actions/checkout@v4` | GitHub | Clone repo |
| `actions/upload-artifact@v4` | GitHub | Upload build artifacts |
| `actions/download-artifact@v4` | GitHub | Download artifacts in release job |

### Built-in Tools Used (no action needed)

- `gh` CLI (GitHub official, pre-installed on runners) — creates/updates/deletes releases
- `rustup` (installed from rust-lang in CI)
- `cargo-deb` (installed from crates.io in CI)

### Workflow Permissions

```yaml
permissions:
  contents: write   # For creating/updating/deleting releases
  actions: read     # For downloading artifacts
```

The `GITHUB_TOKEN` is auto-injected by GitHub, scoped to this repo only, and expires
when the job finishes.

## Files to Create

```
.github/
  workflows/
    ci.yml
    build.yml
packaging/
  photo-frame-display.service
  photo-frame-manager.service
  config.toml
  display.env
  deb-scripts/
    postinst
    prerm
```

## Files to Modify

- `Cargo.toml` — add `[package.metadata.deb]` section
- `Makefile` — add `deb:` target
- `README.md` — add "Packaging & Installation" section

## Makefile

```makefile
deb:
	cargo deb
```

This is a convenience target for local builds. CI calls `cargo deb --deb-version ...`
directly.

## README Additions

New section covering:
1. Download `.deb` from GitHub Releases draft
2. `sudo dpkg -i photo-frame_*.deb`
3. The `photo-frame` user is auto-created with `video` group membership
4. Display app DRM access and the `User=root` fallback
5. Where config files live (`/etc/photo-frame/`)
6. How to configure fade duration and frame skip via `/etc/photo-frame/display.env`

## Design Decisions Preserved

This packaging does **not** violate any AGENTS.md rules:
- Display settings remain env vars (`display.env`), never added to Rust `Config`.
- No PING/PONG protocol changes.
- Socket backpressure remains kernel-buffer-only.
- Paths are canonicalized by the app as before.
- PID lock behavior is unchanged.
