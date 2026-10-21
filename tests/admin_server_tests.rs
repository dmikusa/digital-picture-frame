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
use photo_frame_manager::config::{AspectRatioMode, Config, RemoteSourceConfig};
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use std::collections::HashMap;
use std::path::PathBuf;
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

fn staged_source(source_type: &str, name: &str, params: &[(&str, &str)]) -> RemoteSourceConfig {
    RemoteSourceConfig {
        source_type: source_type.to_string(),
        name: name.to_string(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
        check_interval_seconds: 3600,
    }
}

/// A minimal valid config used as the base for `with_config_save`. Only
/// `remote_sources` changes when the user finishes setup.
fn base_config(photos_dir: &std::path::Path) -> Config {
    Config {
        photos_dir: photos_dir.to_path_buf(),
        display_socket_path: PathBuf::from("/run/photo-frame/photo-frame.sock"),
        control_socket_path: PathBuf::from("/run/photo-frame/control.sock"),
        native_resolution: "1024x768".to_string(),
        aspect_ratio_mode: AspectRatioMode::Fit,
        batch_delete_size: 10,
        log_max_size: 2048,
        log_max_files: 3,
        remote_sources: Vec::new(),
    }
}

impl TestServer {
    fn start() -> Self {
        Self::start_with(Vec::new())
    }

    fn start_with(sources: Vec<RemoteSourceConfig>) -> Self {
        Self::start_full(sources, None, None)
    }

    fn start_with_status(sources: Vec<RemoteSourceConfig>, status_path: Option<PathBuf>) -> Self {
        Self::start_full(sources, status_path, None)
    }

    fn start_with_config_save(
        sources: Vec<RemoteSourceConfig>,
        path: PathBuf,
        base: Config,
    ) -> Self {
        Self::start_full(sources, None, Some((path, base)))
    }

    fn start_full(
        sources: Vec<RemoteSourceConfig>,
        status_path: Option<PathBuf>,
        save_target: Option<(PathBuf, Config)>,
    ) -> Self {
        let lock = SERVER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut server =
            AdminServer::new(PASSWORD.to_string(), sources).expect("create admin server");
        if let Some(path) = status_path {
            server = server.with_status_path(path);
        }
        if let Some((path, base)) = save_target {
            server = server.with_config_save(path, base);
        }
        let server = Arc::new(server);
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
fn finish_with_save_target_writes_config_and_sets_finished() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let s = TestServer::start_with_config_save(
        vec![staged_source(
            "dropbox",
            "Saved Dropbox",
            &[("access_token", "sl.good"), ("folder", "/Photos")],
        )],
        config_path.clone(),
        base_config(dir.path()),
    );

    let response = s.post_empty_form("/api/finish");

    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("text/html"));
    assert!(s.server.finished());

    let saved = Config::from_file(&config_path).expect("saved config should parse");
    assert_eq!(saved.remote_sources.len(), 1);
    assert_eq!(saved.remote_sources[0].name, "Saved Dropbox");
    assert_eq!(
        saved.remote_sources[0].params.get("access_token"),
        Some(&"sl.good".to_string())
    );
}

#[test]
fn finish_save_failure_returns_error_and_leaves_unfinished() {
    let dir = tempfile::tempdir().unwrap();
    // A parent directory that does not exist makes the atomic write fail
    // without depending on file permissions (which root bypasses).
    let config_path = PathBuf::from("/nonexistent-photo-frame-dir/config.toml");
    let s = TestServer::start_with_config_save(
        vec![staged_source(
            "dropbox",
            "Valid Dropbox",
            &[("access_token", "sl.good")],
        )],
        config_path,
        base_config(dir.path()),
    );

    let form = s.post_empty_form("/api/finish");
    assert_eq!(form.status(), 500);
    assert!(
        content_type(&form).starts_with("text/html"),
        "browser save errors must be HTML, got {}",
        content_type(&form)
    );
    let body = form.text().expect("read error page");
    assert!(body.contains("Could not save config"), "error page: {body}");
    assert!(
        !s.server.finished(),
        "a failed save must not stop the server"
    );

    let json = s.post_json("/api/finish", "{}");
    assert_eq!(json.status(), 500);
    assert!(
        content_type(&json).starts_with("application/json"),
        "API save errors must be JSON, got {}",
        content_type(&json)
    );
    let body = json.text().expect("read error JSON");
    assert!(body.contains("Could not save config"), "error JSON: {body}");
    assert!(!s.server.finished());

    // The sources are still staged so the user can retry after fixing the path.
    assert_eq!(s.server.take_sources().len(), 1);
}

#[test]
fn preloaded_sources_appear_in_pages_and_api() {
    let s = TestServer::start_with(vec![staged_source(
        "dropbox",
        "Existing Dropbox",
        &[("access_token", "sl.preseeded")],
    )]);

    let page = s.get("/sources").text().expect("read sources page");
    assert!(page.contains("Existing Dropbox"), "sources page: {page}");

    let dashboard = s.get("/").text().expect("read dashboard");
    assert!(
        dashboard.contains("Existing Dropbox"),
        "dashboard: {dashboard}"
    );

    let response = s.get("/api/sources");
    assert_eq!(response.status(), 200);
    let body = response.text().expect("read sources JSON");
    assert!(body.contains("Existing Dropbox"), "sources JSON: {body}");

    // An existing source can be removed, not just newly added ones.
    let delete = s.post_empty_form("/api/sources/Existing%20Dropbox");
    assert_eq!(delete.status(), 303);
    assert!(s.server.take_sources().is_empty());
}

#[test]
fn finish_form_with_incomplete_google_drive_returns_html_error() {
    let s = TestServer::start_with(vec![staged_source(
        "google_drive",
        "Broken Drive",
        &[("api_key", "AIza-but-no-folder")],
    )]);

    let response = s.post_empty_form("/api/finish");

    assert_eq!(response.status(), 400);
    assert!(
        content_type(&response).starts_with("text/html"),
        "browser form errors must be HTML, got {}",
        content_type(&response)
    );
    let body = response.text().expect("read error page");
    assert!(body.contains("folder ID"), "error page: {body}");
    assert!(
        !s.server.finished(),
        "a failed validation must not stop the server"
    );
}

#[test]
fn finish_json_with_incomplete_source_returns_json_error() {
    let s = TestServer::start_with(vec![staged_source(
        "dropbox",
        "No Token",
        &[("folder", "/Photos")],
    )]);

    let response = s.post_json("/api/finish", "{}");

    assert_eq!(response.status(), 400);
    assert!(
        content_type(&response).starts_with("application/json"),
        "API errors must be JSON, got {}",
        content_type(&response)
    );
    let body = response.text().expect("read error JSON");
    assert!(body.contains("access token"), "error JSON: {body}");
    assert!(!s.server.finished());

    // The invalid source is still staged so the user can fix it.
    assert_eq!(s.server.take_sources().len(), 1);
}

#[test]
fn finish_with_duplicate_names_returns_error() {
    let s = TestServer::start_with(vec![
        staged_source("dropbox", "Dupe", &[("access_token", "sl.a")]),
        staged_source("dropbox", " Dupe ", &[("access_token", "sl.b")]),
    ]);

    let response = s.post_json("/api/finish", "{}");
    assert_eq!(response.status(), 400);
    let body = response.text().unwrap();
    assert!(body.contains("more than once"), "error JSON: {body}");
    assert!(!s.server.finished());
}

#[test]
fn finish_with_valid_preloaded_source_succeeds() {
    let s = TestServer::start_with(vec![staged_source(
        "dropbox",
        "Valid Dropbox",
        &[("access_token", "sl.good")],
    )]);

    let response = s.post_empty_form("/api/finish");

    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("text/html"));
    assert!(s.server.finished());
    let sources = s.server.take_sources();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "Valid Dropbox");
}

