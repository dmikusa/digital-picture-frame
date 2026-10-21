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
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const API_BASE: &str = "https://api.dropboxapi.com/2";
const CONTENT_BASE: &str = "https://content.dropboxapi.com/2";
const TOKEN_URL: &str = "https://api.dropboxapi.com/oauth2/token";
/// Safety valve: `has_more` is server-controlled, so cap the number of pages
/// one sync check will follow rather than trusting it to terminate.
const MAX_PAGES: usize = 1_000;
/// Cursors are stored as `dbx:<provider cursor>` so a stored value can be told
/// apart from a legacy Unix-timestamp cursor.
const CURSOR_PREFIX: &str = "dbx:";
/// Access tokens are refreshed a little before they expire so one cannot lapse
/// in flight.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// Dropbox access tokens last four hours; used when the token response omits
/// `expires_in`.
const DEFAULT_TOKEN_LIFETIME_SECS: u64 = 14_400;

pub struct DropboxSource {
    client: Client,
    auth: Auth,
    folder: String,
    api_base: String,
    content_base: String,
    token_url: String,
}

/// How the source authenticates against the Dropbox API.
enum Auth {
    /// A long-lived access token supplied directly in the config.
    Static(String),
    /// A refresh token exchanged for short-lived access tokens on demand.
    Refresh {
        refresh_token: String,
        app_key: String,
        app_secret: String,
        token: Option<CachedToken>,
    },
}

struct CachedToken {
    access_token: String,
    /// Refresh once this instant passes.
    refresh_at: Instant,
}

#[derive(Serialize)]
struct ListFolderArgs {
    path: String,
    recursive: bool,
}

#[derive(Deserialize)]
struct ListFolderResult {
    #[serde(default)]
    entries: Vec<FileEntry>,
    cursor: String,
    #[serde(default)]
    has_more: bool,
}

#[derive(Deserialize)]
struct FileEntry {
    #[serde(rename = ".tag")]
    tag: String,
    path_lower: Option<String>,
    name: Option<String>,
    size: Option<u64>,
}

#[derive(Serialize)]
struct DownloadArgs {
    path: String,
}

impl DropboxSource {
    /// Constructor with injectable API roots, used by unit tests.
    #[cfg(test)]
    pub(crate) fn with_base_urls(
        access_token: String,
        folder: String,
        api_base: String,
        content_base: String,
    ) -> Self {
        DropboxSource {
            client: http::build_client().expect("test HTTP client"),
            auth: Auth::Static(access_token),
            folder,
            api_base,
            content_base,
            token_url: TOKEN_URL.to_string(),
        }
    }

