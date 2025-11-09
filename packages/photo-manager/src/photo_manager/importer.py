"""
Digital Picture Frame - Photo Import Module
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

import hashlib
import logging
from pathlib import Path
from typing import Tuple
from PIL import Image
from picture_frame_ui.config import FrameConfig

logger = logging.getLogger(__name__)


class PhotoImporter:
    """Handles importing and processing photos from an import directory"""

    # Common image file extensions supported by Pillow
    SUPPORTED_EXTENSIONS = {
        ".jpg",
        ".jpeg",
        ".png",
        ".gif",
        ".bmp",
        ".tiff",
        ".tif",
        ".webp",
    }

    def __init__(
        self,
        config: FrameConfig,
    ):
        """
        Initialize the photo importer

        Args:
            config: Optional FrameConfig object to get dimensions from
            max_width: Maximum width for resized images (defaults to 1920)
            max_height: Maximum height for resized images (defaults to 1080)
        """
        self._config = config

    def calculate_sha1(self, file_path: Path) -> str:
        """
        Calculate SHA1 hash of a file

        Args:
            file_path: Path to the file

        Returns:
            SHA1 hash as hexadecimal string
        """
        logger.debug(f"Calculating SHA1 for: {file_path}")
        sha1_hash = hashlib.sha1()

        with open(file_path, "rb") as f:
            # Read file in chunks to handle large files efficiently
            for chunk in iter(lambda: f.read(8192), b""):
                sha1_hash.update(chunk)

        hash_value = sha1_hash.hexdigest()
        logger.debug(f"SHA1 hash: {hash_value}")
        return hash_value

    def get_image_dimensions(self, file_path: Path) -> Tuple[int, int]:
        """
        Get dimensions of an image file

        Args:
            file_path: Path to the image file

        Returns:
            Tuple of (width, height)

        Raises:
            PIL.UnidentifiedImageError: If file is not a valid image
        """
        logger.debug(f"Getting dimensions for: {file_path}")
        with Image.open(file_path) as img:
            width, height = img.size
            logger.debug(f"Image dimensions: {width}x{height}")
            return width, height

    def calculate_resize_dimensions(
        self, original_width: int, original_height: int
    ) -> Tuple[int, int]:
        """
        Calculate new dimensions that fit within MAX_WIDTH x MAX_HEIGHT while preserving aspect ratio

        Args:
            original_width: Original image width
            original_height: Original image height

        Returns:
            Tuple of (new_width, new_height)
        """
        # If image is already within bounds, return original dimensions
        if (
            original_width <= self._config.screen_width
            and original_height <= self._config.screen_height
        ):
            logger.debug(
                f"Image {original_width}x{original_height} is within bounds, no resize needed"
            )
            return original_width, original_height

        # Calculate scaling factor - use the more restrictive dimension
        width_scale = self._config.screen_width / original_width
        height_scale = self._config.screen_height / original_height
        scale_factor = min(width_scale, height_scale)

        new_width = int(original_width * scale_factor)
        new_height = int(original_height * scale_factor)

        logger.debug(
            f"Calculated resize: {original_width}x{original_height} -> {new_width}x{new_height} (scale: {scale_factor:.3f})"
        )
        return new_width, new_height

    def resize_image(
        self,
        source_path: Path,
        target_path: Path,
        target_width: int,
        target_height: int,
    ) -> None:
        """
        Resize an image and save it to the target path

        Args:
            source_path: Path to the source image
            target_path: Path where the resized image will be saved
            target_width: Target width in pixels
            target_height: Target height in pixels
        """
        logger.debug(
            f"Resizing image: {source_path} -> {target_path} ({target_width}x{target_height})"
        )

        with Image.open(source_path) as img:
            # Convert to RGB if necessary (handles RGBA, grayscale, etc.)
            if img.mode != "RGB":
                logger.debug(f"Converting image from {img.mode} to RGB")
                img = img.convert("RGB")

            # Resize using high-quality resampling
            resized_img = img.resize(
                (target_width, target_height), Image.Resampling.LANCZOS
            )

            # Ensure target directory exists
            target_path.parent.mkdir(parents=True, exist_ok=True)

            # Save with high quality
            resized_img.save(target_path, "JPEG", quality=95, optimize=True)

        logger.info(f"Image resized and saved: {target_path}")

    def copy_image(self, source_path: Path, target_path: Path) -> None:
        """
        Copy an image without resizing (for images that don't need scaling)

        Args:
            source_path: Path to the source image
            target_path: Path where the image will be copied
        """
        logger.debug(f"Copying image: {source_path} -> {target_path}")

        # Ensure target directory exists
        target_path.parent.mkdir(parents=True, exist_ok=True)

        # For images that don't need resizing, we still process them through PIL
        # to ensure consistency and handle any format conversions if needed
        with Image.open(source_path) as img:
            # Convert to RGB if necessary
            if img.mode != "RGB":
                logger.debug(f"Converting image from {img.mode} to RGB")
                img = img.convert("RGB")
                # Save as JPEG for consistency
                img.save(target_path, "JPEG", quality=95, optimize=True)
            else:
                # If already RGB, we can save in original format or convert to JPEG
                # For consistency, let's always save as JPEG
                img.save(target_path, "JPEG", quality=95, optimize=True)

        logger.info(f"Image copied: {target_path}")

    def generate_target_filename(self, original_path: Path, sha1_hash: str) -> str:
        """
        Generate the target filename with SHA1 hash

        Args:
            original_path: Path to the original file
            sha1_hash: SHA1 hash of the file

        Returns:
            Target filename in format: <name>##<hash>.<ext>
        """
        stem = original_path.stem  # filename without extension
        # Always use .jpg extension since we're converting to JPEG
        target_filename = f"{stem}##{sha1_hash}.jpg"
        logger.debug(f"Generated target filename: {target_filename}")
        return target_filename

    def photo_exists_in_directory(self, sha1_hash: str) -> bool:
        """
        Check if a photo with the given SHA1 hash already exists in the photos directory

        Args:
            sha1_hash: SHA1 hash to search for

        Returns:
            True if a photo with this hash exists, False otherwise
        """
        if not self._config.get_photos_path().exists():
            return False

        # Look for any file that contains the hash pattern
        pattern = f"##{sha1_hash}."
        for file_path in self._config.get_photos_path().iterdir():
            if file_path.is_file() and pattern in file_path.name:
                logger.debug(f"Found existing photo with hash {sha1_hash}: {file_path}")
                return True

        return False

    def process_photo(self, source_path: Path) -> bool:
        """
        Process a single photo: calculate hash, check if exists, resize if needed, and save

        Args:
            source_path: Path to the photo to process

        Returns:
            True if photo was processed successfully, False if skipped or failed
        """
        logger.info(f"Processing photo: {source_path}")

        try:
            # Calculate SHA1 hash
            sha1_hash = self.calculate_sha1(source_path)

            # Check if photo already exists
            if self.photo_exists_in_directory(sha1_hash):
                logger.info(
                    f"Photo with hash {sha1_hash} already exists, skipping: {source_path}"
                )
                return False

            # Get image dimensions
            width, height = self.get_image_dimensions(source_path)

            # Calculate target dimensions
            target_width, target_height = self.calculate_resize_dimensions(
                width, height
            )

            # Generate target filename
            target_filename = self.generate_target_filename(source_path, sha1_hash)
            target_path = self._config.get_photos_path() / target_filename

            # Process the image
            if target_width != width or target_height != height:
                # Image needs resizing
                logger.info(
                    f"Resizing photo from {width}x{height} to {target_width}x{target_height}"
                )
                self.resize_image(source_path, target_path, target_width, target_height)
            else:
                # Image doesn't need resizing, just copy
                logger.info("Photo is within size limits, copying without resize")
                self.copy_image(source_path, target_path)

            logger.info(f"Successfully processed photo: {source_path} -> {target_path}")
            return True

        except Exception as e:
            logger.error(f"Failed to process photo {source_path}: {e}", exc_info=True)
            return False

    def import_single_file(self, file_path: Path) -> bool:
        """
        Import a single photo file

        Args:
            file_path: Path to the photo file to import

        Returns:
            True if photo was imported successfully, False if skipped or failed
        """
        if not file_path.exists() or not file_path.is_file():
            logger.error(f"File does not exist or is not a file: {file_path}")
            return False

        if file_path.suffix.lower() not in self.SUPPORTED_EXTENSIONS:
            logger.error(f"File type not supported: {file_path}")
            return False

        logger.info(f"Importing single file: {file_path}")
        return self.process_photo(file_path)

    def import_photos(self, import_directory: Path) -> int:
        """
        Import all supported photos from the import directory

        Args:
            import_directory: Directory to scan for new photos

        Returns:
            Number of photos successfully imported
        """
        if not import_directory.exists():
            logger.warning(f"Import directory does not exist: {import_directory}")
            return 0

        if not import_directory.is_dir():
            logger.error(f"Import path is not a directory: {import_directory}")
            return 0

        logger.info(f"Starting photo import from: {import_directory}")

        # Find all supported image files
        image_files = []
        for file_path in import_directory.iterdir():
            if (
                file_path.is_file()
                and file_path.suffix.lower() in self.SUPPORTED_EXTENSIONS
            ):
                image_files.append(file_path)

        if not image_files:
            logger.info(f"No supported image files found in: {import_directory}")
            return 0

        logger.info(f"Found {len(image_files)} image files to process")

        # Process each image
        processed_count = 0
        for image_path in image_files:
            if self.process_photo(image_path):
                processed_count += 1

        logger.info(
            f"Photo import complete: {processed_count}/{len(image_files)} photos processed successfully"
        )
        return processed_count


def import_photos_from_directory(
    config: FrameConfig,
) -> int:
    """
    Convenience function to import photos from a directory

    Args:
        config: Optional FrameConfig object to get dimensions from

    Returns:
        Number of photos successfully imported
    """
    import_path = config.get_import_path()
    if import_path is None:
        logger.warning("No import directory configured, skipping import")
        return 0

    return PhotoImporter(config).import_photos(import_path)
