# Photo Frame Manager — Specification & Design Decisions

## Project Overview

A Rust binary (`photo-frame-manager`) that runs on a Raspberry Pi Zero W2 under
DietPi. It manages a photo collection, drives a separate DRM/GBM/EGL display app
(`photo-frame-display.c`) over Unix domain sockets, imports photos from USB
drives and remote cloud folders, and runs a LAN setup UI (config mode) when the
frame needs configuration.

The manager uses synchronous, blocking code throughout: a main thread plus four
worker threads built on `std::thread`, blocking socket I/O, and `std::sync`
primitives. There is no async runtime (no tokio) and no third-party CSV parser.

---

## 1. Core Features

### 1.1 Process Startup

- CLI: `photo-frame-manager [--import-dir <dir>] <config.toml>`. `--help` prints
  usage.
- Acquires an exclusive PID lock at `/tmp/photo-frame.lock` using `flock()` and a
  written PID. A lock whose PID is no longer alive is treated as stale and
  removed.
- Parses and validates `config.toml`, canonicalizes `photos_dir`, and
  initializes the tmpfs logger.
- Creates `photos_dir` if needed, opens or creates the CSV index, compacts it on
  startup when the ghost ratio exceeds 50%, and builds the in-memory dedup set.
- Optionally imports a local directory once (`--import-dir`), then continues
  running normally.
- Spawns four worker threads: display, USB watcher/import, config mode/admin, and
  remote sync.
- Waits for `SIGTERM`/`SIGINT`, sets a shared shutdown flag, sleeps 200ms, and
  exits without joining workers.

### 1.2 Display Thread

- Opens the CSV index in streaming mode — never reads the whole file into
  memory.
- Picks a random valid line on startup, streams line-by-line, and wraps to the
  first valid line at EOF.
- Malformed lines are logged and skipped rather than treated as EOF, so a single
  bad row cannot stop the slideshow.
- Sends `IMG <path>\n` to the display app over `display_socket_path`.
- Backpressure is the kernel socket buffer: when the display app stops reading,
  the write blocks. A 30-second `SO_SNDTIMEO` bounds each write. A partial write
  resumes at the current offset, so no bytes are ever sent twice. Timeouts
  (`WouldBlock`/`TimedOut`) are treated as backpressure and retried on the same
  connection after 100ms. A broken pipe or reset drops the connection, waits 5
  seconds, reconnects, and resends the message from the start.
- Watches the index directory with `notify` (inotify). On modify/create, reopens
  the index and seeks back to the current line. If the seek fails, it restarts
  at the first valid line.
- If the index is empty, sleeps 5 seconds and retries.

### 1.3 USB Import Thread

- Watches `/media` with `notify` (inotify) for directory create/remove events.
- Also reconciles `/media` every 2 seconds by comparing `st_dev` with the parent
  directory. This catches drives mounted before startup, manual `mount`/`umount`,
  and re-mounts over an existing mountpoint directory, none of which produce an
  inotify event.
- After a mount event, waits up to 5 seconds for the filesystem to actually be
  attached, then scans (even if it is still not a mount point).
- Recursively scans for `jpg`, `jpeg`, `heif`, `heifs`, `heic`, and `heics`.
  Hidden files and `$RECYCLE.BIN` / `System Volume Information` are skipped.
- For each photo:
  - Computes a fast CRC32 hash of the first 32KB plus the file size.
  - Checks the shared in-memory dedup set.
  - Converts with ImageMagick into `photos_dir/YYYY/MM/DD/DDDDD_<name>.jpg`,
    where `DDDDD` is the zero-padded CSV line index.
  - Appends `path,original_name,hash` to the CSV index.
- **Imports are serialized.** A process-wide `IMPORT_LOCK` is held per photo
  across the dedup check, conversion, rotation, index append, and dedup insert.
  USB imports and remote downloads both go through `import_single_photo()`, so
  two imports cannot race on the dedup set or rename the index over each other.
- If a drive is removed mid-import, reads fail for the remaining files, each
  failure is logged, and the scan finishes quickly; re-inserting rescans and
  skips duplicates.
