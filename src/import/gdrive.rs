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
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::Path;

const API_BASE: &str = "https://www.googleapis.com/drive/v3";

pub struct GoogleDriveSource {
    client: Client,
    api_key: String,
    folder_id: String,
}

impl RemotePhotoSource for GoogleDriveSource {
    fn connect(config: &HashMap<String, String>) -> io::Result<Self> {
        let api_key = config
            .get("api_key")
            .cloned()
            .ok_or_else(|| io::Error::other("Missing 'api_key' for Google Drive source"))?;
        let folder_id = config
            .get("folder_id")
            .cloned()
            .ok_or_else(|| io::Error::other("Missing 'folder_id' for Google Drive source"))?;

        let client = Client::new();

        // Validate by listing 1 file
        let url = format!("{API_BASE}/files?q='{folder_id}'+in+parents&pageSize=1&key={api_key}");
        let resp = client.get(&url).send().map_err(io::Error::other)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Google Drive API key validation failed: {} — ensure the folder is shared publicly",
                resp.status()
            )));
        }

        Ok(GoogleDriveSource {
            client,
            api_key,
            folder_id,
        })
    }

    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)> {
        let last_sync = cursor.and_then(|c| c.parse::<u64>().ok()).unwrap_or(0);

        #[derive(Deserialize)]
        struct FileList {
            #[allow(dead_code)]
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
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

        let query = format!(
            "'{}' in parents and mimeType contains 'image/' and trashed = false",
            self.folder_id
        );
        let url = format!(
            "{API_BASE}/files?q={}&orderBy=modifiedTime&fields=files(id,name,size,mimeType,modifiedTime)&key={}",
            urlencoding(&query),
            self.api_key
        );

        let resp = self.client.get(&url).send().map_err(io::Error::other)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Google Drive list failed: {}",
                resp.status()
            )));
        }

        let result: FileList = resp.json().map_err(io::Error::other)?;

        let mut latest = last_sync;
        let mut photos = Vec::new();

        for file in &result.files {
            let modified = file
                .modified_time
                .as_deref()
                .and_then(crate::import::remote::parse_iso8601)
                .unwrap_or(0);
            if modified <= last_sync {
                continue;
            }
            if modified > latest {
                latest = modified;
            }

            let size_bytes = file.size.as_deref().and_then(|s| s.parse::<u64>().ok());

            photos.push(RemotePhoto {
                remote_id: file.id.clone(),
                filename: file.name.clone(),
                size_bytes,
            });
        }

        Ok((photos, latest.to_string()))
    }

    fn download(&mut self, remote_id: &str, dest: &Path) -> io::Result<()> {
        let url = format!(
            "{API_BASE}/files/{}?alt=media&key={}",
            remote_id, self.api_key
        );

        let resp = self.client.get(&url).send().map_err(io::Error::other)?;

        if !resp.status().is_success() {
            return Err(io::Error::other(format!(
                "Google Drive download failed for {remote_id}: {}",
                resp.status()
            )));
        }

        let bytes = resp.bytes().map_err(io::Error::other)?;
        std::fs::write(dest, &bytes)?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
    }
}
