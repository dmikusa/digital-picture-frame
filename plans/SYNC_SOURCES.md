# Photo Frame Manager — Remote Sync Sources Plan

## Overview

Add the ability to pull photos from remote services (Dropbox, Google Drive) on
a schedule. A USB-triggered configuration mode with a web admin UI handles
setup without requiring SSH. The C display app gains a control socket for pausing
the slideshow to show a QR code on screen.

## Architecture

```
 ┌─────────────────────────────────────────────┐
 │                 config.toml                  │
 │  [[remote_sources]]                          │
 │  type = "dropbox" / "google_drive"           │
 │  ...                                         │
 └─────────────────────────────────────────────┘
                      │
                      ▼
 ┌─────────────────────────────────────────────┐
 │  src/import/remote.rs  (orchestrator)        │
 │  - Loads sources from config                 │
 │  - Scheduler: checks at interval + on boot   │
 │  - Calls trait methods per source            │
 │  - Saves cursor to sync-cursors.json          │
 └─────────────────────────────────────────────┘
          │                    │
          ▼                    ▼
 ┌──────────────┐    ┌──────────────┐
 │ dropbox.rs   │    │ gdrive.rs    │  (each impls RemotePhotoSource trait)
 └──────────────┘    └──────────────┘
          │                    │
          ▼                    ▼
 ┌─────────────────────────────────────────────┐
 │  Existing import pipeline:                  │
 │  hash → dedup → ImageMagick → index append  │
 └─────────────────────────────────────────────┘
```

### Config Mode Flow

```
startup ──photos exist──────────────────────────────▶ NORMAL (slideshow)
        ──no photos + remote sources───────────────▶ FIRST_SYNC
        ──no photos + no remote sources────────────▶ CONFIG_MODE

NORMAL ──USB insert──▶ IMPORTING ──import done──▶ CONFIG_MODE
                          │
                          └──USB removed──▶ NORMAL or FIRST_SYNC

CONFIG_MODE ──user finishes (POST /finish)──▶ validate, save, resume show
            ──USB removed / 10-min timeout──▶ discard staged, resume show
            ──USB insert (re-entry)─────────▶ stop old server, IMPORTING
```

- **First boot** (no photos AND no `[[remote_sources]]` in config): enter
  CONFIG_MODE directly so the frame can show its setup QR.
- **No photos but remote sources configured**: show the "first remote sync in
  progress" status screen instead of the QR. Poll the index and resume the
  slideshow as soon as photos appear.
- **USB insert while already in config mode**: stop the existing admin server
  before running the import and starting a new one. A new session gets a new
  password and port; the old listener must not leak.
- **Display app not ready**: config-mode entry is retried after a short delay
  (`PendingConfigMode`) instead of terminating the config-mode thread.
- **10-minute timeout** resets on every HTTP request (not from server start).
- **USB removal** instantly stops the server and discards staged config. No grace
  period.
- **Config changes are staged** in memory until the user clicks "Finish", at
  which point they are validated and atomically written to `config.toml` (write
  temp file + `rename()`). This prevents partial config on USB removal.
- **Existing sources are seeded into staging**, so they can be edited or
  removed. Saving an empty list is allowed and removes every remote source.

---

## Phase 1: C Display Control Socket

### Task 1.1: Control Socket Protocol
- Add a second Unix domain socket listener in `photo-frame-display.c` at
  `CONTROL_SOCKET_PATH` (env var, default `/run/photo-frame/control.sock`).
- Add to the existing epoll set alongside `drm_fd`, `listen_fd`, and `conn_fd`.
  No threading changes — the C app is single-threaded with an epoll event loop.
- Protocol (line-delimited):
  ```
  CLR\n       — Clear render queue, pause slideshow (don't advance)
  SHOW <path>\n — Load and display JPEG at path, hold indefinitely
  RESUME\n    — Resume normal slideshow operation
  ```
- On `CLR`: free pending buffers in the 2-slot pipeline, set `paused = true`.
- On `SHOW`: load image via stb_image, render immediately, hold until `RESUME`
  or another `CLR`+`SHOW`. Overwrites current display.
- On `RESUME`: clear the held image, set `paused = false`, resume advancing
  through the queue.
- Idempotent: repeated `CLR`/`CLR` or `RESUME`/`RESUME` is harmless.

