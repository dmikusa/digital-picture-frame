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
import json
import tempfile
from pathlib import Path
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse
from photo_manager.importer import PhotoImporter
from functools import partial
from picture_frame_ui.config import FrameConfig


logger = logging.getLogger(__name__)


class FrameServerHandler(BaseHTTPRequestHandler):
    """HTTP request handler for the frame server"""

    def __init__(
        self,
        *args,
        config: FrameConfig,
        **kwargs,
    ):
        # Set attributes BEFORE calling super().__init__() because it immediately processes the request
        self.photo_importer = PhotoImporter(
            config=config,
        )
        self._config = config

        # This starts processing the request immediately!
        super().__init__(*args, **kwargs)

    def do_GET(self):
        """Handle GET requests"""
        parsed_path = urlparse(self.path)

        if parsed_path.path == "/" or parsed_path.path == "/index":
            self._serve_index_html()
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
        logger.info(f"_handle_upload called, checking for max_file_size attribute")
        logger.info(f"self.__dict__: {self.__dict__}")
        if not hasattr(self, "max_file_size"):
            logger.error("max_file_size attribute not found!")
            self._send_json_response({"error": "Server configuration error"}, 500)
            return

        try:
            content_length = int(self.headers.get("Content-Length", 0))
            if content_length == 0:
                self._send_json_response({"error": "No content provided"}, 400)
                return

            # Check for file size limit
            if content_length > self._config.server_max_file_size:
                self._send_json_response(
                    {
                        "error": f"File too large. Maximum size is {self._config.server_max_file_size // (1024 * 1024)}MB"
                    },
                    413,  # HTTP 413 Payload Too Large
                )
                return

            # Read the uploaded file data
            # Create a temporary file and write data in chunks to avoid memory issues
            with tempfile.NamedTemporaryFile(delete=False, suffix=".jpg") as temp_file:
                bytes_remaining = content_length
                bytes_read = 0
                chunk_size = 8192  # 8KB chunks

                while bytes_remaining > 0:
                    chunk_size_to_read = min(chunk_size, bytes_remaining)
                    chunk = self.rfile.read(chunk_size_to_read)
                    if not chunk:
                        break

                    bytes_read += len(chunk)
                    # Check if we've exceeded the max file size during reading
                    if bytes_read > self._config.server_max_file_size:
                        # Clean up the temporary file
                        temp_file.close()
                        Path(temp_file.name).unlink()
                        self._send_json_response(
                            {
                                "error": f"File too large. Maximum size is {self._config.server_max_file_size // (1024 * 1024)}MB"
                            },
                            413,
                        )
                        return

                    temp_file.write(chunk)
                    bytes_remaining -= len(chunk)

                temp_file_path = Path(temp_file.name)

            try:
                # Import the photo directly using the PhotoImporter instance
                success = self.photo_importer.import_single_file(temp_file_path)

                if success:
                    self._send_json_response(
                        {
                            "success": True,
                            "message": "Successfully imported photo",
                        },
                        200,
                    )
                else:
                    self._send_json_response({"error": "Failed to import photo"}, 400)

            except Exception as e:
                logger.error(f"Error importing photo: {e}")
                self._send_json_response({"error": f"Import failed: {str(e)}"}, 500)
            finally:
                # Clean up the temporary file
                if temp_file_path.exists():
                    temp_file_path.unlink()

        except Exception as e:
            logger.error(f"Error handling upload: {e}")
            self._send_json_response({"error": f"Upload failed: {str(e)}"}, 500)

    def _send_json_response(self, data: dict, status_code: int):
        """Send a JSON response"""
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()

        response_data = json.dumps(data, indent=2).encode("utf-8")
        self.wfile.write(response_data)

    def _serve_index_html(self):
        """Serve the index.html file"""
        try:
            # Get the path to the HTML file relative to this module
            current_dir = Path(__file__).parent
            html_file_path = current_dir / "index.html"

            if html_file_path.exists():
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()

                with open(html_file_path, "rb") as f:
                    self.wfile.write(f.read())
            else:
                self._send_json_response({"error": "Index file not found"}, 404)

        except Exception as e:
            logger.error(f"Error serving index.html: {e}")
            self._send_json_response({"error": "Internal server error"}, 500)

    def log_message(self, format, *args):
        """Override log_message to use Python logging"""
        logger.info(f"{self.address_string()} - {format % args}")


def run_server(
    config: FrameConfig,
):
    """Run the frame server"""

    def handler_factory(*args, **kwargs):
        return FrameServerHandler(
            *args,
            config=config,
            **kwargs,
        )

    server = HTTPServer((config.server_host, config.server_port), handler_factory)
    logger.info(
        f"Frame server running at http://{config.server_host}:{config.server_port}"
    )
    logger.info("Available endpoints:")
    logger.info("  GET / or /index - Web interface")
    logger.info("  POST /upload - Upload photo")

    server.serve_forever()
