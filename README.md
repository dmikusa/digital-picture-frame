# Digital Picture Frame

A fullscreen photo slideshow application built with Python and GTK4, designed to make your own digital picture frames and displays.

## Features

- Fullscreen photo display with smooth crossfade transitions
- Automatic cycling through photos in a directory
- Configurable display duration (default: 5 seconds)
- Smooth 1-second crossfade transitions between photos
- Support for common image formats (JPEG, PNG, GIF, BMP, TIFF, WebP)
- Always reads fresh directory listing (no cached file list)
- Configurable via JSON configuration file

## Requirements

- Python 3.10 or later
- GTK4 development libraries
- uv package manager (recommended)

### System Dependencies

#### Ubuntu/Debian
```bash
sudo apt update
sudo apt install python3-gi python3-gi-cairo gir1.2-gtk-4.0 libgtk-4-dev
```

#### macOS (with Homebrew)
```bash
brew install gtk4 pygobject3
```

#### Fedora/CentOS/RHEL
```bash
sudo dnf install python3-gobject gtk4-devel
```

## Installation

### Using uv (recommended)

1. Install uv if you haven't already:
```bash
# macOS/Linux
curl -LsSf https://astral.sh/uv/install.sh | sh

# Or with pip
pip install uv
```

2. Clone and install the project:
```bash
git clone https://github.com/dmikusa/digital-picture-frame.git
cd digital-picture-frame
uv sync
```

### Using pip

```bash
git clone https://github.com/dmikusa/digital-picture-frame.git
cd digital-picture-frame
pip install -e .
```

## Usage

### Quick Start

```bash
# Create a photos directory and add some images
mkdir images
# Add your photos to the images directory

# Run the application
uv run python main.py

# Or with debug logging
DEBUG=1 uv run python main.py
```

### Using pip installation

```bash
# After pip install (run from project directory)
python main.py

# With debug logging
DEBUG=1 python main.py
```

### Configuration

Create a `frame-config.json` file in either:
- Current directory
- `~/.picture-frame-ui/frame-config.json`

Example configuration:
```json
{
  "photos_directory": "/path/to/your/photos",
  "slideshow_duration": 5,
  "fade_duration": 1000
}
```

Configuration options:
- `photos_directory`: Path to directory containing photos (default: "images")
- `slideshow_duration`: Seconds between photo changes (default: 5)
- `fade_duration`: Crossfade transition duration in milliseconds (default: 1000)

### Keyboard Shortcuts

- **Cmd+Q** (macOS) or **Ctrl+Q** (Linux/Windows): Quit application

## Development

### Setting up development environment

```bash
# Clone the repository
git clone https://github.com/dmikusa/digital-picture-frame.git
cd digital-picture-frame

# Install development dependencies
uv sync --dev

# Run tests
uv run python -m pytest

# Run with debug logging
DEBUG=1 uv run python main.py
```

### Project Structure

```
src/picture_frame_ui/           # Python library
├── __init__.py                 # Package initialization
├── config.py                   # Configuration management
├── photos.py                   # Photo loading and directory scanning
└── ui.py                      # GTK4 user interface
main.py                        # Application entry point
test_images/                   # Sample images for testing
pyproject.toml                 # Python project configuration
frame-config.json              # Current config file
frame-config.json.example      # Example config file
justfile                       # Development commands
README.md                      # This documentation
LICENSE                        # AGPL-3.0 license
```

## Migration from Rust Version

This Python version is functionally equivalent to the original Rust version with the following improvements:

- More Pythonic configuration handling
- Better error handling and logging
- Simplified dependency management with uv
- Same GTK4 UI with crossfade transitions
- Same slideshow functionality

The memory monitoring features from the Rust version have been removed as they're not necessary in Python.

## License

This project is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0). This means:

- You can use, modify, and distribute this software freely
- If you distribute modified versions, you must provide the source code
- If you run this software on a server and provide access over a network, you must provide the source code to users

For the full license text, see the [GNU AGPL-3.0 license](https://www.gnu.org/licenses/agpl-3.0.html).