#[test]
fn status_includes_staged_sources_and_sync_file() {
    let dir = tempfile::tempdir().unwrap();
    let status_path = dir.path().join("sync-status.json");
    std::fs::write(
        &status_path,
        r#"{"sources":{"My Dropbox":{"last_sync":123,"last_attempt":120,"state":"ok","error":null}}}"#,
    )
    .unwrap();

    let s = TestServer::start_with_status(
        vec![staged_source(
            "dropbox",
            "My Dropbox",
            &[("access_token", "sl.x")],
        )],
        Some(status_path),
    );

    let response = s.get("/api/status");
    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("application/json"));

    let body: serde_json::Value =
        serde_json::from_str(&response.text().expect("read status")).expect("valid status JSON");
    assert_eq!(body["count"], 1);
    assert_eq!(body["sources"][0]["name"], "My Dropbox");
    assert_eq!(body["sync"]["sources"]["My Dropbox"]["last_sync"], 123);
    assert_eq!(body["sync"]["sources"]["My Dropbox"]["state"], "ok");
}

#[test]
fn status_without_sync_file_returns_empty_sync_object() {
    let s = TestServer::start();

    let body: serde_json::Value =
        serde_json::from_str(&s.get("/api/status").text().expect("read status"))
            .expect("valid status JSON");
    assert_eq!(body["sync"], serde_json::json!({}));
    assert_eq!(body["count"], 0);
}

/// Shape-only service-account key used by the admin tests. The private key is
/// not a real PEM because saving a source never signs anything.
fn service_account_json() -> String {
    r#"{"type":"service_account","project_id":"test-project","private_key_id":"key-id","private_key":"-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----","client_email":"frame@test-project.iam.gserviceaccount.com","token_uri":"https://oauth2.googleapis.com/token"}"#.to_string()
}

#[test]
fn add_source_page_links_the_service_account_form() {
    let s = TestServer::start();

    let page = s.get("/sources/add").text().expect("read add page");
    assert!(
        page.contains("Google Drive (Private, Service Account)"),
        "add page: {page}"
    );
    assert!(
        page.contains("/sources/google-drive-service-account"),
        "add page must link the service-account form: {page}"
    );
    assert!(
        !page.contains("/sources/google-drive-oauth"),
        "the old OAuth link must be gone: {page}"
    );
}

