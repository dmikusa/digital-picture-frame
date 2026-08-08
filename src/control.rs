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
use std::time::{Duration, Instant};

/// Sends control commands to the display app's control socket.
/// Used to pause/resume the slideshow and show overlay images.
pub struct ControlClient {
    socket_path: std::path::PathBuf,
    stream: Option<UnixStream>,
    timeout: Duration,
}

impl ControlClient {
    pub fn new(socket_path: &Path) -> Self {
        ControlClient {
            socket_path: socket_path.to_path_buf(),
            stream: None,
            timeout: Duration::from_secs(5),
        }
    }

    fn connect(&mut self) -> io::Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let deadline = Instant::now() + self.timeout;
        loop {
            match UnixStream::connect(&self.socket_path) {
                Ok(stream) => {
                    stream.set_write_timeout(Some(self.timeout))?;
                    self.stream = Some(stream);
                    return Ok(());
                }
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        || e.kind() == io::ErrorKind::ConnectionRefused =>
                {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn send(&mut self, cmd: &str) -> io::Result<()> {
        self.connect()?;
        let msg = format!("{cmd}\n");
        let stream = self.stream.as_mut().unwrap();
        stream.write_all(msg.as_bytes())
    }

    /// Clear the render queue and pause the slideshow.
    pub fn clear(&mut self) -> io::Result<()> {
        self.send("CLR")
    }

    /// Display an image at the given path and hold it indefinitely.
    pub fn show(&mut self, path: &Path) -> io::Result<()> {
        let cmd = format!("SHOW {}", path.display());
        self.send(&cmd)
    }

    /// Resume normal slideshow operation.
    pub fn resume(&mut self) -> io::Result<()> {
        self.send("RESUME")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn test_clear_command() {
        let tmpdir = tempfile::tempdir().unwrap();
        let socket_path = tmpdir.path().join("control.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut client = ControlClient::new(&socket_path);
        client.clear().unwrap();

        let received = handle.join().unwrap();
        assert_eq!(received, "CLR\n");
    }

    #[test]
    fn test_show_command() {
        let tmpdir = tempfile::tempdir().unwrap();
        let socket_path = tmpdir.path().join("control.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut client = ControlClient::new(&socket_path);
        client.show(std::path::Path::new("/tmp/test.jpg")).unwrap();

        let received = handle.join().unwrap();
        assert_eq!(received, "SHOW /tmp/test.jpg\n");
    }

    #[test]
    fn test_resume_command() {
        let tmpdir = tempfile::tempdir().unwrap();
        let socket_path = tmpdir.path().join("control.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut client = ControlClient::new(&socket_path);
        client.resume().unwrap();

        let received = handle.join().unwrap();
        assert_eq!(received, "RESUME\n");
    }
}
