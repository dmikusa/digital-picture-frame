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

//! Shared HTTP helpers for the remote photo sources.
//!
//! Everything here is blocking, matching the rest of the manager. The goal is
//! simple and testable: one client with a sane timeout, bounded exponential
//! backoff for transient failures, and downloads streamed straight to disk.

use reqwest::blocking::{Client, Response};
use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use std::io;
use std::path::Path;
use std::time::Duration;

/// Total request timeout, including reading the response body. Large photos
/// can take a while on a Pi Zero, but not forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

const MAX_ATTEMPTS: u32 = 4;
const BASE_BACKOFF: Duration = Duration::from_millis(500);
/// Upper bound for both exponential backoff and `Retry-After`, so a hostile
/// or misconfigured server cannot stall the sync thread for minutes.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

pub fn build_client() -> io::Result<Client> {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(io::Error::other)
}

/// Statuses worth retrying: rate limiting and transient server-side errors.
pub fn is_transient(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
}

/// `Retry-After` as a number of seconds. HTTP-date values are ignored, which
/// falls back to exponential backoff.
pub fn retry_after(resp: &Response) -> Option<Duration> {
    resp.headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Send a request, retrying transient responses with bounded exponential
/// backoff. `attempt` must build a fresh request each call (clients and
/// request builders are not reusable after `send`).
pub fn send_with_retry<F>(attempt: F) -> io::Result<Response>
where
    F: FnMut() -> io::Result<Response>,
{
    send_with_retry_config(attempt, MAX_ATTEMPTS, BASE_BACKOFF)
}

fn send_with_retry_config<F>(
    mut attempt: F,
    max_attempts: u32,
    base_backoff: Duration,
) -> io::Result<Response>
where
    F: FnMut() -> io::Result<Response>,
{
    let mut backoff = base_backoff;
    let mut attempts = 0;

    loop {
        attempts += 1;
        let resp = attempt()?;
        let status = resp.status();

        if !is_transient(status) || attempts >= max_attempts {
            return Ok(resp);
        }

        let wait = retry_after(&resp).unwrap_or(backoff).min(MAX_BACKOFF);
        log::warn!(
            "Transient HTTP {}; retrying in {:?} (attempt {}/{})",
            status,
            wait,
            attempts,
            max_attempts
        );
        std::thread::sleep(wait);
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

/// Stream a response body into `dest` instead of buffering it in memory, so a
/// large photo does not sit in RAM. A partial file is removed on error.
pub fn download_to_file(mut resp: Response, dest: &Path) -> io::Result<()> {
    let mut file = std::fs::File::create(dest)?;
    match resp.copy_to(&mut file) {
        Ok(_) => Ok(()),
        Err(e) => {
            drop(file);
            let _ = std::fs::remove_file(dest);
            Err(io::Error::other(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::test_http::{TestReply, TestServer};

    #[test]
    fn test_is_transient() {
        for code in [429, 500, 502, 503, 504] {
            assert!(is_transient(StatusCode::from_u16(code).unwrap()), "{code}");
        }
        for code in [200, 400, 401, 403, 404] {
            assert!(!is_transient(StatusCode::from_u16(code).unwrap()), "{code}");
        }
    }

    #[test]
    fn test_client_timeout_is_set() {
        // Building the client is the only assertion we can make without
        // waiting two minutes; a broken builder would error here.
        assert!(build_client().is_ok());
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(120));
    }

    #[test]
    fn test_send_with_retry_recovers_from_transient_errors() {
        let server = TestServer::start(|_req, index| match index {
            0 => TestReply::status(503),
            1 => TestReply::status(429).with_header("Retry-After", "0"),
            _ => TestReply::new(200, "ok"),
        });

        let client = build_client().unwrap();
        let resp = send_with_retry_config(
            || {
                client
                    .get(&server.base_url)
                    .send()
                    .map_err(io::Error::other)
            },
            4,
            Duration::from_millis(1),
        )
        .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().unwrap(), "ok");
        assert_eq!(server.request_count(), 3);
    }

    #[test]
    fn test_send_with_retry_gives_up_after_max_attempts() {
        let server = TestServer::start(|_req, _index| TestReply::status(500));

        let client = build_client().unwrap();
        let resp = send_with_retry_config(
            || {
                client
                    .get(&server.base_url)
                    .send()
                    .map_err(io::Error::other)
            },
            3,
            Duration::from_millis(1),
        )
        .unwrap();

        // The last response is returned so the caller can report the status.
        assert_eq!(resp.status(), 500);
        assert_eq!(server.request_count(), 3);
    }

    #[test]
    fn test_send_with_retry_does_not_retry_client_errors() {
        let server = TestServer::start(|_req, _index| TestReply::status(401));

        let client = build_client().unwrap();
        let resp = send_with_retry_config(
            || {
                client
                    .get(&server.base_url)
                    .send()
                    .map_err(io::Error::other)
            },
            4,
            Duration::from_millis(1),
        )
        .unwrap();

        assert_eq!(resp.status(), 401);
        assert_eq!(server.request_count(), 1);
    }

    #[test]
    fn test_download_to_file_streams_bytes() {
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        let server = TestServer::start(move |_req, _index| TestReply::new(200, payload.clone()));

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");

        let client = build_client().unwrap();
        let resp = client.get(&server.base_url).send().unwrap();
        download_to_file(resp, &dest).unwrap();

        let written = std::fs::read(&dest).unwrap();
        assert_eq!(written, expected);
    }
}