- `--import-dir` uses the same per-photo pipeline but scans a local directory
  instead of `/media`.

### 1.4 Remote Sync Thread

The scheduler runs in its own `std::thread` and pulls photos from configured
`[[remote_sources]]` into the same import pipeline.

- Loads `config.toml` at startup and watches its mtime and size. Changes written
  by config mode are picked up without a restart. A config that fails to parse
  or validate is logged and ignored; the previous config stays active.
- A source is due when it has never synced, or when `check_interval_seconds` has
  elapsed since the most recent successful sync or failed attempt. Failed
  attempts still wait for the next interval instead of retrying every pass.
- New sources sync immediately after being added. Removed sources are dropped
  from the sync status; their cursor entries are left in place.
- Cursors are stored in `{photos_dir}/sync-cursors.json`, keyed by source name.
  A value is a provider cursor (`dbx:<cursor>` or `gdrive:<pageToken>`) or a
  legacy Unix timestamp in seconds.
- Per-source status is written to `{photos_dir}/sync-status.json` after every
  attempt (see `docs/config.md`). `last_sync` records the time a sync completed
  successfully, is reloaded at startup so it survives a restart, and is the
  scheduling fallback when the cursor is opaque.

#### Supported sources

**Dropbox** (`type = "dropbox"`)
- Authenticates with exactly one method, validated with
  `users/get_current_account`:
  - a static `access_token`, or
  - `refresh_token` + `app_key` + `app_secret`. The refresh token is exchanged
    for a short-lived access token at
    `https://api.dropboxapi.com/oauth2/token` with `grant_type=refresh_token`;
    tokens are cached and refreshed 60 seconds before expiry.
- `files/list_folder` with `recursive = true` and `folder` (default `/Photos`).
- Follows `has_more` with `files/list_folder/continue`, capped at 1000 pages per
  check.
- Cursors are the provider's opaque cursors, stored as `dbx:<cursor>`. A later
  sync continues with `files/list_folder/continue`; a cursor the provider
  rejects falls back to a full listing. A legacy numeric cursor also triggers a
  full catch-up listing.
- Downloads via `files/download` with `Dropbox-API-Arg`, streaming the body to
  disk.

**Google Drive** (`type = "google_drive"`)
- Requires `folder_id` and exactly one authentication method:
  - `api_key`: public folder, passed as `?key=`.
  - `refresh_token` + `client_id` + `client_secret`: exchanged for an access
    token at connect time; supported in `config.toml` but there is no admin UI
    flow for creating the triple.
  - `service_account_json`: the full JSON key. The manager parses
    `client_email` and `private_key`, builds an RS256 JWT with the
    `drive.readonly` scope, and exchanges it for an access token at
    `token_uri` (default `https://oauth2.googleapis.com/token`). Tokens are
    cached and refreshed 60 seconds before expiry.
- Bearer-auth sources (service account or OAuth refresh token) use the Drive
  Changes API, with cursors stored as `gdrive:<pageToken>`:
  - first sync, or a legacy cursor: fetch `changes/startPageToken` **before** the
    folder listing, then return that start token. Taking the token first means a
    file added while the folder is being paginated is still returned by the next
    `changes/list`. If the start token fails, fall back to the timestamp cursor.
  - later syncs: `changes/list`, following `nextPageToken`, keeping only
    added or changed images still inside the folder (deletions, non-images, and
    files in other folders are filtered out), and returning the new start token.
  - a permission or unsupported Changes error falls back to a full folder
    listing rather than pinning the cursor.
- API-key sources keep the timestamp folder listing, re-listing a 2-second
  window before the cursor so a file modified in the same second is not missed.
- Lists `'<folder_id>' in parents and mimeType contains 'image/' and
  trashed = false`, ordered by `modifiedTime`, page size 1000, following
  `nextPageToken` up to 1000 pages.
