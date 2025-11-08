#!/usr/bin/env python3
"""
Digital Picture Frame - Main Application Entry Point
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
import os
import sys
import threading

from picture_frame_ui.config import FrameConfig
from photo_manager.photos import create_photo_loader
from photo_manager.importer import import_photos_from_directory
from picture_frame_ui.ui import (
    run_app,
    get_screen_dimensions,
)
from frame_server.server import run_server


def setup_logging():
    """Set up logging configuration"""
    log_level = logging.DEBUG if os.getenv("DEBUG") else logging.INFO

    # Configure logging
    logging.basicConfig(
        level=log_level,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        handlers=[logging.StreamHandler(sys.stderr)],
    )

    # Set specific logger levels
    logging.getLogger("picture_frame_ui").setLevel(log_level)

    if log_level == logging.DEBUG:
        logging.info("Debug logging enabled")


def main():
    """Main application entry point"""
    setup_logging()
    logger = logging.getLogger(__name__)

    logger.info("Digital Picture Frame starting up")

    # Load configuration
    logger.debug("Loading configuration")
    config = FrameConfig.load()

    # Import new photos if import directory is configured
    import_path = config.get_import_path()
    if import_path is not None:
        logger.info(f"Import directory configured: {import_path}")
        photos_path = config.get_photos_path()

        # Ensure photos directory exists
        photos_path.mkdir(parents=True, exist_ok=True)

        try:
            # Get screen dimensions for optimal photo resizing
            screen_width, screen_height = get_screen_dimensions()
            logger.debug(
                f"Using screen dimensions for photo import: {screen_width}x{screen_height}"
            )

            imported_count = import_photos_from_directory(
                import_path,
                photos_path,
                max_width=screen_width,
                max_height=screen_height,
            )
            if imported_count > 0:
                logger.info(f"Successfully imported {imported_count} new photos")
            else:
                logger.debug("No new photos to import")
        except Exception as e:
            logger.error(f"Failed to import photos: {e}", exc_info=True)
            # Continue with application startup even if import fails
    else:
        logger.debug("No import directory configured, skipping photo import")

    logger.debug(
        f"Creating Photo Loader from configured directory: {config.photos_directory}"
    )

    # Validate photos directory exists
    photos_path = config.get_photos_path()
    if not photos_path.exists():
        logger.error(f"Photos directory does not exist: {photos_path}")
        logger.info(f"Please create the directory or update the configuration")
        return 1

    if not photos_path.is_dir():
        logger.error(f"Photos path is not a directory: {photos_path}")
        return 1

    # Create photo loader
    photo_loader = create_photo_loader(str(photos_path))

    # Start the frame server in a background thread
    logger.info("Starting frame server in background")
    server_thread = threading.Thread(
        target=run_server,
        args=(str(photos_path),),
        kwargs={
            "host": "0.0.0.0",
            "port": 8080,
        },
        daemon=True,  # Dies when main thread dies
        name="FrameServer",
    )
    server_thread.start()
    logger.info("Frame server started on http://0.0.0.0:8080")

    logger.debug("Starting UI")
    exit_code = run_app(config, photo_loader)

    logger.info("Digital Picture Frame shutting down")
    return exit_code


if __name__ == "__main__":
    main()
