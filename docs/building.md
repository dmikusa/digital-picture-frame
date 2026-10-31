# Building

## Makefile Options

```bash
make              # C display app + Rust manager (native)
make c            # C app only
make rust         # Rust manager only
make deb          # Build Debian package (requires cargo-deb)
make update-bundle VERSION=0.2.0 SEQUENCE=7  # Build an unsigned app-update bundle
make test         # Rust tests + C build/lint in a container
make test-rust    # Rust tests only
make test-c       # C build + lint in a container
make clean        # Clean everything
make run-display  # Runs the C display app
make run-manager  # Runs the Rust manager app
make setup-debian # Install build/runtime dependencies on Debian
make setup-cargo  # Install required cargo plugins (cargo-deb)
```

## C display app requirements

Needs: `gcc`, `libdrm-dev`, `libegl1-mesa-dev`, `libgbm-dev`

## Rust manager app requirements

Needs: `rustup` & stable Rust toolchain. Also needs `libssl-dev` and `pkg-config` to
build the `native-tls` backend used for HTTP requests, plus `unzip` for font extraction.

Before building, download the embedded font:

```bash
make font
```

This downloads DejaVu Sans TTF from GitHub releases and verifies its SHA256 hash. The font is embedded in the Rust binary via `include_bytes!()` at compile time. Only needed once (or when running `make clean`).

## App update bundles

`make update-bundle VERSION=0.2.0 SEQUENCE=7` builds an unsigned app-update
bundle at `dist/update/photo-frame-0.2.0-arm64.tar.gz` plus a `sha256sum`-format
`.sha256` sidecar (`ARCH=` and `UPDATE_OUT=` override the defaults). The target
depends on the normal Rust and C builds and calls
`scripts/build-update-bundle.sh`.

Bundles are unsigned by design: CI never holds the update key. Sign the sidecar
with `scripts/sign-checksum.sh` and the Ed25519 private key kept offline. A
device only accepts a bundle signed by a key pinned in
`src/update/keys.rs` (`TRUSTED_KEYS`, empty in this repo until the owner adds
theirs). See `docs/updates.md`.

## Debian VM with GPU acceleration (UTM/QEMU)

For testing/development, I work on a Debian VM. For smooth fades, enable VirGL in your VM:

1. I run on MacOS and use UTM. In UTM, edit the VM settings, go to Display. From the drop down, pick an option that includes GPU. There were a few on my MacBook Pro, I went with `virtio-gpu-gl-pci (GPU Supported)`.
2. In the VM:

```bash
sudo apt update
sudo apt install -y gcc make libdrm-dev libegl1-mesa-dev libgbm-dev mesa-utils
```

3. Check acceleration:

```bash
eglinfo | grep -i "renderer"
# You want "virgl (ANGLE ...)" for host GPU acceleration
# "llvmpipe (...)" means CPU rendering; fades will be jerky & consume lots of CPU
```