### Task 1.2: Rust Control Client
- New `ControlClient` struct in `src/control.rs`.
- Methods:
  - `new(socket_path: &Path) -> Self`
  - `clear(&mut self)` — sends `CLR\n`
  - `show(&mut self, path: &Path)` — sends `SHOW <path>\n`
  - `resume(&mut self)` — sends `RESUME\n`
- Same connection/retry pattern as `DisplayClient` (reconnect on send failure).
- Unit tests: mock Unix socket server, verify each command received as raw text.

### Task 1.3: Packaging
- Add `CONTROL_SOCKET_PATH=/run/photo-frame/control.sock` to `display.env`.
- Update systemd unit to pass the env var.
- No Makefile changes needed (single source compiles already).

---

## Phase 2: QR Code Generation + Config Mode Display

### Task 2.1: QR Code as JPEG
- New `src/qr.rs` module.
- Dependencies: `qrcode` (QR encoding), `image` (PNG render → JPEG encode).
- `pub fn generate_qr_jpeg(url: &str, width: u32, height: u32) -> Result<Vec<u8>>`
- Output: JPEG bytes at configured native resolution so it fills the screen
  without scaling in the C app.
- Unit test: generate a QR for a known URL, verify valid JPEG output.

### Task 2.2: Config Mode Screen Renderer
- New `src/config_mode.rs` module.
- `pub fn render_config_screen(config, fallback_ip, mdns_host, port, password) -> Result<PathBuf>`
- Generates a QR code JPEG with:
  - QR code: `http://<lan-ip>:<port>` (basic auth embedded in URL), so setup
    works without mDNS
  - Below QR: LAN URL, 8-character password, and the optional
    `http://photo-frame.local:<port>` URL
  - Footer: "Insert USB to configure (or remove to cancel)"
- Also provides `render_status_screen(config, title, lines)` for the importing
  and first-sync screens (written to a distinct path).
- Writes to `/tmp/photo-frame-qr.jpg` (or `/tmp/photo-frame-status.jpg`),
  returns the path.
- Uses `ControlClient` to send `CLR` + `SHOW`.

### Task 2.3: Password Generation
- 8 characters from `[a-zA-Z0-9]` (62^8 = 218 trillion combinations).
- Generated fresh each time the admin server starts — rotating.
- Displayed on the config screen beneath the QR for manual entry fallback.
- Logged to system log for SSH access as additional fallback.

---

## Phase 3: Config Mode State Machine

### Task 3.1: State Machine
- New `src/config_mode.rs` (extends Phase 2).
- States: `Normal`, `FirstSync`, `Importing { pending }`, `PendingConfigMode`,
  and `ConfigMode`. Transitions as described in the architecture section above.
- `initial_state(valid_count, has_remote_sources)` is a pure helper (unit
  tested) that chooses the startup screen.
- IMPORTING state: runs the existing USB import and shows an "Importing
  photos..." status screen.
- FIRST_SYNC state: shows the first-sync status screen and polls the index every
  couple of seconds; when photos appear it resumes the slideshow. A USB insert
  switches to IMPORTING.
- CONFIG_MODE state: renders the QR, starts the admin HTTP server, and starts
  the 10-minute idle timer (reset on each HTTP request).
- `PendingConfigMode` retries entry when the display app's control socket is not
  ready yet. The config-mode thread survives these failures.
- Entering config mode always stops any running admin server first, including
  when a USB is inserted during an existing session.
- The timer is checked in the main loop with `Receiver::recv_timeout`.

### Task 3.2: Staged Config
- The admin server is seeded with the current `config.remote_sources`, so
  existing entries can be edited and deleted. An empty list is a valid result.
- Admin UI writes changes into the in-memory staged list.
- On `POST /finish`:
  - Validate staged sources:
    - every name is non-empty and unique;
    - Dropbox requires `access_token`;
    - Google Drive requires `folder_id` plus either `api_key` or a complete
      `refresh_token` + `client_id` + `client_secret` set;
    - an unfinished `oauth_pending` entry is rejected;
    - `check_interval_seconds` must be greater than 0.
  - Return an HTML error page for browser/form clients and a JSON error object
    for API clients. A validation failure keeps the server running.
  - Atomically write to the real `config.toml`: write to `config.toml.tmp`,
    then `fs::rename()`.
  - Update the in-memory config so a later config-mode session sees the new
    sources.
- If new `[[remote_sources]]` are added, the first sync scheduler pass picks
  them up. (The running scheduler is not hot-reloaded in Phase 3; a restart is
  required for an already-running process.)
- On USB removal or idle timeout: discard staged config, stop server. No disk
  writes.