#[test]
fn service_account_form_renders_setup_instructions() {
    let s = TestServer::start();

    let response = s.get("/sources/google-drive-service-account");
    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("text/html"));
    let page = response.text().expect("read service-account form");

    assert!(page.contains("Service Account"), "form page: {page}");
    assert!(
        page.contains("name=\"service_account_json\""),
        "form must have a JSON textarea: {page}"
    );
    assert!(
        page.contains("name=\"folder_id\""),
        "form must ask for a folder ID: {page}"
    );
    assert!(
        page.contains("Enable the <b>Google Drive API</b>"),
        "form must explain enabling the Drive API: {page}"
    );
    assert!(
        page.contains("Viewer"),
        "form must explain sharing the folder as Viewer: {page}"
    );
    assert!(
        page.contains("cannot own"),
        "form must note that a service account cannot own files: {page}"
    );
}

#[test]
fn form_service_account_save_redirects_and_stages_source() {
    let s = TestServer::start();
    let key = service_account_json();

    let response = s.post_form(
        "/api/sources/google-drive",
        &[
            ("name", "Family Drive"),
            ("folder_id", "folder-123"),
            ("check_interval_seconds", "7200"),
            ("service_account_json", &key),
        ],
    );

    assert_eq!(response.status(), 303);
    assert_eq!(location(&response), "/sources");

    let sources = s.server.take_sources();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "Family Drive");
    assert_eq!(sources[0].source_type, "google_drive");
    assert_eq!(sources[0].check_interval_seconds, 7200);
    assert_eq!(
        sources[0].params.get("service_account_json"),
        Some(&key),
        "the full JSON key must be staged for the scheduler"
    );
    assert_eq!(
        sources[0].params.get("folder_id"),
        Some(&"folder-123".to_string())
    );

    let page = s.get("/sources").text().expect("read sources page");
    assert!(page.contains("Family Drive"), "sources page: {page}");
}

#[test]
fn form_service_account_save_rejects_malformed_json_with_html_error() {
    let s = TestServer::start();

    let response = s.post_form(
        "/api/sources/google-drive",
        &[
            ("name", "Broken Drive"),
            ("folder_id", "folder-123"),
            ("service_account_json", "{not json"),
        ],
    );

    assert_eq!(response.status(), 400);
    assert!(
        content_type(&response).starts_with("text/html"),
        "browser form errors must be HTML, got {}",
        content_type(&response)
    );
    let body = response.text().expect("read error page");
    assert!(body.contains("not valid JSON"), "error page: {body}");
    assert!(
        s.server.take_sources().is_empty(),
        "a malformed key must not be staged"
    );
}

#[test]
fn form_service_account_save_rejects_missing_fields_with_html_error() {
    let s = TestServer::start();

    let missing_email =
        r#"{"private_key":"-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----"}"#;
    let response = s.post_form(
        "/api/sources/google-drive",
        &[
            ("name", "No Email"),
            ("folder_id", "folder-123"),
            ("service_account_json", missing_email),
        ],
    );

    assert_eq!(response.status(), 400);
    assert!(content_type(&response).starts_with("text/html"));
    let body = response.text().expect("read error page");
    assert!(body.contains("client_email"), "error page: {body}");
    assert!(
        !body.contains("BEGIN PRIVATE KEY"),
        "error page leaks the key: {body}"
    );
    assert!(s.server.take_sources().is_empty());
}

#[test]
fn finish_succeeds_with_a_valid_service_account_source() {
    let s = TestServer::start_with(vec![staged_source(
        "google_drive",
        "Family Drive",
        &[
            ("folder_id", "folder-123"),
            ("service_account_json", &service_account_json()),
        ],
    )]);

    let response = s.post_empty_form("/api/finish");

    assert_eq!(response.status(), 200);
    assert!(content_type(&response).starts_with("text/html"));
    let body = response.text().expect("read finish page");
    assert!(body.contains("Setup Complete"), "finish page: {body}");
    assert!(s.server.finished());
    assert_eq!(s.server.take_sources().len(), 1);
}

#[test]
fn finish_rejects_a_malformed_service_account_key_with_html_error() {
    let s = TestServer::start_with(vec![staged_source(
        "google_drive",
        "Broken Drive",
        &[("folder_id", "folder-123"), ("service_account_json", "{}")],
    )]);

    let response = s.post_empty_form("/api/finish");

    assert_eq!(response.status(), 400);
    assert!(content_type(&response).starts_with("text/html"));
    let body = response.text().expect("read error page");
    assert!(body.contains("client_email"), "error page: {body}");
    assert!(!s.server.finished());
}

#[test]
fn oauth_callback_route_is_gone_and_requires_auth() {
    let s = TestServer::start();

    // The callback used to be the one unauthenticated route.
    let anonymous = s
        .client
        .get(s.url("/oauth/google/callback?code=abc"))
        .send()
        .expect("unauthenticated OAuth GET");
    assert_eq!(anonymous.status(), 401);

    let authenticated = s.get("/oauth/google/callback?code=abc");
    assert_eq!(authenticated.status(), 404);
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
