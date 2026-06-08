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

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// A client that connects to the display app's Unix domain socket.
/// Relies on the kernel socket buffer for backpressure.
pub struct DisplayClient {
    socket_path: std::path::PathBuf,
    stream: Option<UnixStream>,
    timeout: Duration,
    backoff: Duration,
}

impl DisplayClient {
    pub fn new(socket_path: &Path) -> Self {
        DisplayClient {
            socket_path: socket_path.to_path_buf(),
            stream: None,
            timeout: Duration::from_secs(30),
            backoff: Duration::from_secs(5),
        }
    }

    /// Ensure we have a live connection. Reconnects only if the stream is missing.
    fn ensure_connected(&mut self) -> io::Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        self.reconnect()
    }

    fn reconnect(&mut self) -> io::Result<()> {
        log::info!(
            "Connecting to display socket at {}",
            self.socket_path.display()
        );
        let stream = UnixStream::connect(&self.socket_path)?;
        // 30-second write timeout lets us detect a truly dead display app
        // without breaking normal backpressure (the display app may pause
        // reading for several seconds while it renders).
        stream.set_write_timeout(Some(self.timeout))?;
        self.stream = Some(stream);
        log::info!("Connected to display socket");
        Ok(())
    }

    /// Send an IMG command to the display app.
    ///
    /// When the display app is consuming, this returns immediately.
    /// When the display app is backpressuring us (its buffer is full and it
    /// has paused reading), `write_all` blocks until the kernel buffer has
    /// space or the 30-second timeout expires.
    pub fn send_img(&mut self, path: &str) -> io::Result<()> {
        self.ensure_connected()?;

        let msg = format!("IMG {}\n", path);

        loop {
            let stream = self.stream.as_mut().unwrap();
            match stream.write_all(msg.as_bytes()) {
                Ok(()) => return Ok(()),
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    // Backpressure: display app isn't reading fast enough.
                    // Pause briefly and retry on the same connection.
                    log::debug!("Write timed out (backpressure), retrying in 100ms");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e)
                    if e.kind() == io::ErrorKind::BrokenPipe
                        || e.kind() == io::ErrorKind::ConnectionReset =>
                {
                    // Connection lost. Reconnect once and retry.
                    log::warn!("Display connection lost: {}", e);
                    self.stream = None;
                    std::thread::sleep(self.backoff);
                    self.reconnect()?;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn close(&mut self) {
        self.stream = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn test_send_img() {
        let tmpdir = tempfile::tempdir().unwrap();
        let socket_path = tmpdir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut client = DisplayClient::new(&socket_path);
        client.send_img("/photos/test.jpg").unwrap();

        let received = handle.join().unwrap();
        assert_eq!(received, "IMG /photos/test.jpg\n");
    }
}
