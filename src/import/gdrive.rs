// Photo Frame Manager — DRM/GBM/EGL digital photo frame.
// Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::import::http;
use crate::import::remote::{RemotePhoto, RemotePhotoSource};
use base64::Engine;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::Signer;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
/// Ask for the page token alongside the file fields; without it the caller
/// cannot tell that more pages exist.
const FIELDS: &str = "nextPageToken,files(id,name,size,mimeType,modifiedTime)";
/// Fields for a `changes/list` call. `newStartPageToken` is only present on the
/// last page; `fileId`/`removed` let us skip deletions, and `parents` lets us
/// keep only changes inside the configured folder.
const CHANGES_FIELDS: &str = "nextPageToken,newStartPageToken,\
changes(fileId,removed,file(id,name,size,mimeType,modifiedTime,parents))";
const PAGE_SIZE: usize = 1_000;
/// Safety valve: the server controls `nextPageToken`, so cap the number of
/// pages one sync check will follow.
const MAX_PAGES: usize = 1_000;
/// New cursors are page tokens from the Drive Changes API. The prefix keeps
/// them distinct from the legacy numeric (Unix seconds) cursors.
const GDRIVE_CURSOR_PREFIX: &str = "gdrive:";
/// API-key sources cannot call the Changes API, so they keep the timestamp
/// listing. Re-list a short window before the cursor so a file modified in the
/// same second as the cursor is not skipped; duplicates are deduplicated.
const API_KEY_OVERLAP_SECS: u64 = 2;
/// Service-account assertions are valid for an hour; refresh a little early so
/// a token cannot expire in flight.
const ASSERTION_LIFETIME_SECS: u64 = 3_600;
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);
const JWT_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

enum AuthMode {
    ApiKey(String),
    Bearer(BearerAuth),
}

enum BearerAuth {
    /// An OAuth refresh token was exchanged for an access token in `connect`.
    RefreshToken { access_token: String },
    ServiceAccount {
        key: ServiceAccountKey,
        token: Option<CachedToken>,
    },
}

struct CachedToken {
    access_token: String,
    refresh_at: Instant,
}

/// The subset of a Google service-account JSON key that the frame needs. The
/// key itself is sensitive; this type never formats or logs it.
#[derive(Deserialize)]
pub struct ServiceAccountKey {
    #[serde(default)]
    client_email: String,
    #[serde(default)]
    private_key: String,
    #[serde(default)]
    token_uri: Option<String>,
}

impl ServiceAccountKey {
    /// Parse and sanity-check a service-account JSON key.
    ///
    /// Errors name the missing field but never echo the key material.
    pub fn parse(json: &str) -> Result<Self, String> {
        let key: ServiceAccountKey = serde_json::from_str(json)
            .map_err(|e| format!("service account JSON is not valid JSON: {e}"))?;
        if key.client_email.trim().is_empty() {
            return Err("service account JSON is missing 'client_email'".into());
        }
        if key.private_key.trim().is_empty() {
            return Err("service account JSON is missing 'private_key'".into());
        }
        if let Some(uri) = key.token_uri.as_deref() {
            let uri = uri.trim();
            if !uri.is_empty() && !uri.starts_with("https://") && !uri.starts_with("http://") {
                return Err("service account 'token_uri' must be an http(s) URL".into());
            }
        }
        Ok(key)
    }

    fn token_uri(&self) -> &str {
        self.token_uri
            .as_deref()
            .map(str::trim)
            .filter(|uri| !uri.is_empty())
            .unwrap_or(DEFAULT_TOKEN_URI)
    }

    /// Build a signed RS256 JWT for the OAuth2 JWT-bearer grant.
    fn build_assertion(&self, issued_at: u64) -> io::Result<String> {
        let header = r#"{"alg":"RS256","typ":"JWT"}"#;
        let claims = serde_json::json!({
            "iss": self.client_email.trim(),
            "scope": DRIVE_SCOPE,
            "aud": self.token_uri(),
            "iat": issued_at,
            "exp": issued_at + ASSERTION_LIFETIME_SECS,
        });

        let signing_input = format!(
            "{}.{}",
            base64url(header.as_bytes()),
            base64url(claims.to_string().as_bytes())
        );

        let pkey = PKey::private_key_from_pem(self.private_key.trim().as_bytes())
            .map_err(|e| io::Error::other(format!("Invalid service account private key: {e}")))?;
        let mut signer = Signer::new(MessageDigest::sha256(), &pkey)
            .map_err(|e| io::Error::other(format!("Could not initialize JWT signer: {e}")))?;
        signer
            .update(signing_input.as_bytes())
            .map_err(|e| io::Error::other(format!("Could not sign service account JWT: {e}")))?;
        let signature = signer
            .sign_to_vec()
            .map_err(|e| io::Error::other(format!("Could not sign service account JWT: {e}")))?;

        Ok(format!("{signing_input}.{}", base64url(&signature)))
    }

    /// Exchange a signed assertion for an access token.
    fn fetch_access_token(&self, client: &Client) -> io::Result<CachedToken> {
        let assertion = self.build_assertion(unix_now())?;
        let request = client
            .post(self.token_uri())
            .form(&[("grant_type", JWT_GRANT), ("assertion", assertion.as_str())]);
        let resp = send_request(request)?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Google's error body explains the failure (bad key, missing
            // permission). Truncate it so a hostile server cannot flood logs.
            let detail: String = resp.text().unwrap_or_default().chars().take(300).collect();
            return Err(io::Error::other(format!(
                "Google service account token request failed: {status} {detail}"
            )));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            expires_in: u64,
        }

