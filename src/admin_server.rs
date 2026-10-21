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
use askama::Template;
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tiny_http_fork::{Header, Method, Request, Response, Server, StatusCode};

/// File the remote-sync scheduler writes to report per-source status. It lives
/// next to the photo index in `photos_dir`.
pub const SYNC_STATUS_FILE: &str = "sync-status.json";

/// Basic-auth username for the config-mode admin server. Shown on the setup
/// screen and written to the fallback credentials file.
pub const AUTH_USERNAME: &str = "photo-frame";

// ---------------------------------------------------------------------------
// Askama templates — compiled from templates/*.html at build time
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate<'a> {
    sources: &'a Vec<RemoteSourceConfig>,
}

#[derive(Template)]
#[template(path = "sources.html")]
struct SourcesTemplate<'a> {
    sources: &'a Vec<RemoteSourceConfig>,
}

#[derive(Template)]
#[template(path = "add_source.html")]
struct AddSourceTemplate;

#[derive(Template)]
#[template(path = "dropbox_form.html")]
struct DropboxFormTemplate;

#[derive(Template)]
#[template(path = "gdrive_form.html")]
struct GdriveFormTemplate;

#[derive(Template)]
#[template(path = "gdrive_service_account_form.html")]
struct GdriveServiceAccountFormTemplate;

#[derive(Template)]
#[template(path = "finish.html")]
struct FinishTemplate;

pub struct AdminServer {
    port: u16,
    password: String,
    staged_sources: Arc<Mutex<Vec<RemoteSourceConfig>>>,
    finished: Arc<AtomicBool>,
    last_request: Arc<Mutex<Instant>>,
    status_path: Option<PathBuf>,
}

impl AdminServer {
    /// Start a server that stages `staged_sources`. Config mode seeds this with
    /// the sources already in `config.toml`, so existing entries can be edited
    /// or removed and are not silently lost.
    pub fn new(password: String, staged_sources: Vec<RemoteSourceConfig>) -> io::Result<Self> {
        let port = find_free_port()?;

        Ok(AdminServer {
            port,
            password,
            staged_sources: Arc::new(Mutex::new(staged_sources)),
            finished: Arc::new(AtomicBool::new(false)),
            last_request: Arc::new(Mutex::new(Instant::now())),
            status_path: None,
        })
    }

    /// Point `/api/status` at the scheduler's `sync-status.json`. Without this,
    /// the status endpoint reports an empty `sync` object.
    pub fn with_status_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.status_path = Some(path.into());
        self
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

        // Every route, including form pages, is behind basic auth. Config mode
        // is only reachable from the LAN and the password rotates per session.
        if !self.check_auth(request) {
            return Response::from_string("Unauthorized")
                .with_status_code(StatusCode::from(401))
                .with_header(
                    Header::from_bytes(
                        &b"WWW-Authenticate"[..],
                        &b"Basic realm=\"Photo Frame Setup\""[..],
                    )
                    .unwrap(),
                )
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
                let name = decode_path_segment(&p["/api/sources/".len()..]);
                self.api_delete_source(&name)
            }
            // HTML forms cannot send DELETE, so allow POST for browser deletes.
            (Method::Post, p) if p.starts_with("/api/sources/") => {
                let name = decode_path_segment(&p["/api/sources/".len()..]);
                let response = self.api_delete_source(&name);
                if is_form_request(request) {
                    redirect_response("/sources")
                } else {
                    response
                }
            }
            (Method::Get, "/api/status") => self.api_status(),
            (Method::Post, "/api/finish") => self.api_finish(request),