    /// Resolve credentials and open the API roots. Tests use this with mock
    /// roots so the whole connect path (including token refresh) is exercised.
    pub(crate) fn connect_to(
        config: &HashMap<String, String>,
        api_base: &str,
        content_base: &str,
        token_url: &str,
    ) -> io::Result<Self> {
        let folder = config
            .get("folder")
            .cloned()
            .unwrap_or_else(|| "/Photos".to_string());

        let auth = if let Some(access_token) = config
            .get("access_token")
            .filter(|token| !token.trim().is_empty())
        {
            Auth::Static(access_token.clone())
        } else if let Some(refresh_token) = config
            .get("refresh_token")
            .filter(|token| !token.trim().is_empty())
        {
            let app_key = config.get("app_key").cloned().ok_or_else(|| {
                io::Error::other("Missing 'app_key' for Dropbox refresh-token auth")
            })?;
            let app_secret = config.get("app_secret").cloned().ok_or_else(|| {
                io::Error::other("Missing 'app_secret' for Dropbox refresh-token auth")
            })?;
            Auth::Refresh {
                refresh_token: refresh_token.clone(),
                app_key,
                app_secret,
                token: None,
            }
        } else {
            return Err(io::Error::other(
                "Dropbox source requires 'access_token' or \
                 'refresh_token'+'app_key'+'app_secret'",
            ));
        };

        let client = http::build_client()?;
        let mut source = DropboxSource {
            client,
            auth,
            folder,
            api_base: api_base.to_string(),
            content_base: content_base.to_string(),
            token_url: token_url.to_string(),
        };

        // Validate the credentials. For refresh-token auth this also fetches
        // the first access token, so a bad refresh token fails immediately
        // instead of during a sync.
        let authorization = source.authorization()?;
        let request = source
            .client
            .post(format!("{}/users/get_current_account", source.api_base))
            .header(AUTHORIZATION, authorization);
        let resp = send_request(request)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Dropbox authentication failed: {}",
                resp.status()
            )));
        }

        Ok(source)
    }

    /// The `Authorization` header for a request, refreshing a short-lived
    /// access token when it is missing or near expiry.
    fn authorization(&mut self) -> io::Result<String> {
        match &mut self.auth {
            Auth::Static(token) => Ok(bearer(token)),
            Auth::Refresh {
                refresh_token,
                app_key,
                app_secret,
                token,
            } => {
                let valid = token
                    .as_ref()
                    .map(|cached| cached.refresh_at > Instant::now())
                    .unwrap_or(false);
                if !valid {
                    *token = Some(refresh_access_token(
                        &self.client,
                        &self.token_url,
                        refresh_token,
                        app_key,
                        app_secret,
                    )?);
                }
                let cached = token.as_ref().expect("token was just refreshed");
                Ok(bearer(&cached.access_token))
            }
        }
    }

    fn list_folder(&mut self, args_json: &str) -> io::Result<ListFolderResult> {
        let authorization = self.authorization()?;
        let request = self
            .client
            .post(format!("{}/files/list_folder", self.api_base))
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(args_json.to_string());

        parse_list_result(send_request(request)?, "list_folder")
    }

    fn list_folder_continue(&mut self, cursor: &str) -> io::Result<ListFolderResult> {
        let authorization = self.authorization()?;
        let body = serde_json::json!({ "cursor": cursor }).to_string();
        let request = self
            .client
            .post(format!("{}/files/list_folder/continue", self.api_base))
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body);

        parse_list_result(send_request(request)?, "list_folder/continue")
    }

    /// List the whole folder from scratch, following pagination, and return a
    /// fresh `dbx:` cursor.
    fn full_listing(&mut self) -> io::Result<(Vec<RemotePhoto>, String)> {
        let args = ListFolderArgs {
            path: self.folder.clone(),
            recursive: true,
        };
        let args_json = serde_json::to_string(&args).map_err(io::Error::other)?;
        let result = self.list_folder(&args_json)?;
        self.drain_pages(result)
    }

    /// Continue from a stored Dropbox cursor, following pagination.
    fn continue_listing(&mut self, stored: &str) -> io::Result<(Vec<RemotePhoto>, String)> {
        let result = self.list_folder_continue(stored)?;
        self.drain_pages(result)
    }

    /// Consume every page starting at `result` and return the images plus the
    /// final `dbx:` cursor.
    fn drain_pages(
        &mut self,
        mut result: ListFolderResult,
    ) -> io::Result<(Vec<RemotePhoto>, String)> {
        let mut photos = Vec::new();
        let mut pages = 1usize;

        loop {
            collect_photos(&result.entries, &mut photos);

            if !result.has_more {
                return Ok((photos, format!("{CURSOR_PREFIX}{}", result.cursor)));
            }
            if pages >= MAX_PAGES {
                return Err(io::Error::other(format!(
                    "Dropbox list_folder exceeded {MAX_PAGES} pages"
                )));
            }
            result = self.list_folder_continue(&result.cursor)?;
            pages += 1;
        }
    }
}

fn send_request(request: RequestBuilder) -> io::Result<reqwest::blocking::Response> {
    http::send_with_retry(|| {
        request
            .try_clone()
            .ok_or_else(|| io::Error::other("Dropbox request could not be cloned for retry"))?
            .send()
            .map_err(io::Error::other)
    })
}

impl RemotePhotoSource for DropboxSource {
    fn connect(config: &HashMap<String, String>) -> io::Result<Self> {
        Self::connect_to(config, API_BASE, CONTENT_BASE, TOKEN_URL)
    }

    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)> {
        // A provider cursor means "continue from here". Anything else (no
        // cursor, or a legacy Unix-timestamp cursor) needs a full listing to
        // catch up, including files older than the old timestamp filter.
        if let Some(stored) = cursor
            .and_then(|value| value.strip_prefix(CURSOR_PREFIX))
            .filter(|stored| !stored.is_empty())
        {
            match self.continue_listing(stored) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Dropbox invalidates cursors (reset, path removed), and a
                    // transient failure leaves us here too. Re-list from
                    // scratch rather than retrying a cursor that may be dead.
                    log::warn!(
                        "Dropbox cursor could not be used ({}); falling back to a full listing",
                        e
                    );
                }
            }
        }

        self.full_listing()
    }

    fn download(&mut self, remote_id: &str, dest: &Path) -> io::Result<()> {
        let args = DownloadArgs {
            path: remote_id.to_string(),
        };
        let args_json = serde_json::to_string(&args).map_err(io::Error::other)?;

        let authorization = self.authorization()?;
        let request = self
            .client
            .post(format!("{}/files/download", self.content_base))
            .header(AUTHORIZATION, authorization)
            .header("Dropbox-API-Arg", &args_json);
        let resp = send_request(request)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Dropbox download failed for {remote_id}: {}",
                resp.status()
            )));
        }

        http::download_to_file(resp, dest)
    }
}

