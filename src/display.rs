use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// A client that connects to the display app's Unix domain socket.
/// Handles reconnection with backoff on failures.
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
            timeout: Duration::from_secs(5),
            backoff: Duration::from_secs(5),
        }
    }

    /// Ensure we have a connected socket by sending a PING and expecting PONG.
    fn ensure_connected(&mut self) -> io::Result<()> {
        if let Some(mut stream) = self.stream.take() {
            if ping(&mut stream).is_ok() {
                self.stream = Some(stream);
                return Ok(());
            }
            // Ping failed, drop the stream and reconnect
        }

        self.reconnect()?;
        // Verify the new connection with a ping
        if let Some(ref mut stream) = self.stream {
            ping(stream)?;
        }
        Ok(())
    }

    fn reconnect(&mut self) -> io::Result<()> {
        log::info!("Connecting to display socket at {}", self.socket_path.display());
        let stream = UnixStream::connect(&self.socket_path)?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream.set_read_timeout(Some(self.timeout))?;
        self.stream = Some(stream);
        log::info!("Connected to display socket");
        Ok(())
    }

    /// Send an IMG command to the display app.
    /// Blocks if the kernel socket buffer is full (backpressure).
    /// Returns an error only if the connection is definitively dead.
    pub fn send_img(&mut self, path: &str) -> io::Result<()> {
        self.ensure_connected()?;

        let msg = format!("IMG {}\n", path);
        let stream = self.stream.as_mut().unwrap();

        match stream.write_all(msg.as_bytes()) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Write failed. Mark connection as dead and retry once after reconnect.
                log::warn!("Send failed: {}, will reconnect", e);
                self.stream = None;
                std::thread::sleep(self.backoff);
                self.reconnect()?;
                let stream = self.stream.as_mut().unwrap();
                stream.write_all(msg.as_bytes())
            }
        }
    }

    pub fn close(&mut self) {
        self.stream = None;
    }
}

/// Send a PING and expect a PONG response within the configured timeout.
fn ping(stream: &mut UnixStream) -> io::Result<()> {
    let mut peer = stream.try_clone()?;
    let mut reader = BufReader::new(&mut peer);

    stream.write_all(b"PING\n")?;
    stream.flush()?;

    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "PING: EOF"));
    }
    if line.trim() == "PONG" {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("PING: expected PONG, got: {}", line.trim()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn test_ping_pong() {
        let tmpdir = tempfile::tempdir().unwrap();
        let socket_path = tmpdir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap();
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            assert_eq!(received, "PING\n");
            stream.write_all(b"PONG\n").unwrap();
        });

        let mut client = DisplayClient::new(&socket_path);
        // Ping should succeed
        client.ensure_connected().unwrap();

        handle.join().unwrap();
    }

    #[test]
    fn test_send_img() {
        let tmpdir = tempfile::tempdir().unwrap();
        let socket_path = tmpdir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            // First read is PING
            let n = stream.read(&mut buf).unwrap();
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(received.starts_with("PING"));
            stream.write_all(b"PONG\n").unwrap();

            // Second read is IMG
            let n = stream.read(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut client = DisplayClient::new(&socket_path);
        client.send_img("/photos/test.jpg").unwrap();

        let received = handle.join().unwrap();
        assert_eq!(received, "IMG /photos/test.jpg\n");
    }
}