            // ---- HTML pages ---------------------------------------------------
            (Method::Get, "/") => {
                let sources = self.staged_sources.lock().unwrap();
                serve_template(&DashboardTemplate { sources: &sources })
            }
            (Method::Get, "/sources") => {
                let sources = self.staged_sources.lock().unwrap();
                serve_template(&SourcesTemplate { sources: &sources })
            }
            (Method::Get, "/sources/add") => serve_template(&AddSourceTemplate),
            (Method::Get, "/sources/dropbox") => serve_template(&DropboxFormTemplate),
            (Method::Get, "/sources/google-drive") => serve_template(&GdriveFormTemplate),
            (Method::Get, "/sources/google-drive-service-account") => {
                serve_template(&GdriveServiceAccountFormTemplate)
            }

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
                    base64_encode(&format!("{AUTH_USERNAME}:{}", self.password))
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
        let form = is_form_request(request);
        let params = match parse_params(request) {
            Ok(p) => p,
            Err(e) => {
                let message = format!("Invalid form data: {e}");
                return if form {
                    html_error(400, &message)
                } else {
                    json_error(400, &message)
                };
            }
        };
        let name = params
            .get("name")
            .filter(|n| !n.trim().is_empty())
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

        // Catch a truncated or malformed service-account key at save time so
        // the user sees the problem on the form instead of at Finish.
        if source_type == "google_drive" {
            if let Err(message) = validate_gdrive_params(&clean_params) {
                return if form {
                    html_error(400, &message)
                } else {
                    json_error(400, &message)
                };
            }
        }

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

        if form {
            redirect_response("/sources")
        } else {
            json_response(201, r#"{"ok":true}"#)
        }
    }

    fn api_delete_source(&self, name: &str) -> Response<Box<dyn std::io::Read + Send>> {
        let mut sources = self.staged_sources.lock().unwrap();
        sources.retain(|s| s.name != name);
        json_response(200, r#"{"ok":true}"#)
    }

    fn api_status(&self) -> Response<Box<dyn std::io::Read + Send>> {
        let sources = self.staged_sources.lock().unwrap();
        let names: Vec<_> = sources.iter().map(|s| &s.name).collect();
        let sync = self.read_sync_status();
        let json = serde_json::json!({
            "configured_sources": names,
            "count": sources.len(),
            "sources": &*sources,
            "sync": sync,
        });
        json_response(200, &json.to_string())
    }

    /// Read `<photos_dir>/sync-status.json` if the scheduler has written one.
    /// A missing or malformed file is reported as an empty object so the admin
    /// UI keeps working before the first sync.
    fn read_sync_status(&self) -> serde_json::Value {
        let empty = serde_json::json!({});
        let Some(path) = self.status_path.as_ref() else {
            return empty;
        };

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return empty,
            Err(e) => {
                log::warn!("Could not read sync status {}: {e}", path.display());
                return empty;
            }
        };

        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value @ serde_json::Value::Object(_)) => value,
            Ok(_) => {
                log::warn!(
                    "Sync status {} is not a JSON object; ignoring it",
                    path.display()
                );
                empty
            }
            Err(e) => {
                log::warn!("Could not parse sync status {}: {e}", path.display());
                empty
            }
        }
    }

    fn api_finish(&self, request: &Request) -> Response<Box<dyn std::io::Read + Send>> {
        let html = wants_html(request);
        let sources = self.staged_sources.lock().unwrap().clone();

        if let Err(message) = validate_sources(&sources) {
            return if html {
                html_error(400, &message)
            } else {
                json_error(400, &message)
            };
        }

        self.finished.store(true, Ordering::Relaxed);
        if html {
            serve_template(&FinishTemplate)
        } else {
            json_response(
                200,
                r#"{"ok":true,"message":"Config saved. Server shutting down."}"#,
            )
        }
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

/// A 303 redirect. Browsers switch to a GET request for the target page, which
/// avoids resubmitting the form on refresh.
fn redirect_response(location: &str) -> Response<Box<dyn std::io::Read + Send>> {
    Response::from_string("")
        .with_status_code(StatusCode(303))
        .with_header(Header::from_bytes(&b"Location"[..], location.as_bytes()).unwrap())
        .boxed()
}

fn html_error(code: u16, message: &str) -> Response<Box<dyn std::io::Read + Send>> {
    let body = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Error</title></head>\
         <body><h1>Error</h1><p>{}</p><a href=\"/sources\">Back to Sources</a></body></html>",
        escape_html(message)
    );
    Response::from_string(body)
        .with_status_code(StatusCode(code))
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        )
        .boxed()
}

