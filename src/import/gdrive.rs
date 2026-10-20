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
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::Path;

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Ask for the page token alongside the file fields; without it the caller
/// cannot tell that more pages exist.
const FIELDS: &str = "nextPageToken,files(id,name,size,mimeType,modifiedTime)";
const PAGE_SIZE: usize = 1_000;
/// Safety valve: the server controls `nextPageToken`, so cap the number of
/// pages one sync check will follow.
const MAX_PAGES: usize = 1_000;

enum AuthMode {
    ApiKey(String),
    OAuth {
        #[allow(dead_code)]
        client_id: String,
        #[allow(dead_code)]
        client_secret: String,
        #[allow(dead_code)]
        refresh_token: String,
        access_token: String,
    },
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
    pub(crate) fn with_base_url(api_key: String, folder_id: String, api_base: String) -> Self {
        GoogleDriveSource {
            client: http::build_client().expect("test HTTP client"),
            auth: AuthMode::ApiKey(api_key),
            folder_id,
            api_base,
        }
    }

    fn make_get(&self, url: &str) -> io::Result<reqwest::blocking::Response> {
        let mut req = self.client.get(url);
        // API-key auth is already in the URL via ?key=; OAuth uses a header.
        if let AuthMode::OAuth { access_token, .. } = &self.auth {
            req = req.header(AUTHORIZATION, bearer(access_token));
        }
        send_request(req)
    }

    fn auth_param(&self) -> String {
        match &self.auth {
            AuthMode::ApiKey(key) => format!("key={key}"),
            AuthMode::OAuth { .. } => String::new(),
        }
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
    #[allow(dead_code)]
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    #[serde(rename = "modifiedTime")]
    modified_time: Option<String>,
}

impl RemotePhotoSource for GoogleDriveSource {
    fn connect(config: &HashMap<String, String>) -> io::Result<Self> {
        let folder_id = config
            .get("folder_id")
            .cloned()
            .ok_or_else(|| io::Error::other("Missing 'folder_id' for Google Drive source"))?;

        let auth = if let Some(refresh_token) = config.get("refresh_token").cloned() {
            let client_id = config
                .get("client_id")
                .cloned()
                .ok_or_else(|| io::Error::other("Missing 'client_id' for OAuth"))?;
            let client_secret = config
                .get("client_secret")
                .cloned()
                .ok_or_else(|| io::Error::other("Missing 'client_secret' for OAuth"))?;
            let access_token = refresh_access_token(&refresh_token, &client_id, &client_secret)?;
            AuthMode::OAuth {
                client_id,
                client_secret,
                refresh_token,
                access_token,
            }
        } else if let Some(api_key) = config.get("api_key").cloned() {
            AuthMode::ApiKey(api_key)
        } else {
            return Err(io::Error::other(
                "Google Drive source requires either 'api_key' or 'refresh_token'+'client_id'+'client_secret'",
            ));
        };

        let client = http::build_client()?;
        let source = GoogleDriveSource {
            client,
            auth,
            folder_id: folder_id.clone(),
            api_base: API_BASE.to_string(),
        };

        // Validate connectivity
        let url = format!(
            "{}/files?q='{folder_id}'+in+parents&pageSize=1&{}",
            source.api_base,
            source.auth_param()
        );
        let resp = source.make_get(&url)?;
        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Google Drive validation failed: {} — ensure folder access is granted",
                resp.status()
            )));
        }

        Ok(source)
    }

    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)> {
        let last_sync = cursor.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);

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
            collect_photos(&result.files, last_sync, &mut latest, &mut photos);

            match result.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok((photos, latest.to_string())),
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
    last_sync: u64,
    latest: &mut u64,
    photos: &mut Vec<RemotePhoto>,
) {
    for file in files {
        let modified = file
            .modified_time
            .as_deref()
            .and_then(crate::import::remote::parse_iso8601)
            .unwrap_or(0);
        if modified <= last_sync {
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
        .post(TOKEN_URL)
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

/// Exchange an authorization code for tokens (used by the admin server).
pub fn exchange_code(
    code: &str,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
) -> io::Result<String> {
    let client = http::build_client()?;
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ];
    let resp = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .map_err(io::Error::other)?;

    #[derive(Deserialize)]
    struct TokenResponse {
        refresh_token: String,
    }

    let token: TokenResponse = resp.json().map_err(io::Error::other)?;
    Ok(token.refresh_token)
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
        GoogleDriveSource::with_base_url(
            "test-key".to_string(),
            "folder-1".to_string(),
            server.base_url.clone(),
        )
    }

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
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
    fn test_list_changes_skips_entries_at_or_before_cursor() {
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
        assert!(photos.is_empty(), "entries at the cursor must be skipped");
        assert_eq!(cursor, "1704240000");
    }
}
