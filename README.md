# Digital Picture Frame

A fullscreen photo slideshow application built with Rust and egui, designed to make your own digital picture frames and displays.

## Features

- Fullscreen photo display with smooth crossfade transitions
- Automatic cycling through photos in a directory
- Configurable display duration (default: 10 seconds)
- Smooth 1-second fade transitions between photos
- Support for common image formats (JPEG, PNG, GIF, etc.)
- Always-on-top window mode

## Usage

```bash
# Build and run
cargo run --release

# Run with debug logging
RUST_LOG=debug cargo run --release
```

The application will display photos from a configured directory in fullscreen mode, automatically cycling through them with smooth transitions.

## Building

```bash
# Development build
cargo build

# Release build (recommended)
cargo build --release
```

## Testing

```bash
cargo test
```

## Requirements

- Rust 2024 edition
- Graphics support for egui/eframe

## License

This project is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0). This means:

- You can use, modify, and distribute this software freely
- If you distribute modified versions, you must provide the source code
- If you run this software on a server and provide access over a network, you must provide the source code to users

For the full license text, see the [GNU AGPL-3.0 license](https://www.gnu.org/licenses/agpl-3.0.html).
