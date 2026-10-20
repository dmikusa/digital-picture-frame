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

const API_BASE: &str = "https://api.dropboxapi.com/2";
const CONTENT_BASE: &str = "https://content.dropboxapi.com/2";
/// Safety valve: `has_more` is server-controlled, so cap the number of pages
/// one sync check will follow rather than trusting it to terminate.
const MAX_PAGES: usize = 1_000;

pub struct DropboxSource {
    client: Client,
    access_token: String,
    folder: String,
    api_base: String,
    content_base: String,
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
    server_modified: Option<String>,
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
            access_token,
            folder,
            api_base,
            content_base,
        }
    }

    fn list_folder(&self, args_json: &str) -> io::Result<ListFolderResult> {
        let request = self
            .client
            .post(format!("{}/files/list_folder", self.api_base))
            .header(AUTHORIZATION, bearer(&self.access_token))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(args_json.to_string());

        parse_list_result(send_request(request)?, "list_folder")
    }

    fn list_folder_continue(&self, cursor: &str) -> io::Result<ListFolderResult> {
        let body = serde_json::json!({ "cursor": cursor }).to_string();
        let request = self
            .client
            .post(format!("{}/files/list_folder/continue", self.api_base))
            .header(AUTHORIZATION, bearer(&self.access_token))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body);

        parse_list_result(send_request(request)?, "list_folder/continue")
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
        let access_token = config
            .get("access_token")
            .cloned()
            .ok_or_else(|| io::Error::other("Missing 'access_token' for Dropbox source"))?;
        let folder = config
            .get("folder")
            .cloned()
            .unwrap_or_else(|| "/Photos".to_string());

        let client = http::build_client()?;

        // Validate the token
        let request = client
            .post(format!("{API_BASE}/users/get_current_account"))
            .header(AUTHORIZATION, bearer(&access_token));
        let resp = send_request(request)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Dropbox authentication failed: {}",
                resp.status()
            )));
        }

        Ok(DropboxSource {
            client,
            access_token,
            folder,
            api_base: API_BASE.to_string(),
            content_base: CONTENT_BASE.to_string(),
        })
    }

    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)> {
        let last_sync = cursor.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);

        let args = ListFolderArgs {
            path: self.folder.clone(),
            recursive: true,
        };
        let args_json = serde_json::to_string(&args).map_err(io::Error::other)?;

        let mut latest = last_sync;
        let mut photos = Vec::new();

        // `files/list_folder` returns at most one page. Follow `has_more`
        // with `files/list_folder/continue` until every page is consumed,
        // otherwise folders larger than one page never fully import.
        let mut result = self.list_folder(&args_json)?;
        let mut pages = 1usize;
        loop {
            collect_photos(&result.entries, last_sync, &mut latest, &mut photos);

            if !result.has_more {
                break;
            }
            if pages >= MAX_PAGES {
                return Err(io::Error::other(format!(
                    "Dropbox list_folder exceeded {MAX_PAGES} pages"
                )));
            }
            result = self.list_folder_continue(&result.cursor)?;
            pages += 1;
        }

        Ok((photos, latest.to_string()))
    }

    fn download(&mut self, remote_id: &str, dest: &Path) -> io::Result<()> {
        let args = DownloadArgs {
            path: remote_id.to_string(),
        };
        let args_json = serde_json::to_string(&args).map_err(io::Error::other)?;

        let request = self
            .client
            .post(format!("{}/files/download", self.content_base))
            .header(AUTHORIZATION, bearer(&self.access_token))
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

fn collect_photos(
    entries: &[FileEntry],
    last_sync: u64,
    latest: &mut u64,
    photos: &mut Vec<RemotePhoto>,
) {
    for entry in entries {
        if entry.tag != "file" {
            continue;
        }
        let path_lower = entry.path_lower.as_deref().unwrap_or("");
        if !is_image(path_lower) {
            continue;
        }

        let modified = entry
            .server_modified
            .as_deref()
            .and_then(crate::import::remote::parse_iso8601)
            .unwrap_or(0);
        if modified <= last_sync {
            continue;
        }
        if modified > *latest {
            *latest = modified;
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
        // The timestamp cursor reflects the newest page, not just page one.
        assert_eq!(cursor, "1704153600");

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
        let (photos, _cursor) = source.list_changes(None).unwrap();
        assert_eq!(photos.len(), 1);
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

    #[test]
    fn test_list_changes_skips_entries_at_or_before_cursor() {
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
        let (photos, cursor) = source
            .list_changes(Some("1704240000")) // 2024-01-03T00:00:00Z
            .unwrap();
        assert!(photos.is_empty(), "entries at the cursor must be skipped");
        assert_eq!(cursor, "1704240000");
    }
}