        let token: TokenResponse = resp.json().map_err(io::Error::other)?;
        let lifetime = Duration::from_secs(if token.expires_in == 0 {
            ASSERTION_LIFETIME_SECS
        } else {
            token.expires_in
        });
        // Never refresh later than `lifetime - margin`; for very short-lived
        // test tokens use half the lifetime so there is still a usable window.
        let margin = TOKEN_REFRESH_MARGIN.min(lifetime / 2);
        Ok(CachedToken {
            access_token: token.access_token,
            refresh_at: Instant::now() + lifetime.saturating_sub(margin),
        })
    }
}

pub struct GoogleDriveSource {
    client: Client,
    auth: AuthMode,
    folder_id: String,
    api_base: String,
}

impl GoogleDriveSource {
    /// Constructor with an injectable API root, used by unit tests.
    #[cfg(test)]
    pub(crate) fn with_api_key(api_key: String, folder_id: String, api_base: String) -> Self {
        GoogleDriveSource {
            client: http::build_client().expect("test HTTP client"),
            auth: AuthMode::ApiKey(api_key),
            folder_id,
            api_base,
        }
    }

    /// Bearer-auth constructor with an injectable API root, used by unit tests
    /// that exercise the Changes API without a token exchange.
    #[cfg(test)]
    pub(crate) fn with_access_token(
        access_token: String,
        folder_id: String,
        api_base: String,
    ) -> Self {
        GoogleDriveSource {
            client: http::build_client().expect("test HTTP client"),
            auth: AuthMode::Bearer(BearerAuth::RefreshToken { access_token }),
            folder_id,
            api_base,
        }
    }

    /// Resolve credentials from source params and open the API root. Tests use
    /// this with a mock root so the whole connect path is exercised.
    pub(crate) fn connect_to(config: &HashMap<String, String>, api_base: &str) -> io::Result<Self> {
        let folder_id = config
            .get("folder_id")
            .cloned()
            .ok_or_else(|| io::Error::other("Missing 'folder_id' for Google Drive source"))?;

        let auth = if let Some(json) = config
            .get("service_account_json")
            .filter(|json| !json.trim().is_empty())
        {
            let key = ServiceAccountKey::parse(json).map_err(io::Error::other)?;
            AuthMode::Bearer(BearerAuth::ServiceAccount { key, token: None })
        } else if let Some(refresh_token) = config.get("refresh_token").cloned() {
            let client_id = config
                .get("client_id")
                .cloned()
                .ok_or_else(|| io::Error::other("Missing 'client_id' for OAuth"))?;
            let client_secret = config
                .get("client_secret")
                .cloned()
                .ok_or_else(|| io::Error::other("Missing 'client_secret' for OAuth"))?;
            let access_token = refresh_access_token(&refresh_token, &client_id, &client_secret)?;
            AuthMode::Bearer(BearerAuth::RefreshToken { access_token })
        } else if let Some(api_key) = config.get("api_key").cloned() {
            AuthMode::ApiKey(api_key)
        } else {
            return Err(io::Error::other(
                "Google Drive source requires 'api_key', 'service_account_json', \
                 or 'refresh_token'+'client_id'+'client_secret'",
            ));
        };

        let client = http::build_client()?;
        Self::open(client, auth, folder_id, api_base.to_string())
    }

    /// Build a source and validate that the folder is reachable.
    fn open(
        client: Client,
        auth: AuthMode,
        folder_id: String,
        api_base: String,
    ) -> io::Result<Self> {
        let mut source = GoogleDriveSource {
            client,
            auth,
            folder_id,
            api_base,
        };

        // Validate connectivity. This also fetches the first service-account
        // token, so a bad key fails at startup instead of during a sync.
        let auth_param = source.auth_param();
        let url = format!(
            "{}/files?q='{}'+in+parents&pageSize=1&{auth_param}",
            source.api_base, source.folder_id
        );
        let resp = source.make_get(&url)?;
        if !resp.status().is_success() {
            let hint = if source.uses_service_account() {
                " — ensure the folder is shared with the service account's email as Viewer"
            } else {
                " — ensure folder access is granted"
            };
            return Err(io::Error::other(format!(
                "Google Drive validation failed: {}{hint}",
                resp.status()
            )));
        }

        Ok(source)
    }

    /// The `Authorization` header for this request, fetching or refreshing a
    /// service-account token when needed. `None` means the credential already
    /// travels in the URL (`?key=`).
    fn authorization(&mut self) -> io::Result<Option<String>> {
        let client = &self.client;
        match &mut self.auth {
            AuthMode::ApiKey(_) => Ok(None),
            AuthMode::Bearer(BearerAuth::RefreshToken { access_token }) => {
                Ok(Some(bearer(access_token)))
            }
            AuthMode::Bearer(BearerAuth::ServiceAccount { key, token }) => {
                let valid = token
                    .as_ref()
                    .map(|t| t.refresh_at > Instant::now())
                    .unwrap_or(false);
                if !valid {
                    *token = Some(key.fetch_access_token(client)?);
                }
                let token = token.as_ref().expect("token was just refreshed");
                Ok(Some(bearer(&token.access_token)))
            }
        }
    }