- Lists direct children only; subfolders are not traversed.
- Downloads via `files/<id>?alt=media`, streaming to disk.
- `connect()` validates the folder before syncing. For service accounts this
  also fetches the first token, so a bad key fails immediately with a hint to
  share the folder with the service account email as Viewer.

#### Downloads and failure handling

- All HTTP is blocking `reqwest` with native TLS and a 120-second total request
  timeout.
- Transient responses (`429`, `500`, `502`, `503`, `504`) and transport errors
  are retried up to 4 attempts with exponential backoff (500ms base, 30-second
  cap). `Retry-After` seconds are honored. Client errors are not retried.
- Downloads stream into `{photos_dir}/.sync-tmp/<item-index>/<safe-name>` so a
  large batch does not consume RAM and same-named files cannot collide. The
  directory is hidden and outside the index. It is cleared before each batch and
  temporary files are removed after each photo.
- A download failure is reported and **the cursor is not advanced**, because a
  cursor promises everything up to that point was handled. The batch retries on
  the next interval.
- Import or conversion failures after a successful download are logged and
  skipped; they do not pin the cursor forever.
- Duplicate photos are skipped by the shared dedup set.

#### First sync

If photos exist, remote sync runs in the background without changing the screen.
If there are no photos but remote sources are configured, config mode shows a
"First remote sync in progress" screen and polls the index every 2 seconds,
resuming the slideshow as soon as a photo appears.

### 1.5 Config Mode Thread

Config mode decides what the frame should show at startup and reacts to USB
events from the watcher thread. States:

| State | Meaning |
|-------|---------|
| `Normal` | Slideshow running. |
| `FirstSync` | No photos yet; showing the first-sync screen and polling the index. |
| `Importing { pending }` | One or more USB imports are running; showing "Importing photos...". |
| `PendingConfigMode` | Admin server could not be created; retrying every 2 seconds. |
| `ConfigMode` | Setup QR on screen (retried if it cannot be shown); admin HTTP server running; fallback credentials file present. |

Startup screen:

| Condition | Screen |
|-----------|--------|
| Photos exist | Slideshow |
| No photos, remote sources configured | First sync in progress |
| No photos, no remote sources | Setup QR / admin UI |

Transitions:

- **USB inserted** (any state): stop any running admin server, enter `Importing`,
  and show "Importing photos...".
- **All imports complete**: enter `ConfigMode` (render QR and start a fresh admin
  session with a new password and port).
- **USB removed during import** (`Importing` empty): photos exist → slideshow;
  no photos with remote sources → first sync; no photos and no sources → setup.
- **USB removed during config mode**: discard staged changes, stop the server,
  and recover without reopening setup (slideshow, or first sync if sources are
  configured but no photos exist).
- **`POST /api/finish`**: validate and atomically save staged sources, then
  recover the same way.
- **10-minute idle timeout** (reset on every HTTP request): discard staged
  changes, stop the server, and recover without reopening setup.
- **First sync screen**: when photos appear, send `RESUME` and return to
  `Normal`.

Every transition that stops the admin server also removes
`/tmp/photo-frame-setup.txt`.

A `RESUME` the display app does not accept (for example because it closed the
control socket) is queued and retried every 2 seconds while the state is
`Normal`, until it succeeds or the manager shuts down. The first failure is
logged as a warning, then roughly every 30 seconds; a successful retry logs the
number of failed attempts. Entering config mode, or showing a status screen,
clears the queue because the display is being paused again. On shutdown, a
pending `RESUME` gets one final best-effort attempt; worker threads are not
joined, so process exit can cut that attempt short.

Config-mode screens are rendered as JPEGs with `fontdue` text and a `qrcode`
code at the configured native resolution, written to
`/tmp/photo-frame-qr.jpg` or `/tmp/photo-frame-status.jpg`, and shown with
`CLR` + `SHOW` on the control socket.

Only a failure to create the admin server aborts config-mode entry. Once the
server is running, a screen that cannot be rendered or shown is logged and
retried every 2 seconds without stopping the server or changing the state away
from `ConfigMode`.