fn json_error(code: u16, message: &str) -> Response<Box<dyn std::io::Read + Send>> {
    let body = serde_json::json!({ "error": message }).to_string();
    json_response(code, &body)
}

/// A non-empty, trimmed parameter value.
fn param<'a>(params: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

/// Validate the staged source list at "Finish" time.
///
/// An empty list is valid: it means the user removed every source. Sources of
/// unknown types are accepted (other integrations may use the same schema),
/// but Dropbox and Google Drive entries must be complete enough to sync.
fn validate_sources(sources: &[RemoteSourceConfig]) -> Result<(), String> {
    let mut names = HashSet::new();

    for (index, source) in sources.iter().enumerate() {
        let name = source.name.trim();
        if name.is_empty() {
            return Err(format!("Source {} has an empty name.", index + 1));
        }
        if !names.insert(name) {
            return Err(format!("Source name \"{name}\" is used more than once."));
        }
        if source.check_interval_seconds == 0 {
            return Err(format!(
                "\"{name}\": check interval must be greater than 0 seconds."
            ));
        }

        match source.source_type.as_str() {
            "dropbox" => {
                if param(&source.params, "access_token").is_none() {
                    return Err(format!("\"{name}\": Dropbox access token is required."));
                }
            }
            "google_drive" => {
                validate_gdrive_params(&source.params)
                    .map_err(|message| format!("\"{name}\": {message}"))?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Validate the Google Drive portion of a source: it needs a folder and
/// exactly one authentication method. The service-account JSON is checked for
/// shape only; the key signature is verified when the source connects.
fn validate_gdrive_params(params: &HashMap<String, String>) -> Result<(), String> {
    if param(params, "folder_id").is_none() {
        return Err("Google Drive folder ID is required.".into());
    }

    let has_api_key = param(params, "api_key").is_some();
    let oauth_fields = ["refresh_token", "client_id", "client_secret"];
    let present_oauth: Vec<&str> = oauth_fields
        .iter()
        .copied()
        .filter(|field| param(params, field).is_some())
        .collect();
    if !present_oauth.is_empty() && present_oauth.len() != oauth_fields.len() {
        return Err(
            "Google Drive OAuth credentials are incomplete: refresh token, client ID, \
             and client secret are all required."
                .into(),
        );
    }
    let has_oauth = present_oauth.len() == oauth_fields.len();
    let service_account_json = param(params, "service_account_json");

    let methods =
        u8::from(has_api_key) + u8::from(has_oauth) + u8::from(service_account_json.is_some());
    if methods == 0 {
        return Err(
            "Google Drive requires one authentication method: an API key, a service \
             account JSON key, or a complete OAuth refresh token, client ID, and client \
             secret."
                .into(),
        );
    }
    if methods > 1 {
        return Err(
            "Google Drive accepts exactly one authentication method: API key, service \
             account key, or OAuth refresh token."
                .into(),
        );
    }

    if let Some(json) = service_account_json {
        crate::import::gdrive::ServiceAccountKey::parse(json)?;
    }

    Ok(())
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Look up a request header (case-insensitive field name).
fn header_value<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

fn is_form_request(request: &Request) -> bool {
    header_value(request, "Content-Type")
        .map(|ct| {
            ct.to_ascii_lowercase()
                .starts_with("application/x-www-form-urlencoded")
        })
        .unwrap_or(false)
}

/// Whether the client is a browser looking for an HTML response. Form posts
/// always qualify; plain GET/POST clients can opt in with an Accept header.
fn wants_html(request: &Request) -> bool {
    is_form_request(request)
        || header_value(request, "Accept")
            .map(|accept| accept.contains("text/html"))
            .unwrap_or(false)
}

/// Parse a request body as JSON or `application/x-www-form-urlencoded`.
fn parse_params(request: &mut Request) -> Result<HashMap<String, String>, String> {
    let content_type = header_value(request, "Content-Type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = read_body(request);

    if content_type.contains("json")
        || (content_type.is_empty() && body.trim_start().starts_with('{'))
    {
        serde_json::from_str(&body).map_err(|e| e.to_string())
    } else {
        serde_urlencoded::from_str(&body).map_err(|e| e.to_string())
    }
}

/// Percent-decode a path segment produced by the `urlencode` template filter.
fn decode_path_segment(segment: &str) -> String {
    form_urlencoded::parse(segment.as_bytes())
        .next()
        .map(|(name, _)| name.into_owned())
        .unwrap_or_else(|| segment.to_string())
}

fn serve_html(body: String) -> Response<Box<dyn std::io::Read + Send>> {
    Response::from_string(body)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        )
        .boxed()
}

fn serve_template<T: Template>(t: &T) -> Response<Box<dyn std::io::Read + Send>> {
    match t.render() {
        Ok(html) => serve_html(html),
        Err(e) => Response::from_string(format!("Template error: {e}"))
            .with_status_code(StatusCode(500))
            .boxed(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn source(
        source_type: &str,
        name: &str,
        params: &[(&str, &str)],
        interval: u64,
    ) -> RemoteSourceConfig {
        RemoteSourceConfig {
            source_type: source_type.to_string(),
            name: name.to_string(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            check_interval_seconds: interval,
        }
    }

    /// Shape-only service-account key for validation tests. The private key is
    /// not a real PEM because `validate_sources` never signs anything.
    fn test_service_account_json() -> &'static str {
        r#"{"type":"service_account","client_email":"frame@example.iam.gserviceaccount.com","private_key":"-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----","token_uri":"https://oauth2.googleapis.com/token"}"#
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(
            base64_encode("photo-frame:test"),
            "cGhvdG8tZnJhbWU6dGVzdA=="
        );
    }

    #[test]
    fn test_validate_allows_empty_list() {
        // Removing every source is a supported action.
        assert!(validate_sources(&[]).is_ok());
    }

    #[test]
    fn test_validate_dropbox_requires_access_token() {
        let ok = source("dropbox", "DB", &[("access_token", "sl.x")], 3600);
        assert!(validate_sources(&[ok]).is_ok());

        let missing = source("dropbox", "DB", &[("folder", "/Photos")], 3600);
        let err = validate_sources(&[missing]).unwrap_err();
        assert!(err.contains("access token"), "got: {err}");

        let blank = source("dropbox", "DB", &[("access_token", "   ")], 3600);
        assert!(validate_sources(&[blank]).is_err());
    }

    #[test]
    fn test_validate_rejects_empty_and_duplicate_names() {
        let no_name = source("dropbox", "  ", &[("access_token", "sl.x")], 3600);
        let err = validate_sources(&[no_name]).unwrap_err();
        assert!(err.contains("empty name"), "got: {err}");

        let first = source("dropbox", "Photos", &[("access_token", "sl.x")], 3600);
        let second = source("dropbox", " Photos ", &[("access_token", "sl.y")], 3600);
        let err = validate_sources(&[first, second]).unwrap_err();
        assert!(err.contains("more than once"), "got: {err}");
    }

    #[test]
    fn test_validate_rejects_zero_interval() {
        let source = source("dropbox", "DB", &[("access_token", "sl.x")], 0);
        let err = validate_sources(&[source]).unwrap_err();
        assert!(err.contains("check interval"), "got: {err}");
    }

    #[test]
    fn test_validate_google_drive() {
        let missing_folder = source("google_drive", "GD", &[("api_key", "AIza")], 3600);
        let err = validate_sources(&[missing_folder]).unwrap_err();
        assert!(err.contains("folder ID"), "got: {err}");

        let no_auth = source("google_drive", "GD", &[("folder_id", "f1")], 3600);
        let err = validate_sources(&[no_auth]).unwrap_err();
        assert!(err.contains("authentication method"), "got: {err}");

        let api_key = source(
            "google_drive",
            "GD",
            &[("folder_id", "f1"), ("api_key", "AIza")],
            3600,
        );
        assert!(validate_sources(&[api_key]).is_ok());

        let oauth = source(
            "google_drive",
            "GD",
            &[
                ("folder_id", "f1"),
                ("refresh_token", "1//token"),
                ("client_id", "id"),
                ("client_secret", "secret"),
            ],
            3600,
        );
        assert!(validate_sources(&[oauth]).is_ok());

        let incomplete_oauth = source(
            "google_drive",
            "GD",
            &[
                ("folder_id", "f1"),
                ("refresh_token", "1//token"),
                ("client_id", "id"),
            ],
            3600,
        );
        let err = validate_sources(&[incomplete_oauth]).unwrap_err();
        assert!(err.contains("incomplete"), "got: {err}");
    }

    #[test]
    fn test_validate_google_drive_service_account() {
        let valid = source(
            "google_drive",
            "GD",
            &[
                ("folder_id", "f1"),
                ("service_account_json", test_service_account_json()),
            ],
            3600,
        );
        assert!(validate_sources(&[valid]).is_ok());

        let malformed = source(
            "google_drive",
            "GD",
            &[("folder_id", "f1"), ("service_account_json", "{oops")],
            3600,
        );
        let err = validate_sources(&[malformed]).unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");

        let missing_email = source(
            "google_drive",
            "GD",
            &[
                ("folder_id", "f1"),
                (
                    "service_account_json",
                    r#"{"private_key":"-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----"}"#,
                ),
            ],
            3600,
        );
        let err = validate_sources(&[missing_email]).unwrap_err();
        assert!(err.contains("client_email"), "got: {err}");

        let missing_key = source(
            "google_drive",
            "GD",
            &[
                ("folder_id", "f1"),
                (
                    "service_account_json",
                    r#"{"client_email":"frame@example.iam.gserviceaccount.com"}"#,
                ),
            ],
            3600,
        );
        let err = validate_sources(&[missing_key]).unwrap_err();
        assert!(err.contains("private_key"), "got: {err}");
        assert!(
            !err.contains("BEGIN PRIVATE KEY"),
            "the error must not echo key material: {err}"
        );

        let two_methods = source(
            "google_drive",
            "GD",
            &[
                ("folder_id", "f1"),
                ("api_key", "AIza"),
                ("service_account_json", test_service_account_json()),
            ],
            3600,
        );
        let err = validate_sources(&[two_methods]).unwrap_err();
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn test_validate_accepts_unknown_source_types() {
        let other = source("immich", "Immich", &[("url", "https://example.test")], 3600);
        assert!(validate_sources(&[other]).is_ok());
    }

    #[test]
    fn test_read_sync_status_missing_or_malformed_is_empty() {
        let server = AdminServer {
            port: 0,
            password: String::new(),
            staged_sources: Arc::new(Mutex::new(Vec::new())),
            finished: Arc::new(AtomicBool::new(false)),
            last_request: Arc::new(Mutex::new(Instant::now())),
            status_path: Some(PathBuf::from("/nonexistent/sync-status.json")),
        };
        assert_eq!(server.read_sync_status(), serde_json::json!({}));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SYNC_STATUS_FILE);
        std::fs::write(&path, "not json").unwrap();
        let server = AdminServer {
            status_path: Some(path.clone()),
            ..server
        };
        assert_eq!(server.read_sync_status(), serde_json::json!({}));

        std::fs::write(&path, "[1,2,3]").unwrap();
        assert_eq!(server.read_sync_status(), serde_json::json!({}));
    }

    #[test]
    fn test_read_sync_status_passes_through_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SYNC_STATUS_FILE);
        std::fs::write(
            &path,
            r#"{"sources":{"My Dropbox":{"last_sync":123,"state":"ok"}}}"#,
        )
        .unwrap();

        let server = AdminServer {
            port: 0,
            password: String::new(),
            staged_sources: Arc::new(Mutex::new(Vec::new())),
            finished: Arc::new(AtomicBool::new(false)),
            last_request: Arc::new(Mutex::new(Instant::now())),
            status_path: Some(path),
        };
        let status = server.read_sync_status();
        assert_eq!(status["sources"]["My Dropbox"]["last_sync"], 123);
        assert_eq!(status["sources"]["My Dropbox"]["state"], "ok");
    }
}
