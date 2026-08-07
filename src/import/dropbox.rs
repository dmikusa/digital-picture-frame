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

use crate::import::remote::{RemotePhoto, RemotePhotoSource};
use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::Path;

const API_BASE: &str = "https://api.dropboxapi.com/2";
const CONTENT_BASE: &str = "https://content.dropboxapi.com/2";

pub struct DropboxSource {
    client: Client,
    access_token: String,
    folder: String,
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

        let client = Client::new();

        // Validate the token
        let resp = client
            .post(format!("{API_BASE}/users/get_current_account"))
            .header(AUTHORIZATION, bearer(&access_token))
            .send()
            .map_err(io::Error::other)?;

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
        })
    }

    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)> {
        let last_sync = cursor.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);

        #[derive(Serialize)]
        struct ListFolderArgs {
            path: String,
            recursive: bool,
        }

        #[derive(Deserialize)]
        struct ListFolderResult {
            entries: Vec<FileEntry>,
            #[allow(dead_code)]
            cursor: String,
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

        let args = ListFolderArgs {
            path: self.folder.clone(),
            recursive: true,
        };
        let args_json = serde_json::to_string(&args).map_err(io::Error::other)?;

        let resp = self
            .client
            .post(format!("{API_BASE}/files/list_folder"))
            .header(AUTHORIZATION, bearer(&self.access_token))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(args_json)
            .send()
            .map_err(io::Error::other)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Dropbox list_folder failed: {}",
                resp.status()
            )));
        }

        let result: ListFolderResult = resp.json().map_err(io::Error::other)?;

        let mut latest = last_sync;
        let mut photos = Vec::new();

        for entry in &result.entries {
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
                .and_then(parse_dropbox_time)
                .unwrap_or(0);
            if modified <= last_sync {
                continue;
            }
            if modified > latest {
                latest = modified;
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

        Ok((photos, latest.to_string()))
    }

    fn download(&mut self, remote_id: &str, dest: &Path) -> io::Result<()> {
        #[derive(Serialize)]
        struct DownloadArgs {
            path: String,
        }

        let args = DownloadArgs {
            path: remote_id.to_string(),
        };
        let args_json = serde_json::to_string(&args).map_err(io::Error::other)?;

        let resp = self
            .client
            .post(format!("{CONTENT_BASE}/files/download"))
            .header(AUTHORIZATION, bearer(&self.access_token))
            .header("Dropbox-API-Arg", &args_json)
            .send()
            .map_err(io::Error::other)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Dropbox download failed for {remote_id}: {}",
                resp.status()
            )));
        }

        let bytes = resp.bytes().map_err(io::Error::other)?;
        std::fs::write(dest, &bytes)?;
        Ok(())
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

fn parse_dropbox_time(s: &str) -> Option<u64> {
    // Dropbox format: "2015-05-12T15:50:38Z"
    // Parse manually without chrono.
    if s.len() < 20 || !s.ends_with('Z') {
        return None;
    }
    let s = &s[..s.len() - 1]; // strip Z
    let year: i32 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let min: u32 = s[14..16].parse().ok()?;
    let sec: u32 = s[17..19].parse().ok()?;

    // Simple date-to-unix conversion (correct for dates after 1970).
    let days_before_month: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut days = (year - 1970) as i64 * 365;
    // leap days
    days += ((year - 1 - 1968) / 4) as i64;
    days -= ((year - 1 - 1900) / 100) as i64;
    days += ((year - 1 - 1600) / 400) as i64;
    // days from months
    days += days_before_month[(month - 1) as usize] as i64;
    // leap day in current year
    if month > 2 && is_leap(year) {
        days += 1;
    }
    days += (day - 1) as i64;
    let total = days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    Some(total as u64)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dropbox_time() {
        // 2015-05-12T15:50:38Z = 1431445838
        let ts = parse_dropbox_time("2015-05-12T15:50:38Z").unwrap();
        assert_eq!(ts, 1431445838);

        // 2021-01-01T00:00:00Z = 1609459200
        let ts = parse_dropbox_time("2021-01-01T00:00:00Z").unwrap();
        assert_eq!(ts, 1609459200);
    }

    #[test]
    fn test_is_image() {
        assert!(is_image("photo.jpg"));
        assert!(is_image("path/to/photo.JPEG"));
        assert!(is_image("image.png"));
        assert!(!is_image("document.pdf"));
        assert!(!is_image("folder"));
    }
}