For as long as config mode is active, `/tmp/photo-frame-setup.txt` (mode 0600,
owned by the service user) contains the primary `http://<ip>:<port>` URL, the
`http://photo-frame.local:<port>` alternative, the username `photo-frame`, and
the session password. This is the fallback when the QR screen cannot be shown.
The file is removed when leaving config mode (`POST /api/finish`, USB removal,
idle timeout) and on shutdown.

### 1.6 Admin UI and Setup QR

- The C app creates a second Unix socket (`control_socket_path`) for
  `CLR\n`, `SHOW <path>\n`, and `RESUME\n`. Config mode uses it to pause the
  slideshow and hold a setup or status image.
- If the control connection drops while the slideshow is paused or a `SHOW`
  image is held, the display app resumes the slideshow automatically, so a
  crashed or restarted manager cannot leave a stale QR/status screen up.
- The QR encodes `http://photo-frame:<password>@<ip>:<port>` so a phone can open
  the UI and authenticate in one scan. The screen also prints the plain
  `http://<ip>:<port>` URL, the password, and an optional
  `http://photo-frame.local:<port>` line.
- The LAN IP is detected with `hostname -I` (first non-loopback IPv4), falling
  back to `127.0.0.1`. mDNS is optional; the IP URL works without it.
- The admin server listens on `0.0.0.0` on the first free port in 8100–8199.
- Username is `photo-frame`; the password is 8 random alphanumeric characters,
  regenerated for every config session and shown on screen and in
  `/tmp/photo-frame-setup.txt`.
- Every route is behind HTTP Basic Auth. There are no unauthenticated callbacks.
- Admin changes are staged in memory, seeded with the current sources. `Finish`
  validates every entry and atomically rewrites `config.toml` (temp file created
  with mode 0600 in the same directory, then `rename()`). A validation failure
  returns 400 and a save failure returns 500; either way the server keeps running
  with `finished` false so the user can fix the problem and retry. Leaving config
  mode without finishing writes nothing, and leaving after a successful finish
  does not write the file a second time.
- `GET /api/sources` and `GET /api/status` redact sensitive parameter values
  (`access_token`, `api_key`, `refresh_token`, `client_id`, `client_secret`,
  `app_key`, `app_secret`, `service_account_json`) as `"***"`. Staging,
  validation, and the saved config keep the real values.

Routes:

| Route | Method | Description |
|-------|--------|-------------|
| `/` | GET | Dashboard with staged source cards |
| `/sources` | GET | Source list with delete forms |
| `/sources/add` | GET | Source type picker |
| `/sources/dropbox` | GET | Dropbox form |
| `/sources/google-drive` | GET | Public Google Drive form |
| `/sources/google-drive-service-account` | GET | Service-account form and instructions |
| `/api/sources` | GET | Staged sources as JSON (credentials redacted) |
| `/api/sources/dropbox` | POST | Stage a Dropbox source |
| `/api/sources/google-drive` | POST | Stage a Google Drive source |
| `/api/sources/<name>` | DELETE / POST | Remove a staged source |
| `/api/status` | GET | Staged sources plus `sync-status.json` (credentials redacted) |
| `/api/finish` | POST | Validate, save, and stop the server; 400 on invalid input, 500 on save failure |

Browser source form posts receive `303` redirects, the finish form returns an
HTML success page, and JSON requests receive JSON. Invalid input returns an HTML
error page for browsers and a JSON 400 for API clients.

### 1.7 Storage Rotation

- Photos live on a dedicated ext4 partition.
- When a conversion or write fails with `ENOSPC`, the manager deletes the oldest
  `batch_delete_size` photos (files removed, `start_line` incremented, index
  renamed) and retries. This repeats until the write succeeds or there is
  nothing left to delete.
- Remote downloads stage files on the photos partition, so an `ENOSPC` download
  also rotates the oldest photos and retries. Rotation runs under the import
  lock; if it frees nothing, the sync fails.
- Deleted entries remain as ghosts in the CSV. When ghosts exceed 50% of the
  file, the index is compacted on startup to avoid runtime pauses.

### 1.8 Configuration

TOML file, path passed on the command line. Fields:

