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
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::Path;

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

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
}

impl GoogleDriveSource {
    fn make_get(&self, url: &str) -> io::Result<reqwest::blocking::Response> {
        let mut req = self.client.get(url);
        match &self.auth {
            AuthMode::ApiKey(key) => {
                // key already in url via ?key= param
                let _ = key;
            }
            AuthMode::OAuth { access_token, .. } => {
                req = req.header(AUTHORIZATION, bearer(access_token));
            }
        }
        req.send().map_err(io::Error::other)
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

        let client = Client::new();
        let source = GoogleDriveSource {
            client,
            auth,
            folder_id: folder_id.clone(),
        };

        // Validate connectivity
        let url = format!(
            "{API_BASE}/files?q='{folder_id}'+in+parents&pageSize=1&{}",
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
        let url = format!(
            "{API_BASE}/files?q={}&orderBy=modifiedTime&fields=files(id,name,size,mimeType,modifiedTime)&{}",
            urlencoding(&query),
            self.auth_param()
        );

        let resp = self.make_get(&url)?;

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
            "{API_BASE}/files/{}?alt=media&{}",
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

// ---------------------------------------------------------------------------
// OAuth helpers
// ---------------------------------------------------------------------------

fn refresh_access_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> io::Result<String> {
    let client = Client::new();
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
    let client = Client::new();
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

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
    }
}