    fn make_get(&mut self, url: &str) -> io::Result<reqwest::blocking::Response> {
        let authorization = self.authorization()?;
        let mut req = self.client.get(url);
        if let Some(value) = authorization {
            req = req.header(AUTHORIZATION, value);
        }
        send_request(req)
    }

    fn auth_param(&self) -> String {
        match &self.auth {
            AuthMode::ApiKey(key) => format!("key={key}"),
            AuthMode::Bearer(_) => String::new(),
        }
    }

    fn uses_service_account(&self) -> bool {
        matches!(
            &self.auth,
            AuthMode::Bearer(BearerAuth::ServiceAccount { .. })
        )
    }

    /// API keys can list and download public files, but the Changes API needs
    /// user or service-account auth, so an API-key source stays on the
    /// timestamp listing.
    fn uses_api_key(&self) -> bool {
        matches!(&self.auth, AuthMode::ApiKey(_))
    }

    /// List the image files in the configured folder, newest first.
    ///
    /// `last_sync` is the timestamp cursor to resume from and `overlap` re-lists
    /// a short window before it. Returns the photos and the newest modification
    /// time seen, which is never older than `last_sync`, so the cursor cannot
    /// move backwards.
    fn list_folder_photos(
        &mut self,
        last_sync: u64,
        overlap: u64,
    ) -> io::Result<(Vec<RemotePhoto>, u64)> {
        let threshold = last_sync.saturating_sub(overlap);

        let query = format!(
            "'{}' in parents and mimeType contains 'image/' and trashed = false",
            self.folder_id
        );

        let mut latest = last_sync;
        let mut photos = Vec::new();
        let mut page_token: Option<String> = None;

        // `nextPageToken` pages through the listing; without this loop only
        // the first page (default 100 files) is ever imported.
        let mut pages = 0usize;
        loop {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(io::Error::other(format!(
                    "Google Drive listing exceeded {MAX_PAGES} pages"
                )));
            }
            let mut url = format!(
                "{}/files?q={}&orderBy=modifiedTime&pageSize={}&fields={}&{}",
                self.api_base,
                urlencoding(&query),
                PAGE_SIZE,
                FIELDS,
                self.auth_param()
            );
            if let Some(token) = &page_token {
                url.push_str("&pageToken=");
                url.push_str(&urlencoding(token));
            }

            let resp = self.make_get(&url)?;

            if !resp.status().is_success() {
                return Err(io::Error::other(format!(
                    "Google Drive list failed: {}",
                    resp.status()
                )));
            }

            let result: FileList = resp.json().map_err(io::Error::other)?;
            collect_photos(&result.files, threshold, &mut latest, &mut photos);

            match result.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => break,
            }
        }

        Ok((photos, latest))
    }

    /// Ask Drive for a token representing "now". Every change after this token
    /// is returned by `changes/list`.
    fn start_page_token(&mut self) -> io::Result<String> {
        let url = format!(
            "{}/changes/startPageToken?{}",
            self.api_base,
            self.auth_param()
        );
        let resp = self.make_get(&url)?;

        if !resp.status().is_success() {
            return Err(drive_status_error(
                "Google Drive startPageToken request failed",
                resp.status(),
            ));
        }

        let token: StartPageToken = resp.json().map_err(io::Error::other)?;
        token
            .start_page_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| io::Error::other("Google Drive startPageToken response had no token"))
    }

    /// Fetch changes since `page_token`, following `nextPageToken` until the
    /// final page supplies `newStartPageToken`. Returns the photos and the new
    /// start token.
    fn list_native_changes(&mut self, page_token: &str) -> io::Result<(Vec<RemotePhoto>, String)> {
        let mut photos = Vec::new();
        let mut new_start_token: Option<String> = None;
        let mut next_token: Option<String> = Some(page_token.to_string());

        let mut pages = 0usize;
        while let Some(token) = next_token.take() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(io::Error::other(format!(
                    "Google Drive changes listing exceeded {MAX_PAGES} pages"
                )));
            }

            let url = format!(
                "{}/changes?pageToken={}&pageSize={}&fields={}&{}",
                self.api_base,
                urlencoding(&token),
                PAGE_SIZE,
                CHANGES_FIELDS,
                self.auth_param()
            );
            let resp = self.make_get(&url)?;

            if !resp.status().is_success() {
                return Err(drive_status_error(
                    "Google Drive changes list failed",
                    resp.status(),
                ));
            }

            let result: ChangesList = resp.json().map_err(io::Error::other)?;
            collect_changes(&result.changes, &self.folder_id, &mut photos);

            if let Some(token) = result
                .new_start_page_token
                .filter(|token| !token.is_empty())
            {
                new_start_token = Some(token);
            }
            next_token = result.next_page_token.filter(|token| !token.is_empty());
        }

        let new_start_token = new_start_token.ok_or_else(|| {
            io::Error::other("Google Drive changes listing ended without a newStartPageToken")
        })?;
        Ok((photos, new_start_token))
    }
}

#[derive(Deserialize)]
struct FileList {
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(default)]
    files: Vec<DriveFile>,
}

