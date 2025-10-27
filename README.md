# Digital Picture Frame

A fullscreen photo slideshow application built with Python and GTK4, designed to make your own digital picture frames and displays.

## Features

- Fullscreen photo display with transitions as it rotates through photos in a directory
- Configurable display duration (default: 5 seconds) & crossfade duration
- Support for common image formats (JPEG, PNG, GIF, BMP, TIFF, WebP)

## Requirements

- Python 3.10 or later
- GTK4 development libraries
- uv package manager (for development)

### System Dependencies

This should run on most systems with the following dependencies:

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

Better options coming, but for now...

### Using uv (recommended)

1. Install uv if you haven't already
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

### Create a Configuration File

Create `frame-config.json` file in either:

- Current directory
- `~/.picture-frame-ui/frame-config.json`

You can copy `frame-config.json.example` to get started, or copy what's below:

```json
{
  "photos_directory": "images",
  "slideshow_duration": 5,
  "fade_duration": 1000,
  "import_directory": "import",
  "full_screen": false,
  "rendering_type": "GPU"
}
```

Configuration options:
- `photos_directory`: Path to directory containing photos that will be displayed. This directory is controlled by the application, and is basically where you want it to cache the photos that it is displaying.
- `import_directory`: Path to your photos. These are not changed or modified. Photos from this directory are resized and copied into `photos_directory`.
- `slideshow_duration`: Seconds between photo changes (default: 5)
- `fade_duration`: Crossfade transition duration in milliseconds (default: 1000)
- `full_screen`: Launch the app in full screen mode.
- `rendering_type`: Either `GPU` or `CPU`, the default is `GPU`. Most users should stick with `GPU`, but if the app fails to start on your system, then try `CPU`.

### Run the App

There are a few ways you can run this. They all work, and are in no particular order. Use whatever is more convenient for you.

#### With `uv`

```bash
uv run python main.py

# Or with debug logging
DEBUG=1 uv run python main.py
```

#### Using pip installation

```bash
# After pip install (run from project directory)
python main.py

# With debug logging
DEBUG=1 python main.py
```

#### Custom PYTHONPATH

```bash
PYTHONPATH=./src python main.py
```

## Development

### Setting up development environment

```bash
# Clone the repository
git clone https://github.com/dmikusa/digital-picture-frame.git
cd digital-picture-frame

# Install development dependencies
uv sync --dev

# Run tests
uv run pytest

# Run with debug logging
DEBUG=1 uv run python main.py
```

### Testing

To run tests:

```bash
just test-all

# Run specific test
just test src/picture_frame_ui/photos_test.py
```

or to watch for test changes and auto-run:

```bash
just watch-all

# Run specific test
just watch src/picture_frame_ui/photos_test.py
```

## License

This project is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0). This means:

- You can use, modify, and distribute this software freely
- If you distribute modified versions, you must provide the source code
- If you run this software on a server and provide access over a network, you must provide the source code to users

For the full license text, see the [GNU AGPL-3.0 license](https://www.gnu.org/licenses/agpl-3.0.html).
