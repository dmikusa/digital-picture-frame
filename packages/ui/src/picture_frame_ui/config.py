"""
Digital Picture Frame - Configuration Module
Copyright (C) 2025 Daniel Mikusa

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as published
by the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this program.  If not, see <https://www.gnu.org/licenses/>.
"""

import json
import logging
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)


@dataclass
class FrameConfig:
    """Configuration for the Picture Frame application"""

    photos_directory: Path = Path("images")
    slideshow_duration: int = 5  # seconds between photo changes
    fade_duration: int = 1000  # milliseconds for crossfade transition
    import_directory: Optional[Path] = None  # directory to import new photos from
    full_screen: bool = False  # whether to start in full screen mode
    rendering_type: str = "GPU"  # rendering type: "GPU" or "CPU"
    server_host: str = "0.0.0.0"
    server_port: int = 3400
    server_max_file_size: int = 20 * 1024 * 1024  # max upload size in bytes
    screen_width: int = 1920  # screen width for photo resizing
    screen_height: int = 1080  # screen height for photo resizing

    @classmethod
    def load(cls) -> "FrameConfig":
        """Load configuration from file, with fallback locations and defaults"""

        # Try current directory first
        current_dir_config = Path("frame-config.json")
        if current_dir_config.exists():
            logger.debug(
                f"Found config file in current directory: {current_dir_config}"
            )
            return cls._load_from_file(current_dir_config)

        # Try user home directory
        home_dir = os.getenv("HOME")
        if home_dir:
            home_config = Path(home_dir) / ".picture-frame-ui" / "frame-config.json"
            if home_config.exists():
                logger.debug(f"Found config file in home directory: {home_config}")
                return cls._load_from_file(home_config)

        # No config file found, fail
        logger.error("No configuration file found")
        logger.error(
            "Please create 'frame-config.json' in the current directory "
            "or ~/.picture-frame-ui/"
        )
        raise FileNotFoundError(
            "Configuration file 'frame-config.json' not found. "
            "Please create it in the current directory or ~/.picture-frame-ui/"
        )

    @classmethod
    def _load_from_file(cls, config_path: Path) -> "FrameConfig":
        """Load configuration from a specific file"""
        try:
            with open(config_path, "r") as f:
                config_data = json.load(f)

            # Create config with defaults, then update with file data
            config = cls()
            for key, value in config_data.items():
                if hasattr(config, key):
                    if key == "rendering_type" and value not in ["GPU", "CPU"]:
                        logger.warning(
                            f"Invalid rendering_type '{value}', must be 'GPU' or 'CPU'. Using default 'GPU'"
                        )
                        setattr(config, key, "GPU")
                    elif (
                        key in ["photos_directory", "import_directory"]
                        and value is not None
                    ):
                        # Convert string paths to Path objects
                        setattr(config, key, Path(value))
                    else:
                        setattr(config, key, value)
                else:
                    logger.warning(f"Unknown config option: {key}")

            logger.info(f"Loaded configuration from: {config_path}")
            logger.debug(f"Config: {config}")

            # Normalize paths and validate
            config._normalize_paths()
            config._validate_photos_directory()

            return config

        except (FileNotFoundError, json.JSONDecodeError, OSError) as e:
            logger.error(f"Failed to load config file {config_path}: {e}")
            raise RuntimeError(
                f"Failed to load configuration from {config_path}: {e}"
            ) from e

    def get_photos_path(self) -> Path:
        """Get the absolute path to the photos directory"""
        return self.photos_directory

    def _normalize_paths(self) -> None:
        """Convert relative paths to absolute paths"""
        # Normalize photos directory
        photos_path = Path(self.photos_directory)
        if not photos_path.is_absolute():
            photos_path = Path.cwd() / photos_path
        self.photos_directory = photos_path

        # Normalize import directory if it exists
        if self.import_directory is not None:
            import_path = Path(self.import_directory)
            if not import_path.is_absolute():
                import_path = Path.cwd() / import_path
            self.import_directory = import_path

    def _validate_photos_directory(self) -> None:
        """Validate that the photos directory exists and is a directory"""
        path = self.get_photos_path()

        if not path.exists():
            logger.error(f"Photos directory does not exist: {path}")
            logger.info("Please create the directory or update the configuration")
            raise FileNotFoundError(f"Photos directory does not exist: {path}")

        if not path.is_dir():
            logger.error(f"Photos path is not a directory: {path}")
            raise NotADirectoryError(f"Photos path is not a directory: {path}")

    def get_import_path(self) -> Optional[Path]:
        """Get the absolute path to the import directory"""
        return self.import_directory

    def get_screen_dimensions(self) -> tuple[int, int]:
        """Get screen dimensions, detecting them if not configured"""
        if self.screen_width is not None and self.screen_height is not None:
            return self.screen_width, self.screen_height

        # Import here to avoid circular imports
        # also keeps GTK dependency out of config module
        from .ui import get_screen_dimensions

        self.screen_width, self.screen_height = get_screen_dimensions()

        logger.debug(
            f"Using screen dimensions for photo import: {self.screen_width}x{self.screen_height}"
        )
        return self.screen_width, self.screen_height

    def to_dict(self) -> dict:
        """Convert configuration to dictionary for serialization"""
        return {
            "photos_directory": str(self.photos_directory),
            "slideshow_duration": self.slideshow_duration,
            "fade_duration": self.fade_duration,
            "import_directory": (
                str(self.import_directory) if self.import_directory else None
            ),
            "full_screen": self.full_screen,
            "rendering_type": self.rendering_type,
            "server_host": self.server_host,
            "server_port": self.server_port,
            "server_max_file_size": self.server_max_file_size,
            "screen_width": self.screen_width,
            "screen_height": self.screen_height,
        }