fn parse_list_result(
    resp: reqwest::blocking::Response,
    operation: &str,
) -> io::Result<ListFolderResult> {
    if !resp.status().is_success() {
        return Err(io::Error::other(format!(
            "Dropbox {operation} failed: {}",
            resp.status()
        )));
    }
    resp.json().map_err(io::Error::other)
}

/// Exchange a refresh token for a short-lived access token.
fn refresh_access_token(
    client: &Client,
    token_url: &str,
    refresh_token: &str,
    app_key: &str,
    app_secret: &str,
) -> io::Result<CachedToken> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", app_key),
        ("client_secret", app_secret),
    ];
    let request = client.post(token_url).form(&params);
    let resp = send_request(request)?;

    if !resp.status().is_success() {
        let status = resp.status();
        // The body explains the failure (bad secret, revoked token). Truncate
        // it so a hostile server cannot flood the logs.
        let detail: String = resp.text().unwrap_or_default().chars().take(300).collect();
        return Err(io::Error::other(format!(
            "Dropbox token refresh failed: {status} {detail}"
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
        DEFAULT_TOKEN_LIFETIME_SECS
    } else {
        token.expires_in
    });
    // Never refresh later than `lifetime - margin`; for very short-lived test
    // tokens use half the lifetime so there is still a usable window.
    let margin = TOKEN_REFRESH_MARGIN.min(lifetime / 2);
    Ok(CachedToken {
        access_token: token.access_token,
        refresh_at: Instant::now() + lifetime.saturating_sub(margin),
    })
}

fn collect_photos(entries: &[FileEntry], photos: &mut Vec<RemotePhoto>) {
    for entry in entries {
        if entry.tag != "file" {
            continue;
        }
        let path_lower = entry.path_lower.as_deref().unwrap_or("");
        if !is_image(path_lower) {
            continue;
        }

        photos.push(RemotePhoto {
            remote_id: path_lower.to_string(),
            filename: entry
                .name
                .clone()
                .unwrap_or_else(|| "unknown.jpg".to_string()),
            size_bytes: entry.size,
        });
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn is_image(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".gif")
        || lower.ends_with(".heic")
        || lower.ends_with(".heif")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::test_http::{TestReply, TestServer};

    fn entry(name: &str, modified: &str) -> String {
        format!(
            r#"{{".tag":"file","path_lower":"/photos/{name}","name":"{name}","size":42,"server_modified":"{modified}"}}"#
        )
    }

    fn test_source(server: &TestServer) -> DropboxSource {
        DropboxSource::with_base_urls(
            "token".to_string(),
            "/Photos".to_string(),
            server.base_url.clone(),
            server.base_url.clone(),
        )
    }

    fn refresh_config() -> HashMap<String, String> {
        [
            ("refresh_token".to_string(), "refresh-me".to_string()),
            ("app_key".to_string(), "key".to_string()),
            ("app_secret".to_string(), "secret".to_string()),
        ]
        .into_iter()
        .collect()
    }

    fn token_request_count(server: &TestServer) -> usize {
        server
            .requests()
            .iter()
            .filter(|request| request.url.ends_with("/oauth2/token"))
            .count()
    }

    #[test]
    fn test_is_image() {
        assert!(is_image("photo.jpg"));
        assert!(is_image("path/to/photo.JPEG"));
        assert!(is_image("image.png"));
        assert!(!is_image("document.pdf"));
        assert!(!is_image("folder"));
    }

    #[test]
    fn test_list_changes_follows_pagination() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"cursor-page-2","has_more":true}}"#,
                    entry("a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else if req.url.ends_with("/files/list_folder/continue") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"cursor-page-3","has_more":false}}"#,
                    entry("b.jpg", "2024-01-02T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source.list_changes(None).unwrap();

        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["a.jpg", "b.jpg"]);
        // The provider cursor reflects the last page, not just page one.
        assert_eq!(cursor, "dbx:cursor-page-3");

        let requests = server.requests();
        assert_eq!(requests.len(), 2, "expected two list requests");
        assert!(requests[0].url.ends_with("/files/list_folder"));
        assert!(requests[1].url.ends_with("/files/list_folder/continue"));
        assert!(
            requests[1].body.contains("cursor-page-2"),
            "continue should send the previous cursor, got: {}",
            requests[1].body
        );
    }

    #[test]
    fn test_list_changes_stops_without_has_more() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"cursor-page-2","has_more":false}}"#,
                    entry("only.jpg", "2024-03-01T00:00:00Z")
                ))
            } else {
                TestReply::status(500)
            }
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source.list_changes(None).unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(cursor, "dbx:cursor-page-2");
        assert_eq!(server.request_count(), 1);
    }

    #[test]
    fn test_list_changes_retries_transient_errors() {
        let server = TestServer::start(|req, index| {
            if index == 0 {
                // Retry-After avoids a real backoff sleep in the test.
                TestReply::status(503).with_header("Retry-After", "0")
            } else if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"c","has_more":false}}"#,
                    entry("retried.jpg", "2024-01-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_source(&server);
        let (photos, _cursor) = source.list_changes(None).unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(server.request_count(), 2);
    }

    #[test]
    fn test_list_changes_with_provider_cursor_continues_without_relisting() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/files/list_folder/continue") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"cursor-next","has_more":false}}"#,
                    entry("new.jpg", "2024-05-01T00:00:00Z")
                ))
            } else {
                TestReply::status(500)
            }
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source.list_changes(Some("dbx:stored-cursor")).unwrap();

        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].filename, "new.jpg");
        assert_eq!(cursor, "dbx:cursor-next");

        let requests = server.requests();
        assert_eq!(requests.len(), 1, "a provider cursor must not re-list");
        assert!(requests[0].url.ends_with("/files/list_folder/continue"));
        assert!(
            requests[0].body.contains("stored-cursor"),
            "continue should send the stored cursor, got: {}",
            requests[0].body
        );
    }

    #[test]
    fn test_list_changes_legacy_timestamp_does_full_listing() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{},{}],"cursor":"c","has_more":false}}"#,
                    entry("old.jpg", "2024-01-01T00:00:00Z"),
                    entry("new.jpg", "2024-01-03T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_source(&server);
        // A legacy numeric cursor must not filter entries: the provider cursor
        // returned here is what prevents re-imports on the next sync.
        let (photos, cursor) = source.list_changes(Some("1704240000")).unwrap();

        let names: Vec<&str> = photos.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(names, vec!["old.jpg", "new.jpg"]);
        assert_eq!(cursor, "dbx:c");
        assert!(server.requests()[0].url.ends_with("/files/list_folder"));
    }

    #[test]
    fn test_list_changes_falls_back_to_full_listing_when_cursor_rejected() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/files/list_folder/continue") {
                // Dropbox returns 409 for a reset or otherwise invalid cursor.
                TestReply::new(409, r#"{"error_summary":"reset/..."}"#)
            } else if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"fresh","has_more":false}}"#,
                    entry("after-reset.jpg", "2024-06-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = test_source(&server);
        let (photos, cursor) = source.list_changes(Some("dbx:stale")).unwrap();

        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].filename, "after-reset.jpg");
        assert_eq!(cursor, "dbx:fresh");

        let requests = server.requests();
        assert_eq!(requests.len(), 2, "continue then a full listing");
        assert!(requests[0].url.ends_with("/files/list_folder/continue"));
        assert!(requests[1].url.ends_with("/files/list_folder"));
    }

    #[test]
    fn test_connect_with_static_token_validates_lists_and_downloads() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/users/get_current_account") {
                TestReply::json(r#"{"account_id":"dbid:test"}"#)
            } else if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"c1","has_more":false}}"#,
                    entry("a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else if req.url.ends_with("/files/download") {
                TestReply::new(200, b"photo-bytes".to_vec())
            } else {
                TestReply::status(404)
            }
        });

        let config: HashMap<String, String> =
            [("access_token".to_string(), "static-token".to_string())]
                .into_iter()
                .collect();

        let mut source = DropboxSource::connect_to(
            &config,
            &server.base_url,
            &server.base_url,
            &format!("{}/oauth2/token", server.base_url),
        )
        .expect("connect with a static token");

        let (photos, _cursor) = source.list_changes(None).unwrap();
        assert_eq!(photos.len(), 1);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        source.download("/photos/a.jpg", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"photo-bytes");

        // A static token is never exchanged: every call carries it directly.
        assert_eq!(token_request_count(&server), 0);
        for request in server.requests() {
            assert_eq!(
                request.header("Authorization"),
                Some("Bearer static-token"),
                "expected the static token on {}",
                request.url
            );
        }
    }

    #[test]
    fn test_refresh_token_flow_obtains_and_uses_bearer_token() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/oauth2/token") {
                TestReply::json(r#"{"access_token":"fresh-token","expires_in":3600}"#)
            } else if req.url.ends_with("/users/get_current_account") {
                TestReply::json(r#"{"account_id":"dbid:test"}"#)
            } else if req.url.ends_with("/files/list_folder") {
                TestReply::json(format!(
                    r#"{{"entries":[{}],"cursor":"c1","has_more":false}}"#,
                    entry("a.jpg", "2024-01-01T00:00:00Z")
                ))
            } else {
                TestReply::status(404)
            }
        });

        let mut source = DropboxSource::connect_to(
            &refresh_config(),
            &server.base_url,
            &server.base_url,
            &format!("{}/oauth2/token", server.base_url),
        )
        .expect("connect with a refresh token");

        let (photos, _cursor) = source.list_changes(None).unwrap();
        assert_eq!(photos.len(), 1);

        let requests = server.requests();
        assert_eq!(requests.len(), 3, "token, account check, list");
        let token_request = &requests[0];
        assert!(token_request.url.ends_with("/oauth2/token"));
        assert!(
            token_request.body.contains("grant_type=refresh_token"),
            "got: {}",
            token_request.body
        );
        assert!(token_request.body.contains("refresh_token=refresh-me"));
        assert!(token_request.body.contains("client_id=key"));
        assert!(token_request.body.contains("client_secret=secret"));

        // The API calls use the freshly minted token, not the refresh token.
        for request in &requests[1..] {
            assert_eq!(
                request.header("Authorization"),
                Some("Bearer fresh-token"),
                "expected the refreshed token on {}",
                request.url
            );
        }
    }

    #[test]
    fn test_refresh_token_renews_near_expiry() {
        let server = TestServer::start(|req, _idx| {
            if req.url.ends_with("/oauth2/token") {
                TestReply::json(r#"{"access_token":"short-token","expires_in":1}"#)
            } else if req.url.ends_with("/users/get_current_account") {
                TestReply::json(r#"{"account_id":"dbid:test"}"#)
            } else if req.url.ends_with("/files/list_folder") {
                TestReply::json(r#"{"entries":[],"cursor":"c","has_more":false}"#)
            } else {
                TestReply::status(404)
            }
        });

        let mut source = DropboxSource::connect_to(
            &refresh_config(),
            &server.base_url,
            &server.base_url,
            &format!("{}/oauth2/token", server.base_url),
        )
        .unwrap();

        source.list_changes(None).unwrap();
        assert_eq!(token_request_count(&server), 1);

        // expires_in=1 leaves a 500 ms usable window; wait it out.
        std::thread::sleep(Duration::from_millis(700));
        source.list_changes(None).unwrap();

        assert_eq!(
            token_request_count(&server),
            2,
            "an expired token must be refreshed"
        );
    }

    #[test]
    fn test_refresh_credentials_missing_produce_clear_errors() {
        let none = HashMap::new();
        let err = DropboxSource::connect(&none)
            .err()
            .expect("no credentials must fail");
        assert!(err.to_string().contains("access_token"), "{err}");

        let no_key: HashMap<String, String> =
            [("refresh_token".to_string(), "refresh-me".to_string())]
                .into_iter()
                .collect();
        let err = DropboxSource::connect(&no_key)
            .err()
            .expect("missing app_key must fail");
        assert!(err.to_string().contains("app_key"), "{err}");

        let no_secret: HashMap<String, String> = [
            ("refresh_token".to_string(), "refresh-me".to_string()),
            ("app_key".to_string(), "key".to_string()),
        ]
        .into_iter()
        .collect();
        let err = DropboxSource::connect(&no_secret)
            .err()
            .expect("missing app_secret must fail");
        assert!(err.to_string().contains("app_secret"), "{err}");
    }

    #[test]
    fn test_download_streams_bytes_to_destination() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 97) as u8).collect();
        let expected = payload.clone();
        let server = TestServer::start(move |_req, _idx| TestReply::new(200, payload.clone()));

        let mut source = test_source(&server);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");

        source.download("/photos/a.jpg", &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), expected);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.ends_with("/files/download"));
    }
}
