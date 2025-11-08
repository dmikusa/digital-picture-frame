#!/usr/bin/env python3
"""
Frame Server - Web API for managing photos on the digital picture frame
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
import json
import tempfile
import shutil
from pathlib import Path
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs
from typing import Optional
from photo_manager.importer import import_photos_from_directory


logger = logging.getLogger(__name__)


class FrameServerHandler(BaseHTTPRequestHandler):
    """HTTP request handler for the frame server"""

    def __init__(self, *args, photos_directory: Optional[str] = None, **kwargs):
        self.photos_directory = photos_directory or "/tmp/frame-photos"
        super().__init__(*args, **kwargs)

    def do_GET(self):
        """Handle GET requests"""
        parsed_path = urlparse(self.path)

        if parsed_path.path == "/health":
            self._send_json_response({"status": "healthy"}, 200)
        elif parsed_path.path == "/photos":
            self._list_photos()
        else:
            self._send_json_response({"error": "Not found"}, 404)

    def do_POST(self):
        """Handle POST requests"""
        parsed_path = urlparse(self.path)

        if parsed_path.path == "/upload":
            self._handle_upload()
        else:
            self._send_json_response({"error": "Not found"}, 404)

    def _handle_upload(self):
        """Handle photo upload"""
        try:
            content_length = int(self.headers.get("Content-Length", 0))
            if content_length == 0:
                self._send_json_response({"error": "No content provided"}, 400)
                return

            # Read the uploaded file data
            file_data = self.rfile.read(content_length)

            # Create a temporary file
            with tempfile.NamedTemporaryFile(delete=False, suffix=".jpg") as temp_file:
                temp_file.write(file_data)
                temp_file_path = temp_file.name

            try:
                # Create temporary directory for import
                with tempfile.TemporaryDirectory() as temp_dir:
                    temp_photo_path = Path(temp_dir) / "uploaded_photo.jpg"
                    shutil.move(temp_file_path, temp_photo_path)

                    # Import the photo using the photo_manager
                    photos_path = Path(self.photos_directory)
                    photos_path.mkdir(parents=True, exist_ok=True)

                    # Use standard screen dimensions for import
                    imported_count = import_photos_from_directory(
                        Path(temp_dir),
                        photos_path,
                        max_width=1920,
                        max_height=1080,
                    )

                    if imported_count > 0:
                        self._send_json_response(
                            {
                                "success": True,
                                "message": f"Successfully imported {imported_count} photo(s)",
                            },
                            200,
                        )
                    else:
                        self._send_json_response(
                            {"error": "Failed to import photo"}, 400
                        )

            except Exception as e:
                logger.error(f"Error importing photo: {e}")
                self._send_json_response({"error": f"Import failed: {str(e)}"}, 500)

        except Exception as e:
            logger.error(f"Error handling upload: {e}")
            self._send_json_response({"error": f"Upload failed: {str(e)}"}, 500)

    def _list_photos(self):
        """List all photos in the photos directory"""
        try:
            photos_path = Path(self.photos_directory)
            if not photos_path.exists():
                photos_path.mkdir(parents=True, exist_ok=True)

            # List all image files
            image_extensions = {".jpg", ".jpeg", ".png", ".bmp", ".tiff", ".webp"}
            photos = []

            for file_path in photos_path.iterdir():
                if file_path.is_file() and file_path.suffix.lower() in image_extensions:
                    photos.append(
                        {
                            "name": file_path.name,
                            "size": file_path.stat().st_size,
                            "modified": file_path.stat().st_mtime,
                        }
                    )

            self._send_json_response(
                {"photos": photos, "count": len(photos), "directory": str(photos_path)},
                200,
            )

        except Exception as e:
            logger.error(f"Error listing photos: {e}")
            self._send_json_response({"error": f"Failed to list photos: {str(e)}"}, 500)

    def _send_json_response(self, data: dict, status_code: int):
        """Send a JSON response"""
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()

        response_data = json.dumps(data, indent=2).encode("utf-8")
        self.wfile.write(response_data)

    def log_message(self, format, *args):
        """Override log_message to use Python logging"""
        logger.info(f"{self.address_string()} - {format % args}")


def create_handler_class(photos_directory: str):
    """Create a handler class with the photos directory bound"""

    class BoundFrameServerHandler(FrameServerHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, photos_directory=photos_directory, **kwargs)

    return BoundFrameServerHandler


def run_server(
    host: str = "0.0.0.0", port: int = 8080, photos_directory: Optional[str] = None
):
    """Run the frame server"""
    if photos_directory is None:
        photos_directory = os.environ.get("FRAME_PHOTOS_DIR", "/tmp/frame-photos")

    logger.info(f"Starting frame server on {host}:{port}")
    logger.info(f"Photos directory: {photos_directory}")

    handler_class = create_handler_class(photos_directory)
    server = None

    try:
        server = HTTPServer((host, port), handler_class)
        logger.info(f"Frame server running at http://{host}:{port}")
        logger.info("Available endpoints:")
        logger.info("  GET  /health - Health check")
        logger.info("  GET  /photos - List photos")
        logger.info("  POST /upload - Upload photo")

        server.serve_forever()

    except KeyboardInterrupt:
        logger.info("Shutting down frame server")
        if server is not None:
            server.shutdown()
    except Exception as e:
        logger.error(f"Server error: {e}")
        if server is not None:
            server.shutdown()
        raise


def main():
    """Main entry point for the frame server"""
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    )

    # Parse command line arguments for basic configuration
    import argparse

    parser = argparse.ArgumentParser(description="Digital Picture Frame Server")
    parser.add_argument("--host", default="0.0.0.0", help="Server host")
    parser.add_argument("--port", type=int, default=8080, help="Server port")
    parser.add_argument("--photos-dir", help="Photos directory path")

    args = parser.parse_args()

    try:
        run_server(args.host, args.port, args.photos_dir)
    except Exception as e:
        logger.error(f"Failed to start server: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