#[derive(Deserialize)]
struct DriveFile {
    id: String,
    name: String,
    size: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    #[serde(rename = "modifiedTime")]
    modified_time: Option<String>,
    /// Only requested for `changes/list`; the folder listing omits it. Used to
    /// keep only changes inside the configured folder.
    #[serde(default)]
    parents: Option<Vec<String>>,
}

/// `changes/startPageToken` response.
#[derive(Deserialize)]
struct StartPageToken {
    #[serde(rename = "startPageToken")]
    start_page_token: Option<String>,
}

/// `changes/list` response. `newStartPageToken` is only present on the last
/// page of a result set.
#[derive(Deserialize)]
struct ChangesList {
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "newStartPageToken")]
    new_start_page_token: Option<String>,
    #[serde(default)]
    changes: Vec<DriveChange>,
}

/// One entry from `changes/list`. `file` is absent when `removed` is true.
#[derive(Deserialize)]
struct DriveChange {
    #[serde(rename = "fileId")]
    file_id: String,
    #[serde(default)]
    removed: bool,
    file: Option<DriveFile>,
}

impl RemotePhotoSource for GoogleDriveSource {
    fn connect(config: &HashMap<String, String>) -> io::Result<Self> {
        Self::connect_to(config, API_BASE)
    }

    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)> {
        // API keys cannot call the Changes API, so keep the timestamp listing.
        // The overlap covers files modified in the same second as the cursor.
        if self.uses_api_key() {
            let last_sync = cursor.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);
            let (photos, latest) = self.list_folder_photos(last_sync, API_KEY_OVERLAP_SECS)?;
            return Ok((photos, latest.to_string()));
        }

        match cursor {
            Some(cursor) if cursor.starts_with(GDRIVE_CURSOR_PREFIX) => {
                let token = &cursor[GDRIVE_CURSOR_PREFIX.len()..];
                match self.list_native_changes(token) {
                    Ok((photos, new_token)) => Ok((photos, gdrive_cursor(&new_token))),
                    Err(e) if changes_unsupported(&e) => {
                        // Permission or unsupported: fall back to a folder
                        // listing. A page token carries no timestamp, so
                        // re-list the whole folder (duplicates are skipped
                        // downstream) and try to re-baseline on a fresh start
                        // token. If even that is unavailable, keep the old
                        // cursor so no change is ever silently dropped.
                        log::warn!(
                            "Google Drive Changes API unavailable ({e}); \
                             falling back to a folder listing"
                        );
                        let (photos, _latest) = self.list_folder_photos(0, 0)?;
                        match self.start_page_token() {
                            Ok(token) => Ok((photos, gdrive_cursor(&token))),
                            Err(e) => {
                                log::warn!(
                                    "Google Drive startPageToken unavailable ({e}); \
                                     keeping the previous cursor"
                                );
                                Ok((photos, cursor.to_string()))
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            other => {
                // First sync, or a legacy numeric cursor. Take the start token
                // *before* listing so a file added while the folder is being
                // paginated is still returned by the Changes API. Listing it
                // again is harmless because duplicates are dropped downstream.
                let last_sync = other.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);
                match self.start_page_token() {
                    Ok(token) => {
                        let (photos, _latest) = self.list_folder_photos(last_sync, 0)?;
                        Ok((photos, gdrive_cursor(&token)))
                    }
                    Err(e) => {
                        // The Changes API is not available. Fall back to the
                        // old timestamp listing and cursor so the photos still
                        // import; the next sync retries the start token.
                        log::warn!(
                            "Google Drive startPageToken unavailable ({e}); \
                             falling back to the timestamp cursor"
                        );
                        let (photos, latest) = self.list_folder_photos(last_sync, 0)?;
                        Ok((photos, latest.to_string()))
                    }
                }
            }
        }
    }

    fn download(&mut self, remote_id: &str, dest: &Path) -> io::Result<()> {
        let url = format!(
            "{}/files/{}?alt=media&{}",
            self.api_base,
            remote_id,
            self.auth_param()
        );

        let resp = self.make_get(&url)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Google Drive download failed for {remote_id}: {}",
                resp.status()
            )));
        }

        http::download_to_file(resp, dest)
    }
}

fn send_request(request: RequestBuilder) -> io::Result<reqwest::blocking::Response> {
    http::send_with_retry(|| {
        request
            .try_clone()
            .ok_or_else(|| io::Error::other("Google Drive request could not be cloned for retry"))?
            .send()
            .map_err(io::Error::other)
    })
}

fn collect_photos(
    files: &[DriveFile],
    threshold: u64,
    latest: &mut u64,
    photos: &mut Vec<RemotePhoto>,
) {
    for file in files {
        let modified = file
            .modified_time
            .as_deref()
            .and_then(crate::import::remote::parse_iso8601)
            .unwrap_or(0);
        if modified <= threshold {
            continue;
        }
        if modified > *latest {
            *latest = modified;
        }

        let size_bytes = file.size.as_deref().and_then(|s| s.parse::<u64>().ok());

        photos.push(RemotePhoto {
            remote_id: file.id.clone(),
            filename: file.name.clone(),
            size_bytes,
        });
    }
}