- `photos_dir` (required)
- `display_socket_path` (required; legacy alias `socket_path`)
- `control_socket_path` (default `/run/photo-frame/control.sock`)
- `native_resolution` (`WxH`, required)
- `aspect_ratio_mode` (`fit` default, or `fill`)
- `batch_delete_size` (default 20)
- `log_max_size` (default 262144 bytes)
- `log_max_files` (default 2)
- `remote_sources` (optional array)

See `docs/config.md` for the full reference and source parameters.

### 1.9 Logging

- Uses the `log` facade with a custom tmpfs logger writing to
  `/tmp/photo-frame.log`.
- Size-based rotation at `log_max_size`; old logs are gzip-compressed as
  `.1.gz`, `.2.gz`, and excess files are deleted. `log_max_files` controls how
  many rotated files are retained.
- Format: `YYYY-MM-DDTHH:MM:SSZ <level> <message>`.
- The C display app has its own minimal tmpfs logger writing to
  `/tmp/photo-frame-display.log` (mode 0600, UTC timestamps, flushed per line).
  It rotates once at 256 KiB to `.1` and keeps a single backup; if the file
  cannot be opened it falls back to stderr. Nothing is fsynced.

### 1.10 Graceful Shutdown

- Handles `SIGTERM`/`SIGINT`.
- Sets the shared shutdown flag, closes the display socket, sleeps 200ms, and
  exits. Workers are not joined because they may be blocked in socket or inotify
  I/O. The OS reclaims sockets and file descriptors.
- The display app handles disconnects gracefully and keeps rendering its current
  image.

### 1.11 Display App Environment Variables

The C app reads these optional variables at startup:

- `PHOTO_FRAME_FADE_DURATION`: cross-fade seconds. Default 1.5; 0 = instant cut.
- `PHOTO_FRAME_SKIP_FRAMES`: skip N frames during a fade to reduce CPU. 0 =
  render every frame (default).
- `DISPLAY_SOCKET_PATH`: image socket. Default
  `/run/photo-frame/photo-frame.sock`.
- `CONTROL_SOCKET_PATH`: control socket. Default
  `/run/photo-frame/control.sock`.

The two socket paths are shared configuration and must match `config.toml`.

---

## 2. Decision Points & Rationale

### 2.1 Synchronous Threads, No Async Runtime

**Decision:** Use `std::thread` and blocking I/O everywhere, including the
`reqwest` blocking client and `tiny_http_fork`.

**Why:** The workload is a handful of long-lived loops plus infrequent imports.
Async adds runtime, dependency, and debugging overhead without a benefit on a
single-core Pi Zero W2. Blocking HTTP and socket calls are also easier to test.

### 2.2 Display Protocol — Blocking Send with Write Timeout

**Decision:** Send `IMG` without waiting for `READY`. Backpressure is the kernel
socket buffer. A 30-second write timeout distinguishes a stalled-but-alive app
(backpressure) from a dead connection. A partial write resumes at its current
offset so bytes are never duplicated; timeouts retry on the same connection, and
broken pipes reconnect after 5 seconds and resend from the start.

**Why:** The display app already pauses `read()` when its image pipeline is
full. Kernel-level blocking is simpler than a round-trip protocol and avoids the
risk of a lost `READY`.

### 2.3 Index Format — CSV with Circular Buffer via Filename

**Decision:** One CSV line per photo: `path,original_name,hash`. The filename
encodes metadata: `index-<start_line>-<valid_count>.csv`. Logical deletion
increments `start_line`; compaction rewrites the file when ghosts exceed 50%.

Fields containing a comma or quote are CSV-quoted with doubled quotes. Embedded
`\r`/`\n` are replaced with spaces (with a warning) because the reader is
line-based and cannot represent them. A malformed row is logged and skipped
rather than treated as EOF; legacy unquoted lines still parse.

**Why:** CSV parsing is cheap, appends are atomic and cheap, and the
filename-as-metadata approach needs no sidecar file. Since startup scans the
whole CSV to build the dedup set anyway, the reader simply chooses a random
starting line instead of persisting an offset.

