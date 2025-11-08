# Frame Server

Web server for uploading and managing photos on the digital picture frame.

## Features

- HTTP API for photo upload
- Photo listing endpoint
- Health check endpoint
- Command-line client for remote management

## API Endpoints

- `GET /health` - Health check
- `GET /photos` - List photos on the frame
- `POST /upload` - Upload a new photo

## Usage

### Start the server

```bash
uv run frame-server --host 0.0.0.0 --port 8080 --photos-dir /path/to/photos
```

### Use the client

```bash
# Upload a photo
uv run frame-client upload photo.jpg

# List photos
uv run frame-client list

# Check server health
uv run frame-client health

# Use a different server
uv run frame-client --server http://192.168.1.100:8080 upload photo.jpg
```

## Dependencies

- photo-manager (workspace dependency) for photo processing