/// Keep changes that add or modify an image inside the configured folder.
/// Deletions and moves out of the folder are dropped, as are non-image files.
fn collect_changes(changes: &[DriveChange], folder_id: &str, photos: &mut Vec<RemotePhoto>) {
    for change in changes {
        if change.removed {
            continue;
        }
        let Some(file) = &change.file else {
            continue;
        };
        let is_image = file
            .mime_type
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/"));
        if !is_image {
            continue;
        }
        let in_folder = file
            .parents
            .as_deref()
            .is_some_and(|parents| parents.iter().any(|parent| parent == folder_id));
        if !in_folder {
            continue;
        }

        photos.push(RemotePhoto {
            remote_id: change.file_id.clone(),
            filename: file.name.clone(),
            size_bytes: file.size.as_deref().and_then(|s| s.parse::<u64>().ok()),
        });
    }
}

/// Prefix a Drive changes page token as a cursor.
fn gdrive_cursor(token: &str) -> String {
    format!("{GDRIVE_CURSOR_PREFIX}{token}")
}

/// Build an error whose kind marks a Changes API permission or availability
/// failure, which `list_changes` turns into a safe timestamp-listing fallback.
fn drive_status_error(context: &str, status: reqwest::StatusCode) -> io::Error {
    let kind = match status.as_u16() {
        403 => io::ErrorKind::PermissionDenied,
        404 | 405 | 501 => io::ErrorKind::Unsupported,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("{context}: {status}"))
}

/// True when a Changes API error means "this account cannot use the Changes
/// API" rather than a transient failure worth retrying.
fn changes_unsupported(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
    )
}

/// Percent-encode a string for use in a URL query parameter.
fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char)
            }
            b' ' => result.push('+'),
            _ => result.push_str(&format!("%{:02X}", b)),
        }
    }
    result
}