### 2.4 Serialized Imports

**Decision:** A process-wide mutex serializes `import_single_photo()`.

**Why:** `IndexWriter` captures metadata when opened and renames the file from
that snapshot. Concurrent imports (USB mount threads and the remote sync
scheduler) could otherwise rename over each other and lose entries. The
check-then-insert on the dedup set is racy for the same reason. Scanning happens
before the lock, so it is only held per photo.

### 2.5 Duplicate Detection — CRC32 of First 32KB + File Size

**Decision:** On startup, scan the CSV into an in-memory `HashSet<u64>`. During
import, hash the first 32KB plus total size with `crc32fast`; skip on a match.

**Why:** Full-file hashing is CPU-intensive on a Pi Zero W2. The first 32KB is a
single `read()` and catches identical files; the size guards against tiny-file
collisions. The set costs about 8 bytes per photo.

### 2.6 USB Mount Detection — inotify Plus Periodic Reconciliation

**Decision:** Watch `/media` with `notify`, and also reconcile every 2 seconds
using `st_dev`.

**Why:** inotify is cheap and immediate, but a filesystem attached over an
existing mountpoint directory emits no event under `/media`. The sweep catches
pre-mounted drives and manual mounts without polling file contents.

### 2.7 Image Conversion — Shell Out to ImageMagick

**Decision:** Use `magick`, falling back to `convert`. `-auto-orient` runs first
so EXIF rotation is applied before resizing. Fit mode uses `-resize WxH`; fill
mode uses `-resize WxH^ -gravity center -extent WxH`. Output is always JPEG;
HEIC/HEIF sources are normalized so `stb_image` can decode them.

**Why:** Avoids a heavy Rust image pipeline, keeps memory low, and uses a
well-optimized external tool for the CPU-heavy resize. Imports are infrequent,
so process startup cost is acceptable.

### 2.8 Remote Sync — Provider Cursors with a Timestamp Fallback

**Decision:** Each source uses the provider's own cursor when it can: Dropbox's
opaque `files/list_folder` cursor (`dbx:<cursor>`) and the Drive Changes API page
token (`gdrive:<pageToken>`) for bearer-auth Google sources. Cursors are
prefixed so a stored value can be told apart from a legacy Unix-timestamp
cursor. API-key Google sources keep the timestamp listing, with a 2-second
overlap. Pagination is followed within one check and capped at 1000 pages.

**Why:** Provider cursors express exactly what changed since the last sync and
avoid re-scanning a large folder, which the timestamp filter could not do. A
timestamp remains the fallback for API-key sources and legacy cursors because it
needs no extra API surface, and the overlap covers a same-second edit. The cap
protects against a server that never clears `has_more`.

### 2.9 Cursor Advances Only on Complete Download Batches

**Decision:** If any download in a batch fails, return an error and keep the old
cursor. Import/conversion failures after a successful download are skipped.

**Why:** A cursor means "everything up to here is handled". Advancing past a
failed download would skip those photos forever; pinning it on one corrupt image
would instead block all future syncs.

### 2.10 Config Mode Over Plain HTTP with a Rotating Password

**Decision:** Serve the admin UI on HTTP with a per-session Basic Auth password
embedded in the QR code. Use the LAN IP as the primary URL, with mDNS as an
optional alternative.

**Why:** The frame may have no internet access, no TLS certificate, and no
stable hostname. Plain HTTP on the LAN with a short-lived random password keeps
setup working from a phone with no extra infrastructure. HTTPS for the admin UI
is future work (see `plans/SYNC_SOURCES.md`).

### 2.11 Google Drive Authentication — Service Account for Private Folders

**Decision:** Support API keys for public folders, a config-file-only OAuth
refresh-token triple, and service accounts for private folders. Sign a
short-lived RS256 JWT with the service-account key and exchange it for a
`drive.readonly` token.

