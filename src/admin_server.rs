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
use std::collections::HashMap;
use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tiny_http_fork::{Header, Method, Request, Response, Server, StatusCode};

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
#[template(path = "gdrive_oauth_form.html")]
struct GdriveOauthFormTemplate {
    redirect_uri: String,
}

#[derive(Template)]
#[template(path = "oauth_success.html")]
struct OauthSuccessTemplate {
    name: String,
}

#[derive(Template)]
#[template(path = "oauth_error.html")]
struct OauthErrorTemplate {
    message: String,
}

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
            (Method::Get, "/sources/google-drive-oauth") => {
                let redirect_uri = format!(
                    "http://photo-frame.local:{}/oauth/google/callback",
                    self.port
                );
                serve_template(&GdriveOauthFormTemplate { redirect_uri })
            }
            (Method::Get, p) if p.starts_with("/oauth/google/callback") => {
                self.handle_oauth_callback(request)
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

    fn handle_oauth_callback(&self, request: &Request) -> Response<Box<dyn std::io::Read + Send>> {
        let url = request.url();
        let code = url
            .split("?code=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .unwrap_or("");

        if code.is_empty() {
            return serve_template(&OauthErrorTemplate {
                message: "No authorization code received".into(),
            });
        }

        // We need the client_id and client_secret from the form submission.
        // They are stored in the most recent OAuth source entry.
        let oauth_params = {
            let sources = self.staged_sources.lock().unwrap();
            sources
                .iter()
                .find(|s| s.source_type == "google_drive" && s.params.contains_key("oauth_pending"))
                .map(|s| (s.params.clone(), s.name.clone()))
        };

        let (params, name) = match oauth_params {
            Some(p) => p,
            None => {
                return serve_template(&OauthErrorTemplate {
                    message: "No OAuth setup found. Start from /sources/google-drive-oauth first."
                        .into(),
                });
            }
        };

        let client_id = params.get("client_id").cloned().unwrap_or_default();
        let client_secret = params.get("client_secret").cloned().unwrap_or_default();
        let redirect_uri = format!(
            "http://photo-frame.local:{}/oauth/google/callback",
            self.port
        );

        match crate::import::gdrive::exchange_code(code, &client_id, &client_secret, &redirect_uri)
        {
            Ok(refresh_token) => {
                // Replace the OAuth-pending entry with the real config
                let mut clean = params.clone();
                clean.remove("oauth_pending");
                clean.insert("refresh_token".into(), refresh_token);

                let interval = clean
                    .get("check_interval_seconds")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(86400);

                let config = RemoteSourceConfig {
                    source_type: "google_drive".into(),
                    name: name.clone(),
                    params: clean,
                    check_interval_seconds: interval,
                };

                let mut sources = self.staged_sources.lock().unwrap();
                if let Some(pos) = sources.iter().position(|s| s.name == name) {
                    sources[pos] = config;
                } else {
                    sources.push(config);
                }

                serve_template(&OauthSuccessTemplate { name: name.clone() })
            }
            Err(e) => serve_template(&OauthErrorTemplate {
                message: format!("Failed to exchange code: {e}"),
            }),
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

    #[test]
    fn test_base64_encode() {
        assert_eq!(
            base64_encode("photo-frame:test"),
            "cGhvdG8tZnJhbWU6dGVzdA=="
        );
    }
}
