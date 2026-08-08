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

use crate::config::RemoteSourceConfig;
use std::collections::HashMap;
use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tiny_http_fork::{Header, Method, Request, Response, Server, StatusCode};

pub struct AdminServer {
    port: u16,
    password: String,
    staged_sources: Arc<Mutex<Vec<RemoteSourceConfig>>>,
    finished: Arc<AtomicBool>,
    last_request: Arc<Mutex<Instant>>,
}

impl AdminServer {
    pub fn new(password: String) -> io::Result<Self> {
        let port = find_free_port()?;

        Ok(AdminServer {
            port,
            password,
            staged_sources: Arc::new(Mutex::new(Vec::new())),
            finished: Arc::new(AtomicBool::new(false)),
            last_request: Arc::new(Mutex::new(Instant::now())),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    pub fn timed_out(&self) -> bool {
        let last = *self.last_request.lock().unwrap();
        last.elapsed().as_secs() >= 600
    }

    pub fn take_sources(&self) -> Vec<RemoteSourceConfig> {
        self.staged_sources.lock().unwrap().clone()
    }

    pub fn run(&self, shutdown: Arc<AtomicBool>) -> io::Result<()> {
        let server = Server::http(format!("0.0.0.0:{}", self.port)).map_err(io::Error::other)?;

        let timeout = std::time::Duration::from_millis(500);

        loop {
            if shutdown.load(Ordering::Relaxed) || self.finished.load(Ordering::Relaxed) {
                break;
            }

            match server.recv_timeout(timeout) {
                Ok(Some(mut request)) => {
                    let response = self.handle_request(&mut request);
                    let _ = request.respond(response);
                }
                Ok(None) => {
                    // Timeout — loop back
                }
                Err(e) => {
                    log::error!("Admin server accept error: {e}");
                    break;
                }
            }
        }

        Ok(())
    }

    fn handle_request(&self, request: &mut Request) -> Response<Box<dyn std::io::Read + Send>> {
        *self.last_request.lock().unwrap() = Instant::now();

        let path = request.url().to_string();
        let method = request.method();

        // Skip auth for OAuth callbacks
        if !path.starts_with("/oauth/") && !self.check_auth(request) {
            return Response::from_string("Unauthorized")
                .with_status_code(StatusCode::from(401))
                .boxed();
        }

        match (method, path.as_str()) {
            // ---- API routes ---------------------------------------------------
            (Method::Get, "/api/sources") => self.api_list_sources(),
            (Method::Post, "/api/sources/dropbox") => self.api_save_source(request, "dropbox"),
            (Method::Post, "/api/sources/google-drive") => {
                self.api_save_source(request, "google_drive")
            }
            (Method::Delete, p) if p.starts_with("/api/sources/") => {
                let name = &p["/api/sources/".len()..];
                self.api_delete_source(name)
            }
            (Method::Get, "/api/status") => self.api_status(),
            (Method::Post, "/api/finish") => self.api_finish(),

            // ---- HTML pages ---------------------------------------------------
            (Method::Get, "/") => serve_html(page_dashboard(&self.staged_sources)),
            (Method::Get, "/sources") => serve_html(page_sources(&self.staged_sources)),
            (Method::Get, "/sources/add") => serve_html(page_add_source()),
            (Method::Get, "/sources/dropbox") => serve_html(page_form_dropbox()),
            (Method::Get, "/sources/google-drive") => serve_html(page_form_gdrive()),

            _ => Response::from_string("Not Found")
                .with_status_code(StatusCode(404))
                .boxed(),
        }
    }

    fn check_auth(&self, request: &Request) -> bool {
        for header in request.headers() {
            if header.field.equiv("Authorization") {
                let val = header.value.as_str();
                let expected = format!(
                    "Basic {}",
                    base64_encode(&format!("photo-frame:{}", self.password))
                );
                return val.trim() == expected;
            }
        }
        false
    }

    // ---- API handlers --------------------------------------------------------

    fn api_list_sources(&self) -> Response<Box<dyn std::io::Read + Send>> {
        let sources = self.staged_sources.lock().unwrap();
        let json = serde_json::to_string(&*sources).unwrap_or_else(|_| "[]".into());
        json_response(200, &json)
    }

    fn api_save_source(
        &self,
        request: &mut Request,
        source_type: &str,
    ) -> Response<Box<dyn std::io::Read + Send>> {
        let body = read_body(request);
        let params: HashMap<String, String> = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(e) => return json_response(400, &format!(r#"{{"error":"{e}"}}"#)),
        };
        let name = params
            .get("name")
            .cloned()
            .unwrap_or_else(|| source_type.to_string());
        let check_interval: u64 = params
            .get("check_interval_seconds")
            .and_then(|v| v.parse().ok())
            .unwrap_or(86400);
        let clean_params: HashMap<String, String> = params
            .into_iter()
            .filter(|(k, _)| k != "name" && k != "check_interval_seconds")
            .collect();

        let config = RemoteSourceConfig {
            source_type: source_type.to_string(),
            name: name.clone(),
            params: clean_params,
            check_interval_seconds: check_interval,
        };

        let mut sources = self.staged_sources.lock().unwrap();
        if let Some(pos) = sources.iter().position(|s| s.name == name) {
            sources[pos] = config;
        } else {
            sources.push(config);
        }
        json_response(201, r#"{"ok":true}"#)
    }

    fn api_delete_source(&self, name: &str) -> Response<Box<dyn std::io::Read + Send>> {
        let mut sources = self.staged_sources.lock().unwrap();
        sources.retain(|s| s.name != name);
        json_response(200, r#"{"ok":true}"#)
    }

    fn api_status(&self) -> Response<Box<dyn std::io::Read + Send>> {
        let sources = self.staged_sources.lock().unwrap();
        let names: Vec<_> = sources.iter().map(|s| &s.name).collect();
        let json = serde_json::json!({
            "configured_sources": names,
            "count": sources.len(),
        });
        json_response(200, &json.to_string())
    }

    fn api_finish(&self) -> Response<Box<dyn std::io::Read + Send>> {
        self.finished.store(true, Ordering::Relaxed);
        json_response(
            200,
            r#"{"ok":true,"message":"Config saved. Server shutting down."}"#,
        )
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn json_response(code: u16, body: &str) -> Response<Box<dyn std::io::Read + Send>> {
    Response::from_string(body)
        .with_status_code(StatusCode(code))
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
        .boxed()
}

fn serve_html(body: String) -> Response<Box<dyn std::io::Read + Send>> {
    Response::from_string(body)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        )
        .boxed()
}

fn read_body(request: &mut Request) -> String {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    body
}

fn base64_encode(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0) as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// ---------------------------------------------------------------------------
// HTML pages
// ---------------------------------------------------------------------------

const STYLE: &str = r#"
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:system-ui,sans-serif;background:#111;color:#eee;max-width:480px;margin:0 auto;padding:8px}
nav{display:flex;gap:8px;margin-bottom:16px;border-bottom:1px solid #333;padding-bottom:8px}
nav a{color:#8af;text-decoration:none;padding:4px 8px;border-radius:4px}
nav a.active,nav a:hover{background:#222}
h1{font-size:1.2em;margin-bottom:12px}
.card{background:#1a1a1a;border:1px solid #333;border-radius:6px;padding:12px;margin-bottom:12px}
.card h2{font-size:1em;margin-bottom:8px}
form label{display:block;font-size:.85em;margin:8px 0 4px;color:#aaa}
form input,form select{width:100%;padding:8px;background:#222;border:1px solid #444;border-radius:4px;color:#eee;font-size:1em;margin-bottom:8px}
button,.btn{display:inline-block;padding:8px 16px;background:#246;color:#eee;border:none;border-radius:4px;font-size:1em;cursor:pointer;text-decoration:none}
button:hover,.btn:hover{background:#358}
.danger{background:#622}.danger:hover{background:#844}
.row{display:flex;justify-content:space-between;align-items:center;gap:8px}
.muted{color:#888;font-size:.85em}
"#;

fn nav_bar(active: &str) -> String {
    let pages = [
        ("/", "Home"),
        ("/sources", "Sources"),
        ("/sources/add", "Add"),
    ];
    let mut links = String::new();
    for (href, label) in &pages {
        let cls = if *href == active { "active" } else { "" };
        links.push_str(&format!(r#"<a href="{href}" class="{cls}">{label}</a>"#));
    }
    format!("<nav>{links}</nav>")
}

fn page_wrap(active: &str, title: &str, content: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>{STYLE}</style></head><body>{nav}{content}</body></html>"#,
        nav = nav_bar(active),
    )
}

fn page_dashboard(staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>) -> String {
    let sources = staged.lock().unwrap();
    let count = sources.len();
    let list: String = sources
        .iter()
        .map(|s| {
            format!(
                r#"<div class="card"><h2>{}</h2><p class="muted">Type: {}</p></div>"#,
                s.name, s.source_type
            )
        })
        .collect::<Vec<_>>()
        .join("");

    let body = format!(
        r#"<h1>Photo Frame Setup</h1><div class="card"><h2>Configured Sources</h2><p>{count} source(s) configured</p></div>{list}<form method="post" action="/api/finish"><button>Finish Setup</button></form>"#
    );
    page_wrap("/", "Dashboard", &body)
}

fn page_sources(staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>) -> String {
    let sources = staged.lock().unwrap();
    let list: String = if sources.is_empty() {
        r#"<p class="muted">No sources configured.</p>"#.to_string()
    } else {
        sources
            .iter()
            .map(|s| {
                format!(
                    r#"<div class="card"><div class="row"><div><h2>{}</h2><p class="muted">Type: {}</p></div><form method="post" action="/api/sources/{}" style="margin:0"><button class="danger">Delete</button></form></div></div>"#,
                    s.name, s.source_type,
                    urlencode(&s.name),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let body = format!(
        r#"<h1>Sources</h1>{list}<a href="/sources/add" class="btn">+ Add Source</a><br><br><form method="post" action="/api/finish"><button>Finish Setup</button></form>"#
    );
    page_wrap("/sources", "Sources", &body)
}

fn page_add_source() -> String {
    let body = r#"<h1>Add Source</h1><div class="card"><a href="/sources/dropbox" class="btn">Dropbox</a></div><div class="card"><a href="/sources/google-drive" class="btn">Google Drive</a></div>"#;
    page_wrap("/sources/add", "Add Source", body)
}

fn page_form_dropbox() -> String {
    let body = r#"<h1>Add Dropbox Source</h1><form method="post" action="/api/sources/dropbox"><label>Name</label><input name="name" placeholder="My Dropbox"><label>Access Token</label><input name="access_token" type="password" placeholder="sl.xxxx"><label>Folder</label><input name="folder" value="/Photos"><label>Check Interval (seconds)</label><input name="check_interval_seconds" value="86400" type="number"><br><button>Save</button></form>"#;
    page_wrap("/sources/dropbox", "Dropbox", body)
}

fn page_form_gdrive() -> String {
    let body = r#"<h1>Add Google Drive Source</h1><p class="muted">Requires a publicly shared folder and a Google Cloud API key.</p><form method="post" action="/api/sources/google-drive"><label>Name</label><input name="name" placeholder="My Drive"><label>API Key</label><input name="api_key" type="password" placeholder="AIza..."><label>Folder ID</label><input name="folder_id" placeholder="1abc123..."><label>Check Interval (seconds)</label><input name="check_interval_seconds" value="86400" type="number"><br><button>Save</button></form>"#;
    page_wrap("/sources/google-drive", "Google Drive", body)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_free_port() -> io::Result<u16> {
    for port in 8100..8200 {
        if TcpListener::bind(format!("0.0.0.0:{port}")).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::other("No available port in range 8100-8199"))
}

fn urlencode(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char)
            }
            _ => result.push_str(&format!("%{:02X}", b)),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(
            base64_encode("photo-frame:test"),
            "cGhvdG8tZnJhbWU6dGVzdA=="
        );
    }
}
