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

//! End-to-end tests for the admin HTTP server. Each test starts a real
//! `AdminServer` on its selected port and talks to it with `reqwest::blocking`.
//!
//! `AdminServer::new` picks a port from a small fixed range, so the tests are
//! serialized with a process-wide mutex to keep them deterministic. The server
//! is stopped with its `Arc<AtomicBool>` shutdown flag.

use photo_frame_manager::admin_server::AdminServer;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

static SERVER_LOCK: Mutex<()> = Mutex::new(());

const USERNAME: &str = "photo-frame";
const PASSWORD: &str = "testpass";

struct TestServer {
    port: u16,
    server: Arc<AdminServer>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    client: Client,
    // Held for the lifetime of the server so only one test uses the port range.
    _lock: MutexGuard<'static, ()>,
}

impl TestServer {
    fn start() -> Self {
        let lock = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = Arc::new(AdminServer::new(PASSWORD.to_string()).expect("create admin server"));
        let port = server.port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build HTTP client");

        let thread_server = server.clone();
        let thread_shutdown = shutdown.clone();
        let handle = thread::spawn(move || {
            thread_server
                .run(thread_shutdown)
                .expect("admin server should run");
        });

        // Wait until the listener accepts connections. The unauthenticated GET
        // returns 401 once the server is up, so a response means "ready".
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match client.get(format!("http://127.0.0.1:{port}/")).send() {
                Ok(_) => break,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Err(e) => panic!("admin server did not start on port {port}: {e}"),
            }
        }

        TestServer {
            port,
            server,
            shutdown,
            handle: Some(handle),
            client,
            _lock: lock,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    fn get(&self, path: &str) -> Response {
        self.client
            .get(self.url(path))
            .basic_auth(USERNAME, Some(PASSWORD))
            .send()
            .expect("GET request")
    }

    fn post_form(&self, path: &str, form: &[(&str, &str)]) -> Response {
        self.client
            .post(self.url(path))
            .basic_auth(USERNAME, Some(PASSWORD))
            .form(form)
            .send()
            .expect("form POST request")
    }

    fn post_empty_form(&self, path: &str) -> Response {
        self.client
            .post(self.url(path))
            .basic_auth(USERNAME, Some(PASSWORD))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("")
            .send()
            .expect("empty form POST request")
    }

    fn post_json(&self, path: &str, body: &str) -> Response {
        self.client
            .post(self.url(path))
            .basic_auth(USERNAME, Some(PASSWORD))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .expect("JSON POST request")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn location(response: &Response) -> String {
    response
        .headers()
        .get("Location")
        .expect("Location header")
        .to_str()
        .expect("Location should be ASCII")
        .to_string()
}

fn content_type(response: &Response) -> String {
    response
        .headers()
        .get("Content-Type")
        .expect("Content-Type header")
        .to_str()
        .expect("Content-Type should be ASCII")
        .to_string()
}

#[test]
fn form_dropbox_save_redirects_and_stages_source() {
    let s = TestServer::start();

    let response = s.post_form(
        "/api/sources/dropbox",
        &[
            ("name", "My Dropbox"),
            ("access_token", "sl.test-token"),
            ("folder", "/Photos"),
            ("check_interval_seconds", "3600"),
        ],
    );

    assert_eq!(response.status(), 303);
    assert_eq!(location(&response), "/sources");

    let sources = s.server.take_sources();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "My Dropbox");
    assert_eq!(sources[0].source_type, "dropbox");
    assert_eq!(sources[0].check_interval_seconds, 3600);
    assert_eq!(
        sources[0].params.get("access_token"),
        Some(&"sl.test-token".to_string())
    );
    assert_eq!(
        sources[0].params.get("folder"),
        Some(&"/Photos".to_string())
    );

    // The browser is redirected to the sources list, which renders the name.
    let page = s.get("/sources").text().expect("read sources page");
    assert!(page.contains("My Dropbox"), "sources page: {page}");
}

#[test]
fn json_dropbox_save_still_returns_201_json() {
    let s = TestServer::start();

    let response = s.post_json(
        "/api/sources/dropbox",
        r#"{"name":"API Dropbox","access_token":"sl.json","folder":"/J","check_interval_seconds":"900"}"#,
    );

    assert_eq!(response.status(), 201);
    assert!(content_type(&response).starts_with("application/json"));
    let body = response.text().expect("read JSON response");
    assert_eq!(body, r#"{"ok":true}"#);

    let sources = s.server.take_sources();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "API Dropbox");
    assert_eq!(sources[0].check_interval_seconds, 900);
}

#[test]
fn form_delete_via_post_redirects_and_removes_source() {
    let s = TestServer::start();

    let save = s.post_form(
        "/api/sources/dropbox",
        &[("name", "Delete Me"), ("access_token", "sl.x")],
    );
    assert_eq!(save.status(), 303);
    assert_eq!(s.server.take_sources().len(), 1);

    // The sources page URL-encodes the name in the form action.
    let response = s.post_empty_form("/api/sources/Delete%20Me");
    assert_eq!(response.status(), 303);
    assert_eq!(location(&response), "/sources");
    assert!(s.server.take_sources().is_empty());
}

#[test]
fn delete_api_still_returns_json_and_removes_source() {
    let s = TestServer::start();

    let save = s.post_form(
        "/api/sources/dropbox",
        &[("name", "API Delete"), ("access_token", "sl.y")],
    );
    assert_eq!(save.status(), 303);

    let response = s
        .client
        .delete(s.url("/api/sources/API%20Delete"))
        .basic_auth(USERNAME, Some(PASSWORD))
        .send()
        .expect("DELETE request");

    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("application/json"));
    assert_eq!(response.text().unwrap(), r#"{"ok":true}"#);
    assert!(s.server.take_sources().is_empty());
}

#[test]
fn finish_via_form_returns_html_and_sets_finished() {
    let s = TestServer::start();

    let response = s.post_empty_form("/api/finish");

    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("text/html"));
    let body = response.text().expect("read finish page");
    assert!(body.contains("Setup Complete"), "finish page: {body}");
    assert!(s.server.finished());
}

#[test]
fn finish_via_json_returns_json() {
    let s = TestServer::start();

    let response = s.post_json("/api/finish", "{}");

    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("application/json"));
    let body = response.text().expect("read finish JSON");
    assert!(body.contains(r#""ok":true"#), "finish body: {body}");
    assert!(s.server.finished());
}

#[test]
fn unauthenticated_requests_are_rejected() {
    let s = TestServer::start();

    let get = s
        .client
        .get(s.url("/"))
        .send()
        .expect("unauthenticated GET");
    assert_eq!(get.status(), 401);
    assert!(
        get.headers().get("WWW-Authenticate").is_some(),
        "401 must advertise basic auth so browsers prompt for credentials"
    );

    let post = s
        .client
        .post(s.url("/api/sources/dropbox"))
        .form(&[("name", "Sneaky"), ("access_token", "sl.x")])
        .send()
        .expect("unauthenticated form POST");
    assert_eq!(post.status(), 401);
    assert!(
        s.server.take_sources().is_empty(),
        "unauthenticated request must not stage a source"
    );
}