**Why:** A web OAuth callback needs an HTTPS redirect URI, which the HTTP admin
UI cannot provide. Google's device flow is restricted to
`drive.appdata`/`drive.file`, which cannot read an existing user folder. A
service account reads a folder shared with its synthetic email and needs no
interactive login.

### 2.12 Logging — Custom Rotation Logger on Tmpfs

**Decision:** Custom `log::Log` writing to `/tmp/photo-frame.log` with
size-based rotation and gzip compression.

**Why:** `/tmp` is tmpfs on DietPi, so logging causes no SD card wear. Existing
crates lack size-based rotation, and `tracing` is overkill.

### 2.13 Storage Rotation — Delete Oldest Batch on ENOSPC

**Decision:** On `ENOSPC`, delete the oldest `batch_delete_size` valid entries,
increment `start_line`, and retry.

**Why:** A dedicated partition makes "full" unambiguous. The CSV is ordered by
insertion time, so the oldest photos are always at the front of the valid
range; no filesystem walk is needed.

### 2.14 Graceful Shutdown — Immediate Socket Close

**Decision:** On a signal, close the display socket and exit without flushing an
in-flight send.

**Why:** Completing a send while the kernel buffer is full would block
indefinitely. The display app already tolerates disconnects.

---

## 3. Technical Architecture

### 3.1 Threads

- **Main thread:** CLI, PID lock, config, logger, index init/compaction, dedup
  set, signal handling.
- **Display thread:** streams the index and sends `IMG`.
- **USB watcher thread:** watches `/media`, spawns one import thread per mount,
  emits `MountEvent`s.
- **Config mode/admin thread:** state machine, control-socket screens, QR/admin
  server, staged config. The admin server itself runs in a child thread while
  config mode is active.
- **Remote sync thread:** scheduler, provider clients, downloads, cursors,
  status file.

### 3.2 Concurrency

- The index is append-only and one display thread reads it.
- A process-wide import mutex serializes all imports and index mutations.
- The dedup set is `Arc<Mutex<HashSet<u64>>>`, shared by startup, USB, and
  remote sync.
- The USB watcher sends mount events to config mode over an `mpsc` channel.
- Shutdown is an `Arc<AtomicBool>` observed by every worker.

### 3.3 Error Handling

- **Fatal:** config parse/validation failure, PID lock held, photos directory
  creation failure, index init/compaction failure, logger init failure.
- **Retry:** display socket disconnect (5s), control socket connect (5s
  deadline), transient HTTP.
- **Skip and continue:** individual file read/convert/copy errors, duplicates,
  remote import/conversion errors.
- **Wait:** empty index, display app not yet running, first remote sync.

### 3.4 File Formats

- **Config:** TOML.
- **Index:** CSV, fields `path,original_name,hash`; metadata in the filename.
- **Photos:** `photos_dir/YYYY/MM/DD/DDDDD_<original_name>.jpg`, where `DDDDD` is
  the CSV line index and the date comes from the source file's mtime (UTC).
- **Sync cursors:** `photos_dir/sync-cursors.json`,
  `{source_name: "dbx:<cursor>" | "gdrive:<pageToken>" | "unix_seconds"}`.
- **Sync status:** `photos_dir/sync-status.json`,
  `{"sources": {name: {last_sync, last_attempt, state, error}}}`. `last_sync` is
  the last successful sync completion time and is reloaded at startup.
- **Download staging:** `photos_dir/.sync-tmp/<index>/<safe_name>`.

### 3.5 Dependencies and System Requirements

**Rust crates:** `notify` (inotify), `serde`/`serde_json`/`toml` (config,
cursors, status), `log`, `crc32fast`, `signal-hook`, `chrono` (date folders),
`flate2` (log compression), `libc` (flock, `kill`, errno), `rand` (passwords),
`reqwest` blocking with `json` + `native-tls` (HTTP), `qrcode`, `image` (JPEG
only), `fontdue` (screen text), `tiny_http_fork = 0.12.8` (admin HTTP;
CVE-fixed fork pinned for security), `openssl` (RS256 JWT), `base64` (JWT
segments), `askama` (templates), `serde_urlencoded`/`form_urlencoded` (forms).
`tempfile` is a dev-dependency.

