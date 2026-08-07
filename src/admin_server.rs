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
use httparse::{Request, Status};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct AdminServer {
    listener: TcpListener,
    port: u16,
    password: String,
    staged_sources: Arc<Mutex<Vec<RemoteSourceConfig>>>,
    finished: Arc<AtomicBool>,
    last_request: Arc<Mutex<Instant>>,
}

impl AdminServer {
    pub fn new(password: String) -> io::Result<Self> {
        let (listener, port) = bind_random_port()?;

        Ok(AdminServer {
            listener,
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
        self.listener
            .set_nonblocking(true)
            .map_err(io::Error::other)?;

        loop {
            if shutdown.load(Ordering::Relaxed) || self.finished.load(Ordering::Relaxed) {
                break;
            }

            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let password = self.password.clone();
                    let staged = self.staged_sources.clone();
                    let finished = self.finished.clone();
                    let last = self.last_request.clone();
                    std::thread::spawn(move || {
                        handle_connection(&mut stream, &password, &staged, &finished, &last);
                    });
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }

        Ok(())
    }
}

fn handle_connection(
    stream: &mut dyn ReadWrite,
    password: &str,
    staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>,
    finished: &Arc<AtomicBool>,
    last: &Arc<Mutex<Instant>>,
) {
    *last.lock().unwrap() = Instant::now();

    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };

    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut req = Request::new(&mut headers);
    let _body_offset = match req.parse(&buf[..n]) {
        Ok(Status::Complete(off)) => off,
        _ => {
            respond_text(stream, 400, "Bad Request");
            return;
        }
    };

    let method = req.method.unwrap_or("GET");
    let path = req.path.unwrap_or("/");

    // Basic auth check (skip for OAuth callback)
    if !path.starts_with("/oauth/") && !check_auth(&req, password) {
        respond_text(stream, 401, "Unauthorized");
        return;
    }

    match (method, path) {
        // ---- API routes ---------------------------------------------------
        ("GET", "/api/sources") => api_list_sources(stream, staged),
        ("POST", "/api/sources/dropbox") => api_save_source(stream, staged, "dropbox", &buf, n),
        ("POST", "/api/sources/google-drive") => {
            api_save_source(stream, staged, "google_drive", &buf, n)
        }
        ("DELETE", p) if p.starts_with("/api/sources/") => {
            let name = &p["/api/sources/".len()..];
            api_delete_source(stream, staged, name);
        }
        ("GET", "/api/status") => api_status(stream, staged),
        ("POST", "/api/finish") => api_finish(stream, finished),

        // ---- HTML pages ---------------------------------------------------
        ("GET", "/") => serve_html(stream, page_dashboard(staged)),
        ("GET", "/sources") => serve_html(stream, page_sources(staged)),
        ("GET", "/sources/add") => serve_html(stream, page_add_source()),
        ("GET", "/sources/dropbox") => serve_html(stream, page_form_dropbox()),
        ("GET", "/sources/google-drive") => serve_html(stream, page_form_gdrive()),

        _ => respond_text(stream, 404, "Not Found"),
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn check_auth(req: &Request, password: &str) -> bool {
    for header in req.headers.iter() {
        if header.name.eq_ignore_ascii_case("Authorization") {
            let val = String::from_utf8_lossy(header.value);
            let expected = format!(
                "Basic {}",
                base64_encode(&format!("photo-frame:{}", password))
            );
            return val.trim() == expected;
        }
    }
    false
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
// HTTP response helpers
// ---------------------------------------------------------------------------

fn respond_text(stream: &mut dyn ReadWrite, code: u16, body: &str) {
    let status = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Unknown",
    };
    let resp = format!(
        "HTTP/1.1 {code} {status}\r\nContent-Type: text/plain\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn respond_json(stream: &mut dyn ReadWrite, code: u16, body: &str) {
    let status = match code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Unknown",
    };
    let resp = format!(
        "HTTP/1.1 {code} {status}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn serve_html(stream: &mut dyn ReadWrite, body: String) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = stream.write_all(resp.as_bytes());
}

// ---------------------------------------------------------------------------
// API handlers
// ---------------------------------------------------------------------------

fn api_list_sources(stream: &mut dyn ReadWrite, staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>) {
    let sources = staged.lock().unwrap();
    let json = serde_json::to_string(&*sources).unwrap_or_else(|_| "[]".into());
    respond_json(stream, 200, &json);
}

fn api_save_source(
    stream: &mut dyn ReadWrite,
    staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>,
    source_type: &str,
    buf: &[u8],
    n: usize,
) {
    let body = extract_body(buf, n);
    let params: HashMap<String, String> = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            respond_json(stream, 400, &format!(r#"{{"error":"{e}"}}"#));
            return;
        }
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

    let mut sources = staged.lock().unwrap();
    // Replace existing source with the same name, or append
    if let Some(pos) = sources.iter().position(|s| s.name == name) {
        sources[pos] = config;
    } else {
        sources.push(config);
    }
    respond_json(stream, 201, r#"{"ok":true}"#);
}

fn api_delete_source(
    stream: &mut dyn ReadWrite,
    staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>,
    name: &str,
) {
    let mut sources = staged.lock().unwrap();
    sources.retain(|s| s.name != name);
    respond_json(stream, 200, r#"{"ok":true}"#);
}

fn api_status(stream: &mut dyn ReadWrite, staged: &Arc<Mutex<Vec<RemoteSourceConfig>>>) {
    let sources = staged.lock().unwrap();
    let names: Vec<_> = sources.iter().map(|s| &s.name).collect();
    let json = serde_json::json!({
        "configured_sources": names,
        "count": sources.len(),
    });
    respond_json(stream, 200, &json.to_string());
}

fn api_finish(stream: &mut dyn ReadWrite, finished: &Arc<AtomicBool>) {
    finished.store(true, Ordering::Relaxed);
    respond_json(
        stream,
        200,
        r#"{"ok":true,"message":"Config saved. Server shutting down."}"#,
    );
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

fn bind_random_port() -> io::Result<(TcpListener, u16)> {
    for port in 8100..8200 {
        if let Ok(listener) = TcpListener::bind(format!("0.0.0.0:{port}")) {
            return Ok((listener, port));
        }
    }
    Err(io::Error::other("No available port in range 8100-8199"))
}

fn extract_body(buf: &[u8], n: usize) -> String {
    let data = &buf[..n];
    if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
        String::from_utf8_lossy(&data[pos + 4..]).to_string()
    } else {
        String::new()
    }
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

// ---------------------------------------------------------------------------
// Trait for testability
// ---------------------------------------------------------------------------

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct MockStream {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for MockStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for MockStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn mk_request(path: &str, body: Option<&str>) -> String {
        let auth_str = base64_encode("photo-frame:test");
        let body_part = body.unwrap_or("");
        format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic {auth_str}\r\nContent-Length: {cl}\r\n\r\n{body_part}",
            cl = body_part.len(),
        )
    }

    #[test]
    fn test_dashboard_page() {
        let mut stream = MockStream {
            input: Cursor::new(mk_request("/", None).into_bytes()),
            output: Vec::new(),
        };
        let staged = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let last = Arc::new(Mutex::new(Instant::now()));

        handle_connection(&mut stream, "test", &staged, &finished, &last);

        let output = String::from_utf8_lossy(&stream.output);
        assert!(output.contains("200 OK"));
        assert!(output.contains("text/html"));
        assert!(output.contains("Photo Frame Setup"));
    }

    #[test]
    fn test_source_not_found() {
        let mut stream = MockStream {
            input: Cursor::new(mk_request("/api/sources", None).into_bytes()),
            output: Vec::new(),
        };
        let staged = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let last = Arc::new(Mutex::new(Instant::now()));

        handle_connection(&mut stream, "test", &staged, &finished, &last);

        let output = String::from_utf8_lossy(&stream.output);
        assert!(output.contains("200 OK"));
        assert!(output.contains("[]"));
    }
}
