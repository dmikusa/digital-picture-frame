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
from pathlib import Path

from picture_frame_ui.config import FrameConfig
from picture_frame_ui.photos import create_photo_loader
from picture_frame_ui.ui import run_app, InitializationError, RuntimeError


def setup_logging():
    """Set up logging configuration"""
    log_level = logging.DEBUG if os.getenv('DEBUG') else logging.INFO
    
    # Configure logging
    logging.basicConfig(
        level=log_level,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s',
        handlers=[
            logging.StreamHandler(sys.stderr)
        ]
    )
    
    # Set specific logger levels
    logging.getLogger('picture_frame_ui').setLevel(log_level)
    
    if log_level == logging.DEBUG:
        logging.info("Debug logging enabled")


def main():
    """Main application entry point"""
    setup_logging()
    logger = logging.getLogger(__name__)
    
    logger.info("Digital Picture Frame starting up")
    
    try:
        # Load configuration
        logger.debug("Loading configuration")
        config = FrameConfig.load()
        
        logger.debug(f"Creating Photo Loader from configured directory: {config.photos_directory}")
        
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
        
        logger.debug("Starting UI")
        exit_code = run_app(config, photo_loader)
        
        logger.info("Digital Picture Frame shutting down")
        return exit_code
        
    except InitializationError as e:
        logger.error(f"UI Initialization Error: {e}")
        return 1
    except RuntimeError as e:
        logger.error(f"UI Runtime Error: {e}")
        return 1
    except FileNotFoundError as e:
        logger.error(f"File not found: {e}")
        logger.info("Please check your photos directory configuration")
        return 1
    except Exception as e:
        logger.error(f"Unexpected error: {e}", exc_info=True)
        return 1


if __name__ == "__main__":
    sys.exit(main())