**Build requirements:**
- Rust stable toolchain.
- `libssl-dev` and `pkg-config` for `native-tls`.
- `curl` and `unzip` to download and extract the embedded DejaVu font
  (`make font`).
- C build: `gcc`, `libdrm-dev`, `libegl1-mesa-dev`, `libgbm-dev`.

**Runtime requirements:**
- ImageMagick (`magick`, or `convert`). HEIC/HEIF imports also need the
  appropriate ImageMagick delegate.
- `libdrm2`, `libegl1`, `libgbm1` for the C app.
- A USB auto-mount solution that mounts under `/media` (`usbmount`,
  `dietpi-drive_manager`, `udisks2`, or manual `fstab`).
- Optional `avahi-daemon` for the `photo-frame.local` URL.
- `hostname` (util-linux) for LAN IP detection.

---

## 4. Known Limitations

- **Google service accounts cannot own files.** Create the folder with a normal
  Google account and share it with the service account as Viewer. A service
  account has no storage quota of its own.
- **No HTTPS admin UI yet.** Config mode is plain HTTP on the LAN. HTTPS (for
  example Let's Encrypt DNS-01 or a TLS-terminating tunnel) is future work in
  `plans/SYNC_SOURCES.md`.
- **No web OAuth callback.** It requires an HTTPS redirect URI, so it depends on
  the HTTPS work above. The OAuth refresh-token triple is config-file only.
- **Device flow is rejected.** Google restricts it to `drive.appdata` and
  `drive.file`, which cannot read an existing folder of photos.
- **Google Drive lists one folder level.** Subfolders are not traversed; Dropbox
  sync is recursive.
- **Opaque cursors rely on the status file for scheduling.** Dropbox and
  bearer-auth Google Drive cursors carry no timestamp, so their last successful
  sync time comes from `sync-status.json`; deleting that file makes those
  sources look never-synced and sync immediately. API-key Google Drive sources
  still use a timestamp cursor, where a file modified in the same second as the
  cursor (or with clock skew) is covered by the 2-second overlap but may be
  re-listed. Unknown source types are skipped by the scheduler.
- **Config mode is USB-triggered** after the first setup; there is no on-screen
  menu or button. It also needs the display app's control socket and retries
  every 2 seconds if the app is not ready.
- **First-sync progress is not reported on screen.** The screen is static until
  photos appear; per-source status is available from `/api/status`.
- **Imports are serialized.** A very large USB drive or remote batch blocks
  other imports until each photo finishes.
- **Only `remote_sources` can change without a restart.** The scheduler
  hot-reloads config, but display, USB, and config-mode threads keep the values
  they started with; changing photos directory, sockets, or resolution requires
  a restart.

---

## 5. DietPi Setup Notes

### 5.1 Preventing RootFS Auto-Expansion

1. Flash the DietPi image.
2. Mount the ext4 root partition on another machine.
3. Remove
   `<mount>/etc/systemd/system/local-fs.target.wants/dietpi-fs_partition_resize.service`.
4. Unmount and boot.

### 5.2 Creating the Photos Partition

After first boot, as root:

```bash
parted /dev/mmcblk0
# (parted) mkpart primary ext4 <start> 100%
# (parted) quit
mkfs.ext4 /dev/mmcblk0p3
mkdir -p /var/lib/photo-frame/photos
mount /dev/mmcblk0p3 /var/lib/photo-frame/photos

# /etc/fstab:
# PARTUUID=<uuid> /var/lib/photo-frame/photos ext4 noatime,lazytime 0 2
```

### 5.3 Required Packages

```bash
apt update
apt install -y imagemagick
```

### 5.4 USB Auto-Mount

Any solution that mounts drives under `/media` works:

- **Debian/Raspberry Pi OS:** `usbmount` (`/media/usb0`, `/media/usb1`, ...)
- **DietPi:** enable auto-mount in `dietpi-drive_manager`
- **Manual:** `/etc/fstab` entries or systemd mount units

---

*End of Specification Document*
