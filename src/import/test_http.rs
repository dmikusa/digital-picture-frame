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

//! Test-only local HTTP server for the remote source unit tests.
//!
//! Wraps `tiny_http_fork` on an ephemeral port so Dropbox and Google Drive
//! pagination, retries, and downloads can be exercised without touching the
//! real APIs.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tiny_http_fork::{Header, Response, Server, StatusCode};

/// A request captured by [`TestServer`].
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub body: String,
}

/// The response a test handler wants to send.
pub struct TestReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl TestReply {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        TestReply {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        TestReply::new(200, body).with_header("Content-Type", "application/json")
    }

    pub fn status(status: u16) -> Self {
        TestReply::new(status, Vec::new())
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// A local HTTP server that lives for the lifetime of the value.
pub struct TestServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Start a server. `handler` receives each recorded request plus its
    /// zero-based index, so responses can change between calls.
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest, usize) -> TestReply + Send + 'static,
    {
        let server = Server::http("127.0.0.1:0").expect("bind test HTTP server");
        let addr = server
            .server_addr()
            .to_ip()
            .expect("test server should have an IP address");
        let base_url = format!("http://{addr}");

        let shutdown = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_shutdown = shutdown.clone();
        let thread_requests = requests.clone();

        let handle = std::thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Relaxed) {
                match server.recv_timeout(Duration::from_millis(50)) {
                    Ok(Some(mut request)) => {
                        let mut body = String::new();
                        let _ = request.as_reader().read_to_string(&mut body);
                        let recorded = RecordedRequest {
                            method: request.method().as_str().to_string(),
                            url: request.url().to_string(),
                            body,
                        };
                        let index = {
                            let mut list = thread_requests.lock().unwrap();
                            list.push(recorded.clone());
                            list.len() - 1
                        };

                        let reply = handler(&recorded, index);
                        let headers: Vec<Header> = reply
                            .headers
                            .iter()
                            .filter_map(|(name, value)| {
                                Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
                            })
                            .collect();
                        let len = reply.body.len();
                        let response = Response::new(
                            StatusCode(reply.status),
                            headers,
                            std::io::Cursor::new(reply.body),
                            Some(len),
                            None,
                        );
                        let _ = request.respond(response);
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });

        TestServer {
            base_url,
            requests,
            shutdown,
            handle: Some(handle),
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
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