fn base64url(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// OAuth helpers
// ---------------------------------------------------------------------------

fn refresh_access_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> io::Result<String> {
    let client = http::build_client()?;
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];
    let resp = client
        .post(DEFAULT_TOKEN_URI)
        .form(&params)
        .send()
        .map_err(io::Error::other)?;

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
    }

    let token: TokenResponse = resp.json().map_err(io::Error::other)?;
    Ok(token.access_token)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::test_http::{TestReply, TestServer};

    fn file(id: &str, name: &str, modified: &str) -> String {
        format!(
            r#"{{"id":"{id}","name":"{name}","size":"123","mimeType":"image/jpeg","modifiedTime":"{modified}"}}"#
        )
    }

    fn test_source(server: &TestServer) -> GoogleDriveSource {
        GoogleDriveSource::with_api_key(
            "test-key".to_string(),
            "folder-1".to_string(),
            server.base_url.clone(),
        )
    }

    /// A bearer-auth source so the Changes API path is exercised without a
    /// token exchange.
    fn test_bearer_source(server: &TestServer) -> GoogleDriveSource {
        GoogleDriveSource::with_access_token(
            "test-token".to_string(),
            "folder-1".to_string(),
            server.base_url.clone(),
        )
    }

    fn rsa_key_pem() -> String {
        let rsa = openssl::rsa::Rsa::generate(2048).expect("generate RSA test key");
        let pkey = PKey::from_rsa(rsa).expect("wrap RSA key");
        String::from_utf8(pkey.private_key_to_pem_pkcs8().expect("PEM encode"))
            .expect("PEM is UTF-8")
    }

    fn service_account_json(pem: &str, token_uri: &str) -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": "test-project",
            "private_key_id": "key-id",
            "private_key": pem,
            "client_email": "frame@test-project.iam.gserviceaccount.com",
            "token_uri": token_uri,
        })
        .to_string()
    }

    fn decode_segment(segment: &str) -> serde_json::Value {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segment)
            .expect("base64url segment");
        serde_json::from_slice(&bytes).expect("JSON segment")
    }

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn test_service_account_assertion_has_expected_claims_and_signature() {
        let pem = rsa_key_pem();
        let key = ServiceAccountKey::parse(&service_account_json(&pem, DEFAULT_TOKEN_URI))
            .expect("valid service account key");
        let assertion = key.build_assertion(1_700_000_000).expect("sign JWT");

        let parts: Vec<&str> = assertion.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three segments");

        let header = decode_segment(parts[0]);
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["typ"], "JWT");

        let claims = decode_segment(parts[1]);
        assert_eq!(claims["iss"], "frame@test-project.iam.gserviceaccount.com");
        assert_eq!(claims["scope"], DRIVE_SCOPE);
        assert_eq!(claims["aud"], DEFAULT_TOKEN_URI);
        assert_eq!(claims["iat"], 1_700_000_000u64);
        assert_eq!(claims["exp"], 1_700_003_600u64);

        // Verify the signature with the matching public key, not the private
        // key, so a signing mistake cannot hide behind shared state.
        let private = PKey::private_key_from_pem(pem.as_bytes()).unwrap();
        let public_pem = private.public_key_to_pem().unwrap();
        let public = PKey::public_key_from_pem(&public_pem).unwrap();
        let mut verifier = openssl::sign::Verifier::new(MessageDigest::sha256(), &public).unwrap();
        verifier
            .update(format!("{}.{}", parts[0], parts[1]).as_bytes())
            .unwrap();
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        assert!(
            verifier.verify(&signature).unwrap(),
            "the RS256 signature must verify"
        );
    }

    #[test]
    fn test_service_account_connect_lists_and_downloads_with_bearer_token() {
        let pem = rsa_key_pem();
        let server = TestServer::start(|req, _idx| {
            if req.method == "POST" {
                TestReply::json(r#"{"access_token":"sa-token","expires_in":3600}"#)
            } else if req.url.contains("alt=media") {
                TestReply::new(200, b"photo-bytes".to_vec())
            } else if req.url.contains("/changes/startPageToken") {
                TestReply::json(r#"{"startPageToken":"start-token"}"#)
            } else {
                TestReply::json(r#"{"files":[]}"#)
            }
        });
        let key_json = service_account_json(&pem, &format!("{}/token", server.base_url));

        let config: HashMap<String, String> = [
            ("folder_id".to_string(), "folder-1".to_string()),
            ("service_account_json".to_string(), key_json),
        ]
        .into_iter()
        .collect();

        let mut source = GoogleDriveSource::connect_to(&config, &server.base_url)
            .expect("connect with service account");
        let (photos, cursor) = source.list_changes(None).unwrap();
        assert!(photos.is_empty());
        assert_eq!(cursor, "gdrive:start-token");

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        source.download("file-id", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"photo-bytes");

        let requests = server.requests();
        assert_eq!(
            requests.len(),
            5,
            "one token request, a folder validation, the listing, a start \
             token, and the download"
        );

        let token_request = &requests[0];
        assert_eq!(token_request.method, "POST");
        assert_eq!(token_request.url, "/token");
        assert!(
            token_request
                .body
                .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"),
            "token request must use the jwt-bearer grant, got: {}",
            token_request.body
        );
        assert!(
            token_request.body.contains("assertion="),
            "token request must include the signed assertion, got: {}",
            token_request.body
        );

        // Every Drive call uses the cached token as a Bearer header and none
        // of them leak the API key parameter.
        for request in &requests[1..] {
            assert_eq!(
                request.header("Authorization"),
                Some("Bearer sa-token"),
                "expected Bearer auth on {}",
                request.url
            );
            assert!(
                !request.url.contains("key="),
                "no API key in {}",
                request.url
            );
        }
        assert!(
            requests[1].url.contains("folder-1"),
            "connect must validate the configured folder, got: {}",
            requests[1].url
        );
    }

    #[test]
    fn test_service_account_token_refreshes_near_expiry() {
        let pem = rsa_key_pem();
        let server = TestServer::start(|req, _idx| {
            if req.method == "POST" {
                TestReply::json(r#"{"access_token":"short-token","expires_in":1}"#)
            } else if req.url.contains("/changes/startPageToken") {
                TestReply::json(r#"{"startPageToken":"tok"}"#)
            } else {
                TestReply::json(r#"{"files":[]}"#)
            }
        });
        let key_json = service_account_json(&pem, &format!("{}/token", server.base_url));
        let config: HashMap<String, String> = [
            ("folder_id".to_string(), "folder-1".to_string()),
            ("service_account_json".to_string(), key_json),
        ]
        .into_iter()
        .collect();
        let mut source = GoogleDriveSource::connect_to(&config, &server.base_url).unwrap();

        source.list_changes(None).unwrap();
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|r| r.method == "POST")
                .count(),
            1
        );

        // expires_in=1 gives a 500 ms usable window; wait it out.
        std::thread::sleep(Duration::from_millis(700));
        source.list_changes(None).unwrap();

        let posts: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST")
            .collect();
        assert_eq!(posts.len(), 2, "an expired token must be refreshed");
    }

    #[test]
    fn test_service_account_missing_fields_produce_clear_errors() {
        let no_email =
            r#"{"private_key":"-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----"}"#;
        let err = ServiceAccountKey::parse(no_email)
            .err()
            .expect("missing client_email must fail");
        assert!(err.contains("client_email"), "got: {err}");

        let no_key = r#"{"client_email":"frame@example.iam.gserviceaccount.com"}"#;
        let err = ServiceAccountKey::parse(no_key)
            .err()
            .expect("missing private_key must fail");
        assert!(err.contains("private_key"), "got: {err}");

        let malformed = "{not json";
        let err = ServiceAccountKey::parse(malformed)
            .err()
            .expect("malformed JSON must fail");
        assert!(err.contains("not valid JSON"), "got: {err}");
        assert!(
            !err.contains("private_key"),
            "the error must not echo key material: {err}"
        );
    }

    #[test]
    fn test_service_account_token_uri_falls_back_to_google_default() {
        let pem = rsa_key_pem();
        let json = serde_json::json!({
            "client_email": "frame@example.iam.gserviceaccount.com",
            "private_key": pem,
        })
        .to_string();
        let key = ServiceAccountKey::parse(&json).unwrap();
        assert_eq!(key.token_uri(), DEFAULT_TOKEN_URI);
    }

    #[test]
    fn test_connect_rejects_malformed_service_account_json() {
        let config: HashMap<String, String> = [
            ("folder_id".to_string(), "folder-1".to_string()),
            ("service_account_json".to_string(), "{oops".to_string()),
        ]
        .into_iter()
        .collect();

        let err = GoogleDriveSource::connect(&config)
            .err()
            .expect("malformed key must fail");
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn test_list_changes_follows_next_page_token() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("pageToken=page-2") {
                TestReply::json(format!(
                    r#"{{"files":[{}]}}"#,
                    file("id-b", "b.jpg", "2024-01-02T00:00:00Z")
                ))
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"nextPageToken":"page-2","files":[{}]}}"#,
                    file("id-a", "a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source.list_changes(None).unwrap();

        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["a.jpg", "b.jpg"]);
        assert_eq!(cursor, "1704153600");

        let requests = server.requests();
        assert_eq!(requests.len(), 2, "expected two list requests");
        assert!(
            requests[0].url.contains("fields=nextPageToken"),
            "fields must request nextPageToken, got: {}",
            requests[0].url
        );
        assert!(
            !requests[0].url.contains("pageToken="),
            "first page must not send a page token"
        );
        assert!(
            requests[1].url.contains("pageToken=page-2"),
            "second page must send the token, got: {}",
            requests[1].url
        );
        assert!(
            requests[0].url.contains("key=test-key"),
            "API-key sources keep the key in the URL, got: {}",
            requests[0].url
        );
    }

    #[test]
    fn test_list_changes_stops_without_next_page_token() {
        let server = TestServer::start(|_req, _idx| {
            TestReply::json(format!(
                r#"{{"files":[{}]}}"#,
                file("id-a", "a.jpg", "2024-02-01T00:00:00Z")
            ))
        });

        let mut source = test_source(&server);
        let (photos, _cursor) = source.list_changes(None).unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(server.request_count(), 1);
    }

    #[test]
    fn test_download_streams_bytes_to_destination() {
        let payload: Vec<u8> = (0..2048u32).map(|i| (i % 89) as u8).collect();
        let expected = payload.clone();
        let server = TestServer::start(move |_req, _idx| TestReply::new(200, payload.clone()));

        let mut source = test_source(&server);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");

        source.download("file-id", &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), expected);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.contains("/files/file-id?alt=media"));
    }

    #[test]
    fn test_api_key_listing_overlaps_cursor_to_avoid_missing_same_second_files() {
        let server = TestServer::start(|_req, _idx| {
            TestReply::json(format!(
                r#"{{"files":[{},{}]}}"#,
                file("old", "old.jpg", "2024-01-01T00:00:00Z"),
                file("new", "new.jpg", "2024-01-03T00:00:00Z")
            ))
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source
            .list_changes(Some("1704240000")) // 2024-01-03T00:00:00Z
            .unwrap();

        // A file exactly at the cursor is re-listed (the dedup set drops it
        // later) so a same-second edit is not missed. Older files are skipped
        // and the cursor never moves backwards.
        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["new.jpg"]);
        assert_eq!(cursor, "1704240000");
    }

    #[test]
    fn test_api_key_source_keeps_timestamp_listing_and_never_calls_changes() {
        let server = TestServer::start(|req, _idx| {
            assert!(
                !req.url.contains("/changes"),
                "API-key sources must not call the Changes API: {}",
                req.url
            );
            TestReply::json(format!(
                r#"{{"files":[{}]}}"#,
                file("id-a", "a.jpg", "2024-01-02T00:00:00Z")
            ))
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source.list_changes(None).unwrap();

        assert_eq!(photos.len(), 1);
        assert_eq!(cursor, "1704153600");

        let requests = server.requests();
        assert_eq!(requests.len(), 1, "only the folder listing is requested");
        assert!(requests[0].url.contains("/files?"));
        assert!(requests[0].url.contains("key=test-key"));
    }

    #[test]
    fn test_first_sync_fetches_start_token_before_listing() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("/changes/startPageToken") {
                TestReply::json(r#"{"startPageToken":"start-token-1"}"#)
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"files":[{}]}}"#,
                    file("id-a", "a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source.list_changes(None).unwrap();

        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].filename, "a.jpg");
        assert_eq!(cursor, "gdrive:start-token-1");

        // The token is taken first so a file added during the listing is still
        // returned by the Changes API.
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "a token, then the folder listing");
        assert!(requests[0].url.contains("/changes/startPageToken"));
        assert!(requests[1].url.contains("/files?"));
        assert!(requests.iter().all(|r| !r.url.contains("key=")));
    }

    #[test]
    fn test_first_sync_falls_back_to_timestamp_cursor_when_start_token_fails() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("/changes/startPageToken") {
                TestReply::status(403)
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"files":[{}]}}"#,
                    file("id-a", "a.jpg", "2024-01-02T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source.list_changes(None).unwrap();

        assert_eq!(photos.len(), 1, "photos still import on the fallback");
        assert_eq!(cursor, "1704153600", "the timestamp cursor is kept");

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.contains("/changes/startPageToken"));
        assert!(requests[1].url.contains("/files?"));
    }

    #[test]
    fn test_legacy_cursor_falls_back_to_timestamp_cursor_when_start_token_fails() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("/changes/startPageToken") {
                // A non-permission failure also falls back rather than
                // pinning the sync.
                TestReply::status(400)
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"files":[{},{}]}}"#,
                    file("old", "old.jpg", "2024-01-01T00:00:00Z"),
                    file("new", "new.jpg", "2024-01-03T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source
            .list_changes(Some("1704153600")) // 2024-01-02T00:00:00Z
            .unwrap();

        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["new.jpg"]);
        assert_eq!(cursor, "1704240000");
    }

    #[test]
    fn test_legacy_numeric_cursor_does_catch_up_and_returns_gdrive_cursor() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("/changes/startPageToken") {
                TestReply::json(r#"{"startPageToken":"fresh-token"}"#)
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"files":[{},{}]}}"#,
                    file("old", "old.jpg", "2024-01-01T00:00:00Z"),
                    file("new", "new.jpg", "2024-01-03T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source
            .list_changes(Some("1704153600")) // 2024-01-02T00:00:00Z
            .unwrap();

        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["new.jpg"], "only files after the timestamp");
        assert_eq!(cursor, "gdrive:fresh-token");
    }

    #[test]
    fn test_gdrive_cursor_filters_changes_and_follows_pagination() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("pageToken=page-2") {
                TestReply::json(
                    serde_json::json!({
                        "changes": [
                            {"fileId":"in-2","removed":false,"file":{
                                "id":"in-2","name":"b.jpg","size":"2","mimeType":"image/png",
                                "modifiedTime":"2024-01-02T00:00:00Z","parents":["folder-1"]
                            }},
                        ],
                        "newStartPageToken": "new-token",
                    })
                    .to_string(),
                )
            } else if req.url.contains("/changes?pageToken=page-1") {
                TestReply::json(
                    serde_json::json!({
                        "changes": [
                            // Kept: an image inside the folder.
                            {"fileId":"in-1","removed":false,"file":{
                                "id":"in-1","name":"a.jpg","size":"1","mimeType":"image/jpeg",
                                "modifiedTime":"2024-01-01T00:00:00Z","parents":["folder-1"]
                            }},
                            // Dropped: a deletion.
                            {"fileId":"gone","removed":true},
                            // Dropped: not an image.
                            {"fileId":"doc","removed":false,"file":{
                                "id":"doc","name":"notes.txt","size":"3","mimeType":"text/plain",
                                "modifiedTime":"2024-01-01T00:00:00Z","parents":["folder-1"]
                            }},
                            // Dropped: an image in another folder.
                            {"fileId":"other","removed":false,"file":{
                                "id":"other","name":"c.jpg","size":"4","mimeType":"image/jpeg",
                                "modifiedTime":"2024-01-01T00:00:00Z","parents":["other-folder"]
                            }},
                            // Dropped: no parents reported.
                            {"fileId":"noparents","removed":false,"file":{
                                "id":"noparents","name":"d.jpg","size":"5","mimeType":"image/jpeg",
                                "modifiedTime":"2024-01-01T00:00:00Z"
                            }},
                        ],
                        "nextPageToken": "page-2",
                    })
                    .to_string(),
                )
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source.list_changes(Some("gdrive:page-1")).unwrap();

        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["a.jpg", "b.jpg"]);
        assert_eq!(cursor, "gdrive:new-token");

        let requests = server.requests();
        assert_eq!(requests.len(), 2, "both pages of changes are fetched");
        assert!(requests[0].url.contains("/changes?pageToken=page-1"));
        assert!(requests[0].url.contains("newStartPageToken"));
        assert!(requests[0].url.contains("changes(fileId,removed,file("));
        assert!(requests[1].url.contains("pageToken=page-2"));
        assert!(
            requests.iter().all(|r| !r.url.contains("/files?")),
            "a native cursor must not re-list the folder"
        );
        for request in &requests {
            assert_eq!(request.header("Authorization"), Some("Bearer test-token"));
            assert!(!request.url.contains("key="));
        }
    }

    #[test]
    fn test_changes_permission_error_falls_back_and_keeps_cursor() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("/changes") {
                TestReply::status(403)
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"files":[{}]}}"#,
                    file("id-a", "a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source.list_changes(Some("gdrive:stale-token")).unwrap();

        assert_eq!(photos.len(), 1, "the fallback still imports the folder");
        assert_eq!(
            cursor, "gdrive:stale-token",
            "an unusable Changes API keeps the old cursor so nothing is skipped"
        );
    }

    #[test]
    fn test_changes_permission_error_rebaselines_when_start_token_available() {
        let server = TestServer::start(|req, _idx| {
            if req.url.contains("/changes?") {
                TestReply::status(403)
            } else if req.url.contains("/changes/startPageToken") {
                TestReply::json(r#"{"startPageToken":"fresh-baseline"}"#)
            } else if req.url.contains("/files?") {
                TestReply::json(format!(
                    r#"{{"files":[{}]}}"#,
                    file("id-a", "a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_bearer_source(&server);
        let (photos, cursor) = source.list_changes(Some("gdrive:stale-token")).unwrap();

        assert_eq!(photos.len(), 1);
        assert_eq!(cursor, "gdrive:fresh-baseline");
    }

    #[test]
    fn test_drive_status_error_marks_permission_and_unsupported() {
        assert!(changes_unsupported(&drive_status_error(
            "changes",
            reqwest::StatusCode::FORBIDDEN
        )));
        assert!(changes_unsupported(&drive_status_error(
            "changes",
            reqwest::StatusCode::NOT_FOUND
        )));
        assert!(changes_unsupported(&drive_status_error(
            "changes",
            reqwest::StatusCode::NOT_IMPLEMENTED
        )));
        assert!(!changes_unsupported(&drive_status_error(
            "changes",
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        )));
    }
}
