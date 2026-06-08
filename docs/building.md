# Building

## Makefile Options

```bash
make              # C display app + Rust manager (native)
make c            # C app only
make rust         # Rust manager only
make deb          # Build Debian package (requires cargo-deb)
make test         # Run Rust tests
make clean        # Clean everything
make run-display  # Runs the C display app
make run-manager  # Runs the Rust manager app
make setup-debian # Install build/runtime dependencies on Debian
make setup-cargo  # Install required cargo plugins (cargo-deb)
```

## C display app requirements

Needs: `gcc`, `libdrm-dev`, `libegl1-mesa-dev`, `libgbm-dev`

## Rust manager app requirements

Needs: `rustup` & stable Rust toolchain.

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