### Task 3.3: First Sync Status Screen
- If photos exist: background sync — no screen interruption.
- If no photos exist and remote sources are configured: show "First remote sync
  in progress" on screen via the control socket. Resume the normal display when
  photos appear in the index. Progress reporting is deferred (see
  `/api/status`).

---

## Phase 4: Remote Source Trait + Scheduler

### Task 4.1: Module Reorganization
- Rename `src/import.rs` to `src/import/usb.rs`.
- New `src/import/mod.rs` — re-exports USB + remote, shared helpers.
- New `src/import/remote.rs` — trait definition, scheduler, cursor persistence.

### Task 4.2: Trait Definition
```rust
pub trait RemotePhotoSource: Send {
    fn connect(config: &HashMap<String, String>) -> Result<Self>
    where
        Self: Sized;

    fn list_changes(
        &mut self,
        cursor: Option<&str>,
    ) -> Result<(Vec<RemotePhoto>, String)>;

    fn download(&mut self, remote_id: &str, dest: &Path) -> Result<()>;
}

pub struct RemotePhoto {
    pub remote_id: String,    // opaque source-specific ID
    pub filename: String,     // original filename
    pub size_bytes: Option<u64>,
}
```
- `list_changes` returns photos + opaque cursor string for next call.
- Cursor format is source-specific (ISO 8601 timestamp, page token, opaque).
- `download` writes raw bytes to `dest` path.

### Task 4.3: Scheduler
- A `std::thread` with a loop:
  1. Load `[[remote_sources]]` from config.
  2. For each source: read last cursor from `{photos_dir}/sync-cursors.json`.
  3. If `check_interval_seconds` has elapsed since last sync (or never synced):
     - Call `list_changes(cursor)`.
     - For each new photo: `download()` to temp file.
     - Run through existing `import_single_photo()` pipeline (hash → dedup →
       ImageMagick → index append).
     - Save new cursor.
  4. After all sources checked, sleep until next due or shutdown signal.
- Startup sync: if any source has never synced, triggers a first sync. Shows
  status screen if no photos exist yet (see Task 3.3).
- Cursor file format:
  ```json
  {"My Dropbox": "2024-01-15T12:00:00Z", "Drive": "cursor_abc123"}
  ```
- Dependency: `serde_json` (add to `Cargo.toml`).

### Task 4.4: Config Schema
```toml
[[remote_sources]]
type = "dropbox"
name = "My Dropbox"
access_token = "sl.xxxx"
folder = "/Photos"
check_interval_seconds = 86400

[[remote_sources]]
type = "google_drive"
name = "Google Drive"
api_key = "AIza..."
folder_id = "1abc123..."
check_interval_seconds = 86400

# OAuth fields for private folders (Phase 8):
# [[remote_sources]]
# type = "google_drive"
# name = "Private Drive"
# client_id = "..."
# client_secret = "..."
# refresh_token = "1//xxxx"
# folder_id = "1abc123..."
```
- `type` dispatches to the appropriate source implementation.
- `name` used in cursor file and admin UI.
- `check_interval_seconds` defaults to 86400 (24h) if omitted.

---

## Phase 5: Dropbox Source

### Task 5.1: Implementation
- New `src/import/dropbox.rs` implementing `RemotePhotoSource`.
- Uses raw HTTP via `reqwest` (blocking client). No `dropbox-sdk` crate —
  only 2 endpoints needed:
  - `POST https://api.dropboxapi.com/2/files/list_folder` with
    `Dropbox-API-Arg` header for pagination.
  - `POST https://content.dropboxapi.com/2/files/download` with
    `Dropbox-API-Arg` header.
- `connect()`: validates the access token by calling `list_folder` with
  `limit=1`.
- `list_changes()`:
  - First call: `list_folder` with `recursive=true`.
  - Subsequent: uses `list_folder/continue` with cursor from prior call.
  - Compares `server_modified` against last sync timestamp to detect new files.
  - Returns `RemotePhoto`s + cursor (ISO 8601 of latest `server_modified`).
- `download()`: calls `files/download`, streams response body to file.
- Config fields: `access_token`, `folder`.
- Unit tests: mock the HTTP layer, verify list/download behavior.

---

## Phase 6: Google Drive Source (API Key — Public Folders)

### Task 6.1: Implementation
- New `src/import/google_drive.rs` implementing `RemotePhotoSource`.
- Uses raw HTTP via `reqwest` (blocking client). `api_key` passed as
  `?key=<api_key>` query parameter — no OAuth required.
