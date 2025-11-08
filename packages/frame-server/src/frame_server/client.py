#!/usr/bin/env python3
"""
Frame Server Client - Command-line client for interacting with the frame server
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

import argparse
import sys
import json
from pathlib import Path
from urllib.request import urlopen, Request
from urllib.error import URLError, HTTPError
from urllib.parse import urlencode


def upload_photo(server_url: str, photo_path: Path):
    """Upload a photo to the frame server"""
    try:
        with open(photo_path, "rb") as f:
            photo_data = f.read()

        request = Request(f"{server_url}/upload", data=photo_data, method="POST")
        request.add_header("Content-Type", "application/octet-stream")

        with urlopen(request) as response:
            result = json.loads(response.read().decode())
            print(f"✓ Upload successful: {result.get('message', 'Photo uploaded')}")
            return True

    except FileNotFoundError:
        print(f"✗ Error: Photo file not found: {photo_path}")
        return False
    except HTTPError as e:
        try:
            error_data = json.loads(e.read().decode())
            print(f"✗ Upload failed: {error_data.get('error', 'Unknown error')}")
        except:
            print(f"✗ Upload failed: HTTP {e.code}")
        return False
    except URLError as e:
        print(f"✗ Connection failed: {e.reason}")
        return False
    except Exception as e:
        print(f"✗ Upload failed: {e}")
        return False


def list_photos(server_url: str):
    """List photos on the frame server"""
    try:
        with urlopen(f"{server_url}/photos") as response:
            result = json.loads(response.read().decode())

            photos = result.get("photos", [])
            print(f"Photos directory: {result.get('directory', 'Unknown')}")
            print(f"Total photos: {result.get('count', 0)}")
            print()

            if photos:
                print("Photos:")
                for photo in photos:
                    size_mb = photo.get("size", 0) / (1024 * 1024)
                    print(f"  • {photo.get('name', 'Unknown')} ({size_mb:.1f} MB)")
            else:
                print("No photos found.")

            return True

    except HTTPError as e:
        try:
            error_data = json.loads(e.read().decode())
            print(
                f"✗ Failed to list photos: {error_data.get('error', 'Unknown error')}"
            )
        except:
            print(f"✗ Failed to list photos: HTTP {e.code}")
        return False
    except URLError as e:
        print(f"✗ Connection failed: {e.reason}")
        return False
    except Exception as e:
        print(f"✗ Failed to list photos: {e}")
        return False


def check_health(server_url: str):
    """Check server health"""
    try:
        with urlopen(f"{server_url}/health") as response:
            result = json.loads(response.read().decode())
            status = result.get("status", "unknown")
            print(f"Server status: {status}")
            return status == "healthy"

    except HTTPError as e:
        print(f"✗ Health check failed: HTTP {e.code}")
        return False
    except URLError as e:
        print(f"✗ Connection failed: {e.reason}")
        return False
    except Exception as e:
        print(f"✗ Health check failed: {e}")
        return False


def main():
    """Main entry point for the frame server client"""
    parser = argparse.ArgumentParser(
        description="Digital Picture Frame Server Client",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s upload photo.jpg                    # Upload a photo
  %(prog)s list                                # List photos on server
  %(prog)s health                              # Check server health
  %(prog)s --server http://192.168.1.100:8080 upload photo.jpg
        """,
    )

    parser.add_argument(
        "--server",
        "-s",
        default="http://localhost:8080",
        help="Frame server URL (default: http://localhost:8080)",
    )

    subparsers = parser.add_subparsers(dest="command", help="Available commands")

    # Upload command
    upload_parser = subparsers.add_parser("upload", help="Upload a photo")
    upload_parser.add_argument("photo", help="Path to photo file")

    # List command
    subparsers.add_parser("list", help="List photos on server")

    # Health command
    subparsers.add_parser("health", help="Check server health")

    args = parser.parse_args()

    if args.command is None:
        parser.print_help()
        return 1

    # Remove trailing slash from server URL
    server_url = args.server.rstrip("/")

    if args.command == "upload":
        photo_path = Path(args.photo)
        if not photo_path.exists():
            print(f"✗ Error: Photo file not found: {photo_path}")
            return 1

        success = upload_photo(server_url, photo_path)
        return 0 if success else 1

    elif args.command == "list":
        success = list_photos(server_url)
        return 0 if success else 1

    elif args.command == "health":
        success = check_health(server_url)
        return 0 if success else 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
