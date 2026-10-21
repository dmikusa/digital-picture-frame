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
        let stream = connect_stream(&self.socket_path, self.timeout)?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Send an IMG command to the display app.
    ///
    /// When the display app is consuming, this returns immediately.
    /// When the display app is backpressuring us (its buffer is full and it
    /// has paused reading), the write loop blocks until the kernel buffer has
    /// space or the 30-second timeout expires. A partial write is resumed on
    /// the same connection so no bytes are duplicated. A broken connection is
    /// reconnected and the message is resent from the start.
    pub fn send_img(&mut self, path: &str) -> io::Result<()> {
        self.ensure_connected()?;

        let msg = format!("IMG {}\n", path);
        let socket_path = self.socket_path.clone();
        let timeout = self.timeout;
        let backoff = self.backoff;

        let mut stream = self.stream.take().expect("connected above");
        let mut reconnect_failed = false;
        let result = send_with_reconnect(
            msg.as_bytes(),
            &mut stream,
            |s: &mut UnixStream| match connect_stream(&socket_path, timeout) {
                Ok(new_stream) => {
                    *s = new_stream;
                    Ok(())
                }
                Err(e) => {
                    reconnect_failed = true;
                    Err(e)
                }
            },
            backoff,
        );

        self.stream = if reconnect_failed { None } else { Some(stream) };
        result
    }

    pub fn close(&mut self) {
        self.stream = None;
    }
}

/// Connect to the display socket and apply the write timeout.
fn connect_stream(socket_path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    log::info!("Connecting to display socket at {}", socket_path.display());
    let stream = UnixStream::connect(socket_path)?;
    // 30-second write timeout lets us detect a truly dead display app
    // without breaking normal backpressure (the display app may pause
    // reading for several seconds while it renders).
    stream.set_write_timeout(Some(timeout))?;
    log::info!("Connected to display socket");
    Ok(stream)
}

/// Returns true when the error means the connection is no longer usable.
fn is_disconnect(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    )
}

/// Write all of `buf` to `writer`, tracking how many bytes have been written.
///
/// `Interrupted` retries immediately. `WouldBlock`/`TimedOut` means the socket
/// buffer is full (backpressure): sleep briefly and resume from the current
/// offset on the same connection, so a partial write is never duplicated.
/// Any other error (including a broken pipe) is returned to the caller.
fn write_all_resumable<W: Write>(writer: &mut W, buf: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < buf.len() {
        match writer.write(&buf[written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole buffer",
                ));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                // Backpressure: display app isn't reading fast enough.
                // Pause briefly and resume on the same connection.
                log::debug!("Write timed out (backpressure), retrying in 100ms");
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Send `msg`, reconnecting and resending from offset 0 on a broken pipe or
/// reset. `reconnect` swaps in a fresh connection for the writer.
fn send_with_reconnect<W, F>(
    msg: &[u8],
    writer: &mut W,
    mut reconnect: F,
    backoff: Duration,
) -> io::Result<()>
where
    W: Write,
    F: FnMut(&mut W) -> io::Result<()>,
{
    loop {
        match write_all_resumable(writer, msg) {
            Ok(()) => return Ok(()),
            Err(e) if is_disconnect(&e) => {
                log::warn!("Display connection lost: {}", e);
                std::thread::sleep(backoff);
                reconnect(writer)?;
                // Resend the whole message from the start on the new connection.
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::rc::Rc;
    use std::thread;

    /// One scripted `write` call for [`MockWriter`].
    enum Action {
        /// Accept up to this many bytes (0 means accept all remaining).
        Accept(usize),
        /// Return this error kind.
        Fail(io::ErrorKind),
    }

    /// A mock writer that records the bytes it accepts and can be scripted to
    /// accept a limited number of bytes per call or fail in specific ways.
    struct MockWriter {
        out: Vec<u8>,
        actions: Vec<Action>,
    }

    impl MockWriter {
        fn scripted(actions: Vec<Action>) -> Self {
            MockWriter {
                out: Vec::new(),
                actions,
            }
        }
    }

    impl Write for MockWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match if self.actions.is_empty() {
                Action::Accept(0)
            } else {
                self.actions.remove(0)
            } {
                Action::Fail(kind) => Err(io::Error::new(kind, "scripted error")),
                Action::Accept(0) => {
                    self.out.extend_from_slice(buf);
                    Ok(buf.len())
                }
                Action::Accept(n) => {
                    let n = n.min(buf.len());
                    self.out.extend_from_slice(&buf[..n]);
                    Ok(n)
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

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

    #[test]
    fn test_write_all_resumable_one_byte_at_a_time() {
        let msg = b"IMG /photos/test.jpg\n";
        let actions = (0..msg.len()).map(|_| Action::Accept(1)).collect();
        let mut writer = MockWriter::scripted(actions);
        write_all_resumable(&mut writer, msg).unwrap();
        // Delivered exactly once, not duplicated.
        assert_eq!(writer.out, msg);
    }

    #[test]
    fn test_write_all_resumable_resumes_after_would_block() {
        let msg = b"IMG /photos/test.jpg\n";
        // Accept 4 bytes, signal backpressure, then accept the rest.
        let mut writer = MockWriter::scripted(vec![
            Action::Accept(4),
            Action::Fail(io::ErrorKind::WouldBlock),
            Action::Accept(0),
        ]);
        write_all_resumable(&mut writer, msg).unwrap();
        // No duplication and the message is complete.
        assert_eq!(writer.out, msg);
    }

    #[test]
    fn test_write_all_resumable_retries_interrupted() {
        let msg = b"IMG /photos/test.jpg\n";
        let mut writer = MockWriter::scripted(vec![
            Action::Fail(io::ErrorKind::Interrupted),
            Action::Accept(0),
        ]);
        write_all_resumable(&mut writer, msg).unwrap();
        assert_eq!(writer.out, msg);
    }

    #[test]
    fn test_send_with_reconnect_resends_from_start() {
        let msg = b"IMG /photos/test.jpg\n";
        let log: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));

        // First connection accepts 5 bytes then breaks; the reconnect closure
        // archives the partial write and installs a fresh, working writer.
        let mut writer = MockWriter::scripted(vec![
            Action::Accept(5),
            Action::Fail(io::ErrorKind::BrokenPipe),
        ]);
        let log_clone = Rc::clone(&log);
        send_with_reconnect(
            msg,
            &mut writer,
            |w: &mut MockWriter| {
                log_clone.borrow_mut().push(std::mem::take(&mut w.out));
                w.actions = vec![Action::Accept(0)];
                Ok(())
            },
            Duration::ZERO,
        )
        .unwrap();

        let mut attempts = log.borrow().clone();
        attempts.push(writer.out.clone());

        // First connection got a partial write...
        assert_eq!(attempts[0], &msg[..5]);
        // ...and the resend delivered the full message from offset 0.
        assert_eq!(attempts[1], msg);
    }

    #[test]
    fn test_send_with_reconnect_propagates_other_errors() {
        let msg = b"IMG /photos/test.jpg\n";
        let mut writer = MockWriter::scripted(vec![Action::Fail(io::ErrorKind::PermissionDenied)]);
        let result = send_with_reconnect(
            msg,
            &mut writer,
            |_w: &mut MockWriter| Ok(()),
            Duration::ZERO,
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}
