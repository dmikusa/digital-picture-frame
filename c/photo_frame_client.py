#!/usr/bin/env python3
"""
photo_frame_client.py

Simple Python 3 client for photo_frame.c.
Opens a persistent AF_UNIX connection, sends one image path,
blocks until it receives "READY", then sends the next.
This gives natural backpressure because the display app pauses
socket reads when both GPU slots and the CPU pending buffer are full.

Usage:
    python3 photo_frame_client.py /tmp/photo-frame.sock \
        /path/to/photo1.jpg \
        /path/to/photo2.jpg \
        /path/to/photo3.jpg
"""

import sys
import socket


def wait_for_ready(sock: socket.socket) -> bool:
    """Block until the server sends the string READY."""
    buf = b""
    while True:
        chunk = sock.recv(64)
        if not chunk:
            print("Server closed connection before sending READY", file=sys.stderr)
            return False
        buf += chunk
        if b"READY" in buf:
            return True


def main() -> int:
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <socket_path> <image1> [image2] ...", file=sys.stderr)
        return 1

    sock_path = sys.argv[1]
    image_paths = sys.argv[2:]

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.connect(sock_path)
        print(f"Connected to {sock_path}")

        for path in image_paths:
            print(f"Sending: {path}")
            msg = f"IMG {path}\n".encode("utf-8")
            sock.sendall(msg)

            if not wait_for_ready(sock):
                return 1
            print("  -> got READY, can send next")

    print("All images sent.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
