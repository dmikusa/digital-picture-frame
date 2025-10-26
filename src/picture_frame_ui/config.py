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

    photos_directory: str = "images"
    slideshow_duration: int = 5  # seconds between photo changes
    fade_duration: int = 1000  # milliseconds for crossfade transition
    import_directory: Optional[str] = None  # directory to import new photos from
    full_screen: bool = False  # whether to start in full screen mode
    rendering_type: str = "GPU"  # rendering type: "GPU" or "CPU"

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

        # No config file found, use defaults
        logger.warning("No configuration file found, using defaults")
        logger.info(
            "To create a config file, place 'frame-config.json' in the current directory "
            "or ~/.picture-frame-ui/"
        )
        return cls()

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
                    else:
                        setattr(config, key, value)
                else:
                    logger.warning(f"Unknown config option: {key}")

            logger.info(f"Loaded configuration from: {config_path}")
            logger.debug(f"Config: {config}")

            return config

        except (FileNotFoundError, json.JSONDecodeError, OSError) as e:
            logger.error(f"Failed to load config file {config_path}: {e}")
            logger.info("Using default configuration")
            return cls()

    def get_photos_path(self) -> Path:
        """Get the absolute path to the photos directory"""
        path = Path(self.photos_directory)

        # Convert relative path to absolute if needed
        if not path.is_absolute():
            return Path.cwd() / path
        else:
            return path

    def get_import_path(self) -> Optional[Path]:
        """Get the absolute path to the import directory"""
        if self.import_directory is None:
            return None

        path = Path(self.import_directory)

        # Convert relative path to absolute if needed
        if not path.is_absolute():
            return Path.cwd() / path
        else:
            return path

    def to_dict(self) -> dict:
        """Convert configuration to dictionary for serialization"""
        return {
            "photos_directory": self.photos_directory,
            "slideshow_duration": self.slideshow_duration,
            "fade_duration": self.fade_duration,
            "import_directory": self.import_directory,
            "full_screen": self.full_screen,
            "rendering_type": self.rendering_type,
        }

    def save(self, config_path: Optional[Path] = None) -> None:
        """Save configuration to file"""
        if config_path is None:
            config_path = Path("frame-config.json")

        # Create directory if it doesn't exist
        config_path.parent.mkdir(parents=True, exist_ok=True)

        try:
            with open(config_path, "w") as f:
                json.dump(self.to_dict(), f, indent=2)
            logger.info(f"Configuration saved to: {config_path}")
        except OSError as e:
            logger.error(f"Failed to save config to {config_path}: {e}")
