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

use crate::admin_server::AdminServer;
use crate::config::Config;
use crate::control::ControlClient;
use crate::import::MountEvent;
use crate::qr;
use std::collections::HashSet;
use std::io;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long config mode stays active without interaction.
const CONFIG_MODE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, PartialEq)]
enum ConfigState {
    /// Slideshow running normally.
    Normal,
    /// USB insert detected; waiting for import threads to complete.
    Importing { pending: HashSet<PathBuf> },
    /// Showing config-QR on screen, admin server running (Phase 7).
    ConfigMode { entered: Instant, password: String },
}

/// Owns the config-mode state machine.  Communicates with the USB-watcher via
/// the shared `MountEvent` channel.
pub struct ConfigEnvironment {
    state: ConfigState,
    control: ControlClient,
    config: Config,
    /// Admin server handle (running while in ConfigMode).
    server_handle: Option<JoinHandle<()>>,
    server_shutdown: Arc<AtomicBool>,
}

impl ConfigEnvironment {
    pub fn new(config: Config, control_socket_path: &std::path::Path) -> Self {
        ConfigEnvironment {
            state: ConfigState::Normal,
            control: ControlClient::new(control_socket_path),
            config,
            server_handle: None,
            server_shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Enter config mode: display the QR code and start the admin server.
    fn enter_config_mode(&mut self, fallback_ip: &str) -> io::Result<()> {
        let password = qr::generate_password();

        let server = AdminServer::new(password.clone())?;
        let port = server.port();
        let admin_host = format!("photo-frame.local:{port}");

        let qr_path = qr::render_config_screen(&self.config, &admin_host, &password, fallback_ip)
            .map_err(io::Error::other)?;

        self.control.clear()?;
        self.control.show(&qr_path)?;

        // Start server in background
        self.server_shutdown.store(false, Ordering::Relaxed);
        let srv_shutdown = self.server_shutdown.clone();
        self.server_handle = Some(std::thread::spawn(move || {
            if let Err(e) = server.run(srv_shutdown) {
                log::error!("Admin server error: {e}");
            }
        }));

        log::info!("Config mode active: http://{admin_host}  password={password}");
        self.state = ConfigState::ConfigMode {
            entered: Instant::now(),
            password,
        };

        Ok(())
    }

    /// Leave config mode: stop the admin server and resume the slideshow.
    fn leave_config_mode(&mut self) -> io::Result<()> {
        log::info!("Leaving config mode");
        self.server_shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.join();
        }
        self.control.resume()?;
        self.state = ConfigState::Normal;
        Ok(())
    }

    /// Process a mount event, updating the state machine.
    fn handle_event(&mut self, event: MountEvent, fallback_ip: &str) -> io::Result<()> {
        // Handle Inserted events separately to avoid borrow conflicts when
        // we need to read the old Importing { pending } set.
        if let MountEvent::Inserted(path) = event {
            let existing: Vec<_> = match &self.state {
                ConfigState::Importing { pending } => pending.iter().cloned().collect(),
                _ => vec![],
            };
            let mut pending: HashSet<_> = existing.into_iter().collect();
            pending.insert(path);
            self.state = ConfigState::Importing { pending };
            return Ok(());
        }

        match (&mut self.state, event) {
            // ---- Import finished (one mount) ----------------------------------
            (ConfigState::Importing { ref mut pending }, MountEvent::ImportComplete(path)) => {
                log::info!("Import complete: {}", path.display());
                pending.remove(&path);
                if pending.is_empty() {
                    log::info!("All imports done — entering config mode");
                    self.enter_config_mode(fallback_ip)?;
                }
            }

            // ---- USB removed --------------------------------------------------
            (ConfigState::Importing { ref mut pending }, MountEvent::Removed(path)) => {
                log::info!("USB removed during import: {}", path.display());
                pending.remove(&path);
                if pending.is_empty() {
                    self.state = ConfigState::Normal;
                }
            }
            (ConfigState::ConfigMode { .. }, MountEvent::Removed(path)) => {
                log::info!(
                    "USB removed — leaving config mode  ({path})",
                    path = path.display()
                );
                self.leave_config_mode()?;
            }
            (ConfigState::Normal, MountEvent::Removed(path)) => {
                log::info!("USB removed: {}", path.display());
            }

            // ---- Late import-complete after mode change (ignore) --------------
            (ConfigState::Normal, MountEvent::ImportComplete(_))
            | (ConfigState::ConfigMode { .. }, MountEvent::ImportComplete(_)) => {}

            // USB inserted while already in config mode: restart.
            _ => {}
        }

        Ok(())
    }

    /// Check whether the config-mode timeout has elapsed (no server activity).
    fn check_timeout(&mut self) -> io::Result<()> {
        if let ConfigState::ConfigMode { entered, .. } = self.state {
            if entered.elapsed() >= CONFIG_MODE_TIMEOUT {
                log::info!("Config mode timed out");
                self.leave_config_mode()?;
            }
        }
        Ok(())
    }
}

/// Detect a reasonable IPv4 fallback address.  Returns the first non-loopback
/// IPv4 address found, or `127.0.0.1` if none is available.
pub fn detect_fallback_ip() -> String {
    // Try `hostname -I` first (DietPi / Debian-style quick list).
    if let Ok(out) = std::process::Command::new("hostname").arg("-I").output() {
        let s = String::from_utf8_lossy(&out.stdout);
        for word in s.split_whitespace() {
            if let Ok(ip) = word.parse::<IpAddr>() {
                if ip.is_ipv4() && !ip.is_loopback() {
                    return ip.to_string();
                }
            }
        }
    }

    // Fallback: iterate local interfaces via getifaddrs (via libc or
    // platform-dependent).  For simplicity on Linux, return the loopback and
    // let the user SSH in.
    "127.0.0.1".to_string()
}

/// Runs the config-mode state machine in a dedicated thread.
pub fn run_config_mode_loop(
    config: Config,
    control_socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    event_rx: mpsc::Receiver<MountEvent>,
) -> io::Result<()> {
    let fallback_ip = detect_fallback_ip();

    // First-boot detection: no photos and no remote sources → enter config
    // mode immediately.
    let first_boot = {
        let (_, meta) = crate::index::init_index(&config.photos_dir)?;
        meta.valid_count == 0
    };
    // TODO: also check for absence of [[remote_sources]] in config (Phase 4).

    let mut env = ConfigEnvironment::new(config, &control_socket_path);

    if first_boot {
        log::info!("First boot detected — entering config mode");
        env.enter_config_mode(&fallback_ip)?;
    }

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Config mode shutting down");
            if env.state != ConfigState::Normal {
                let _ = env.leave_config_mode();
            }
            break;
        }

        env.check_timeout()?;

        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                if let Err(e) = env.handle_event(event, &fallback_ip) {
                    log::error!("Config mode event error: {e}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No event — loop back to check timeout + shutdown.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log::warn!("Mount event channel disconnected");
                break;
            }
        }
    }

    Ok(())
}
