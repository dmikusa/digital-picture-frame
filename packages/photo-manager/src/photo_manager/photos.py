"""
Digital Picture Frame - Photo Loading Module
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

import logging
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Iterator, Optional

logger = logging.getLogger(__name__)


class PhotoLoader(ABC):
    """Abstract base class for photo loaders"""

    @abstractmethod
    def load_next_photo(self) -> str:
        """Load the next photo and return its file URL"""
        pass


class FilePhotoLoader(PhotoLoader):
    """Photo loader that reads from a filesystem directory"""

    # Common image file extensions
    IMAGE_EXTENSIONS = {
        ".jpg",
        ".jpeg",
        ".png",
        ".gif",
        ".bmp",
        ".tiff",
        ".tif",
        ".webp",
    }

    def __init__(self, base_directory: str):
        self.base_directory = Path(base_directory)
        self._current_iterator: Optional[Iterator[Path]] = None
        logger.info(f"Created FilePhotoLoader for directory: {self.base_directory}")

    def _get_image_files(self) -> Iterator[Path]:
        """Get an iterator of image files in the directory"""
        if not self.base_directory.exists():
            raise FileNotFoundError(
                f"Photos directory does not exist: {self.base_directory}"
            )

        if not self.base_directory.is_dir():
            raise NotADirectoryError(
                f"Photos path is not a directory: {self.base_directory}"
            )

        # Get all files in directory and filter for image extensions
        image_files = []
        for file_path in self.base_directory.iterdir():
            if (
                file_path.is_file()
                and file_path.suffix.lower() in self.IMAGE_EXTENSIONS
            ):
                image_files.append(file_path.resolve())

        if not image_files:
            raise FileNotFoundError(
                f"No image files found in directory: {self.base_directory}"
            )

        # Sort files to ensure consistent ordering across platforms
        image_files.sort(key=lambda p: p.name.lower())

        logger.debug(f"Found {len(image_files)} image files in {self.base_directory}")
        return iter(image_files)

    def load_next_photo(self) -> str:
        """Load the next photo and return its file URL"""
        if self._current_iterator is None:
            logger.info(f"Reading photos from directory: {self.base_directory}")
            self._current_iterator = self._get_image_files()

        try:
            # Get next file from iterator
            next_file = next(self._current_iterator)
            file_url = Path(next_file).as_uri()
            logger.debug(f"Loading image: {file_url}")
            return file_url

        except StopIteration:
            # End of iterator, restart from beginning
            logger.debug("Reached end of photo list, restarting")
            self._current_iterator = None
            return self.load_next_photo()  # Recursive call to restart

    def refresh_directory(self) -> None:
        """Force refresh of the directory listing on next load_next_photo call"""
        self._current_iterator = None
        logger.debug("Directory listing will be refreshed on next photo load")


def create_photo_loader(photos_directory: str) -> PhotoLoader:
    """Factory function to create appropriate photo loader"""
    return FilePhotoLoader(photos_directory)