- `connect()`: validates the API key and folder by listing files with `limit=1`.
- `list_changes()`:
  - Lists files with `q='<folder_id>'+in+parents+and+mimeType+contains+'image/'`.
  - Uses `modifiedTime` for filtering. Cursor is ISO 8601 of latest.
  - Pagination via `pageToken`.
- `download()`: `GET https://www.googleapis.com/drive/v3/files/<id>?alt=media`,
  streams to file.
- Config fields: `api_key`, `folder_id`.

### Task 6.2: Setup Documentation
- New `docs/google-drive-setup.md`:
  - Step-by-step: create Google Cloud project, enable Drive API, create API key
    (restrict to Drive API), share a Drive folder as "Anyone with the link can
    view", copy folder ID, enter both in admin UI.
  - Screenshots at key steps.

---

## Phase 7: Admin UI (DIY HTTP Server)

### Task 7.1: HTTP Server
- New `src/admin_server.rs` module.
- Uses `tiny_http_fork = "=0.12.8"` (pinned) instead of the DIY `TcpListener`
  + `httparse` approach originally considered.
  - **Rationale:** The DIY server was chosen to avoid dependency bloat, but
    after research the team identified `tiny_http_fork` as a drop-in
    replacement for the original (unmaintained) `tiny_http`. The fork fixes
    CVE-2026-66752 (Transfer-Encoding smuggling), CVE-2026-66753 (CR/LF header
    injection), and several panic-on-drop / ECONNABORTED bugs from the
    original. A full diff review against upstream confirmed no malicious code —
    all changes are from publicly-tracked PRs (#275, #284, #285) or CVE
    mitigations.
  - **Dependency tree:** `ascii`, `chunked_transfer`, `httpdate`, `log` (4
    deps, matching the original `tiny_http` footprint).
- Starts on a random port in range 8100–8199 during CONFIG_MODE state.
- `AdminServer::new(password, staged_sources)` seeds staging from
  `config.remote_sources`; `.with_status_path(...)` points the status endpoint at
  `<photos_dir>/sync-status.json`.
- The setup QR and primary URL use the detected LAN IP, so phones on the same
  network do not need mDNS. `photo-frame.local:<port>` is shown as an optional
  alternative.
- Stops on: `POST /finish`, USB removal, 10-minute idle timeout (reset on
  each request). Re-entering config mode stops any previous server first.
- All routes except `/oauth/google/callback` behind HTTP Basic Auth with the
  rotating 8-char password.
- Basic auth guard: pre-compute the expected `Basic <base64>` string once at
  server start, then string-compare on each request. No `base64` crate needed.

### Task 7.2: API Routes (JSON, REST)

| Route | Method | Auth | Description |
|-------|--------|------|-------------|
| `/api/sources` | GET | Yes | List all staged sources as JSON |
| `/api/sources/dropbox` | POST | Yes | Save Dropbox config to staging. JSON request returns JSON; form request returns a 303 redirect. |
| `/api/sources/google-drive` | POST | Yes | Save Google Drive config to staging. JSON request returns JSON; form request returns a 303 redirect. |
| `/api/sources/<name>` | DELETE / POST | Yes | Remove a source from staging. DELETE returns JSON; form POST returns a 303 redirect. |
| `/api/status` | GET | Yes | Staged sources plus the parsed contents of `<photos_dir>/sync-status.json` under `sync` (empty object if missing/unreadable). Returns JSON. |
| `/api/finish` | POST | Yes | Validate staged config, write atomically, stop server. JSON request returns JSON; form request returns an HTML success page. Invalid sources return an HTML error page for browsers or a JSON error object for API clients (400). |

### Task 7.3: HTML Pages (HTMX)

| Route | Method | Auth | Description |
|-------|--------|------|-------------|
| `/` | GET | Yes | Dashboard: menu bar + HTMX-loaded status card from `/api/status` |
| `/sources` | GET | Yes | Source list: menu bar + HTMX-loaded source table from `/api/sources` |
| `/sources/add` | GET | Yes | Menu bar + source type picker (links to `/sources/dropbox` and `/sources/google-drive`) |
| `/sources/dropbox` | GET | Yes | Menu bar + form (submits via plain HTML POST to `/api/sources/dropbox`) |
| `/sources/google-drive` | GET | Yes | Menu bar + form (submits via plain HTML POST to `/api/sources/google-drive`) |
| `/sources/google-drive-oauth` | GET | Yes | Menu bar + OAuth setup form (Phase 8) |
| `/oauth/google/callback` | GET | **No** | Handles Google OAuth redirect (Phase 8) |

### Task 7.4: Page Layout

Every HTML page shares the same structure:

```
┌─────────────────────────┐
│  Home │ Sources │ Add   │  ← menu bar, active page highlighted
├─────────────────────────┤
│                         │
│  [page content here]    │  ← HTMX-loaded or server-rendered
│                         │
└─────────────────────────┘
```

- Menu bar across the top: `Home` (dashboard), `Sources` (list), `Add` (add new).
- Active page highlighted in the menu bar.
- Source cards are rendered server-side; no client-side JavaScript is required.
- Forms use plain HTML `method="post"` to `/api/*` endpoints. Browser form
  submissions receive a `303 See Other` redirect to the next page.
- Phone-friendly: viewport meta tag, no horizontal scroll, stacked layout.
- No external CDN dependency and no JavaScript.

**Add a Dropbox source flow:**

1. User navigates to `/sources/dropbox` → server returns menu bar + form HTML.
2. User fills in access token, folder, interval.
3. User clicks "Save" → browser POSTs form data to `/api/sources/dropbox`.
4. Server validates, saves to staging, responds `303` with `Location: /sources`.
5. Browser loads `/sources`, which lists the staged sources. A "Finish" button
   at the bottom POSTs to `/api/finish` to deploy the config.

---

## Phase 8: Google Drive OAuth (Private Folders — Deferred)

### Task 8.1: OAuth Flow
- Extends Google Drive source to support `refresh_token` + `client_id` +
  `client_secret` in addition to `api_key`.
- Admin UI: `/sources/google-drive-oauth` with OAuth setup form and
  "Connect Google Drive" button.
- User must create their own Google Cloud project (documented in
  `docs/google-drive-setup.md`).
- Button redirects user's browser to Google's OAuth consent screen.
- Callback: `/oauth/google/callback?code=...` (no basic auth on this route).
- Server exchanges code for refresh token, stores in staging config.
- Dependencies added: `oauth2` crate.

**Deferred:** This is the most complex phase. The API key path (Phase 6)
handles the common case (public folders) with zero OAuth complexity.

---

## Dependencies Added

| Crate | Phase | Purpose |
|-------|-------|---------|
| `qrcode` | 2 | QR code generation |
| `image` | 2 | QR PNG → JPEG encode, screen-size rendering |
| `serde_json` | 4 | Cursor file format, config serialization |
| `reqwest` (blocking) | 5, 6 | HTTP client for Dropbox, Google Drive |
| `httparse` | — (replaced by tiny_http_fork) | — |
| `tiny_http_fork` | 7 | HTTP server (CVE-fixed fork of tiny_http) |
| `oauth2` | 8 (deferred) | Google Drive OAuth client/refresh flow |

---

## Task Summary Table

| Task | Description | Est. Effort |
|------|-------------|-------------|
| 1.1 | Control socket protocol in C | Medium |
| 1.2 | Rust `ControlClient` + tests | Small |
| 1.3 | Packaging (env var, systemd) | Small |
| 2.1 | QR code JPEG generation + tests | Small |
| 2.2 | Config mode screen render | Small |
| 2.3 | Password generation + basic auth | Small |
| 3.1 | State machine (NORMAL/FIRST_SYNC/IMPORT/CONFIG + retry) | Medium |
| 3.2 | Staged config (seeded), validation, atomic deploy | Small |
| 3.3 | First sync status screen + index polling | Small |
| 4.1 | Module reorg (`src/import/`) | Small |
| 4.2 | `RemotePhotoSource` trait | Small |
| 4.3 | Scheduler + cursor persistence | Medium |
| 4.4 | Config schema (`[[remote_sources]]`) | Small |
| 5.1 | Dropbox source + tests | Medium |
| 6.1 | Google Drive source (API key) + tests | Medium |
| 6.2 | Google Drive setup docs | Small |
| 7.1 | DIY HTTP server (TcpListener + httparse) | Medium |
| 7.2 | API routes (JSON, REST) | Medium |
| 7.3 | HTML pages (HTMX, menu bar layout) | Medium |
| 7.4 | Page layout + HTMX wiring | Small |
| 8.1 | Google Drive OAuth (deferred) | Large |
| 8.2 | OAuth setup documentation | Small |

---

*End of Plan*
