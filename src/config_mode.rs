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
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long to wait before retrying a config-mode entry that could not reach
/// the display app (for example, because it has not created its socket yet).
const CONFIG_MODE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// How often the first-sync screen checks the index for newly synced photos.
const FIRST_SYNC_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What to show when the manager starts, before any USB events arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupState {
    /// Photos exist, so run the slideshow.
    Slideshow,
    /// No photos, but remote sources are configured: wait for the first sync.
    FirstSync,
    /// No photos and no remote sources: this is a fresh frame, so show setup.
    ConfigMode,
}

#[derive(Debug, PartialEq)]
enum ConfigState {
    /// Slideshow running normally.
    Normal,
    /// No photos yet; waiting for the first remote sync to produce some.
    FirstSync,
    /// USB insert detected; waiting for import threads to complete.
    Importing { pending: HashSet<PathBuf> },
    /// Config-mode entry failed (display app not ready); retry shortly.
    PendingConfigMode { retry_at: Instant },
    /// Showing config-QR on screen, admin server running (Phase 7).
    ConfigMode,
}

/// Pick the startup screen from the current photo count and config.
fn initial_state(valid_count: usize, has_remote_sources: bool) -> StartupState {
    if valid_count > 0 {
        StartupState::Slideshow
    } else if has_remote_sources {
        StartupState::FirstSync
    } else {
        StartupState::ConfigMode
    }
}

/// Whether the index currently reports at least one photo.
fn photos_available(photos_dir: &Path) -> bool {
    crate::index::init_index(photos_dir)
        .map(|(_, meta)| meta.valid_count > 0)
        .unwrap_or(false)
}

/// Serialize `config` to TOML and atomically replace `path`.
///
/// The temporary file is created in the same directory so the final rename is
/// atomic. It is created with mode 0600 (config may contain credentials) and
/// removed if any step fails, leaving the original file untouched.
pub(crate) fn save_config_atomic(path: &Path, config: &Config) -> io::Result<()> {
    let toml_str = toml::to_string_pretty(config).map_err(io::Error::other)?;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("config path has no file name: {}", path.display()),
        )
    })?;
    let tmp_path = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));

    let result =
        write_config_tmp(&tmp_path, &toml_str).and_then(|()| std::fs::rename(&tmp_path, path));

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }

    result
}

/// Write `contents` to `path`, creating it with mode 0600.
fn write_config_tmp(path: &Path, contents: &str) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;

    // Explicitly set the mode as well: the open mode is filtered through the
    // process umask, and we always want config.toml to end up as 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }

    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

/// Owns the config-mode state machine.  Communicates with the USB-watcher via
/// the shared `MountEvent` channel.
pub struct ConfigEnvironment {
    state: ConfigState,
    control: ControlClient,
    config: Config,
    config_path: PathBuf,
    server: Option<Arc<AdminServer>>,
    server_handle: Option<JoinHandle<()>>,
    server_shutdown: Arc<AtomicBool>,
}

impl ConfigEnvironment {
    pub fn new(
        config: Config,
        control_socket_path: &std::path::Path,
        config_path: PathBuf,
    ) -> Self {
        ConfigEnvironment {
            state: ConfigState::Normal,
            control: ControlClient::new(control_socket_path),
            config,
            config_path,
            server: None,
            server_handle: None,
            server_shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Enter config mode: display the QR code and start the admin server.
    ///
    /// If the display app is not reachable yet, this returns an error without
    /// leaving a listener behind. The caller retries through
    /// [`ConfigState::PendingConfigMode`].
    fn enter_config_mode(&mut self, fallback_ip: &str) -> io::Result<()> {
        let password = qr::generate_password();
        let staged_sources = self.config.remote_sources.clone();
        let status_path = self
            .config
            .photos_dir
            .join(crate::admin_server::SYNC_STATUS_FILE);

        let server = Arc::new(
            AdminServer::new(password.clone(), staged_sources)?.with_status_path(status_path),
        );
        let port = server.port();

        let qr_path =
            qr::render_config_screen(&self.config, fallback_ip, qr::MDNS_HOST, port, &password)
                .map_err(io::Error::other)?;

        // Start accepting requests before showing the QR so the URL is live as
        // soon as someone scans it.
        self.server_shutdown.store(false, Ordering::Relaxed);
        let srv = server.clone();
        let srv_shutdown = self.server_shutdown.clone();
        self.server_handle = Some(std::thread::spawn(move || {
            if let Err(e) = srv.run(srv_shutdown) {
                log::error!("Admin server error: {e}");
            }
        }));
        self.server = Some(server);

        if let Err(e) = self
            .control
            .clear()
            .and_then(|()| self.control.show(&qr_path))
        {
            self.stop_server();
            return Err(e);
        }

        log::info!(
            "Config mode active: http://{fallback_ip}:{port} (or http://{}:{port})",
            qr::MDNS_HOST
        );
        self.state = ConfigState::ConfigMode;

        Ok(())
    }

    /// Try to enter config mode, falling back to a retry when the display app
    /// is not ready. Never leaves the state machine without a server retry.
    fn begin_config_mode(&mut self, fallback_ip: &str) {
        match self.enter_config_mode(fallback_ip) {
            Ok(()) => {}
            Err(e) => {
                log::warn!(
                    "Config mode not ready ({e}); retrying in {:?}",
                    CONFIG_MODE_RETRY_DELAY
                );
                self.stop_server();
                self.state = ConfigState::PendingConfigMode {
                    retry_at: Instant::now() + CONFIG_MODE_RETRY_DELAY,
                };
            }
        }
    }

    /// Stop the admin server, if one is running.
    fn stop_server(&mut self) {
        self.server_shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.join();
        }
        self.server = None;
    }

    /// Leave config mode: stop the admin server and resume the slideshow.
    /// If `save` is true, write the staged remote sources to config.toml.
    fn leave_config_mode(&mut self, save: bool) {
        log::info!("Leaving config mode (save={save})");

        let staged = self.server.as_ref().map(|server| server.take_sources());
        self.stop_server();

        if save {
            if let Some(sources) = staged {
                if let Err(e) = self.save_staged_sources(sources) {
                    log::error!("Failed to save config: {e}");
                }
            }
        }

        self.resume_slideshow();
        self.state = ConfigState::Normal;
    }

    /// Persist `sources` and remember them so a later config session sees them.
    fn save_staged_sources(
        &mut self,
        sources: Vec<crate::config::RemoteSourceConfig>,
    ) -> io::Result<()> {
        let mut config = self.config.clone();
        config.remote_sources = sources;
        save_config_atomic(&self.config_path, &config)?;
        log::info!("Config saved to {}", self.config_path.display());
        self.config = config;
        Ok(())
    }

    /// Best-effort `RESUME`; a missing display app must not stop the loop.
    fn resume_slideshow(&mut self) {
        if let Err(e) = self.control.resume() {
            log::warn!("Could not resume slideshow: {e}");
        }
    }

    /// Draw a simple status screen (importing, waiting for first sync, ...).
    fn show_status_screen(&mut self, title: &str, lines: &[&str]) {
        match qr::render_status_screen(&self.config, title, lines) {
            Ok(path) => {
                if let Err(e) = self.control.clear().and_then(|()| self.control.show(&path)) {
                    log::warn!("Could not display status screen: {e}");
                }
            }
            Err(e) => log::warn!("Could not render status screen: {e}"),
        }
    }

    fn show_first_sync_screen(&mut self) {
        self.show_status_screen(
            "First remote sync in progress",
            &[
                "Your photos will appear here when the sync finishes.",
                "This may take some time.",
            ],
        );
    }

    /// Process a mount event, updating the state machine.
    fn handle_event(&mut self, event: MountEvent, fallback_ip: &str) {
        match event {
            MountEvent::Inserted(path) => {
                log::info!("USB inserted: {}", path.display());

                // A new USB drive invalidates any running config session. Stop
                // the old listener before starting another so we do not leak a
                // port, listener, and password.
                self.stop_server();

                let mut pending = match &self.state {
                    ConfigState::Importing { pending } => pending.clone(),
                    _ => HashSet::new(),
                };
                pending.insert(path);
                self.state = ConfigState::Importing { pending };

                self.show_status_screen(
                    "Importing photos...",
                    &[
                        "Please wait, this can take a few minutes.",
                        "Do not remove the USB drive.",
                    ],
                );
            }

            // ---- Import finished (one mount) ----------------------------------
            MountEvent::ImportComplete(path) => {
                let done = match &mut self.state {
                    ConfigState::Importing { pending } => {
                        log::info!("Import complete: {}", path.display());
                        pending.remove(&path);
                        pending.is_empty()
                    }
                    _ => false,
                };

                if done {
                    log::info!("All imports done — entering config mode");
                    self.begin_config_mode(fallback_ip);
                }
            }

            // ---- USB removed --------------------------------------------------
            MountEvent::Removed(path) => {
                if matches!(self.state, ConfigState::Importing { .. }) {
                    log::info!("USB removed during import: {}", path.display());
                    let empty = match &mut self.state {
                        ConfigState::Importing { pending } => {
                            pending.remove(&path);
                            pending.is_empty()
                        }
                        _ => false,
                    };
                    if empty {
                        self.state = ConfigState::Normal;
                        self.resume_slideshow();
                    }
                } else if matches!(
                    self.state,
                    ConfigState::ConfigMode | ConfigState::PendingConfigMode { .. }
                ) {
                    log::info!("USB removed — leaving config mode ({})", path.display());
                    self.leave_config_mode(false);
                } else {
                    log::info!("USB removed: {}", path.display());
                }
            }
        }
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
    config_path: PathBuf,
    control_socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    event_rx: mpsc::Receiver<MountEvent>,
) -> io::Result<()> {
    let fallback_ip = detect_fallback_ip();

    // Decide what to show on startup from the photo index and the config.
    let valid_count = crate::index::init_index(&config.photos_dir)?.1.valid_count;
    let startup = initial_state(valid_count, !config.remote_sources.is_empty());

    let mut env = ConfigEnvironment::new(config, &control_socket_path, config_path);

    match startup {
        StartupState::Slideshow => {
            log::info!("Photos available — starting in slideshow mode");
        }
        StartupState::FirstSync => {
            log::info!("No photos yet and remote sources configured — waiting for first sync");
            env.state = ConfigState::FirstSync;
            env.show_first_sync_screen();
        }
        StartupState::ConfigMode => {
            log::info!("First boot detected — entering config mode");
            env.begin_config_mode(&fallback_ip);
        }
    }

    let mut next_first_sync_poll = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Config mode shutting down");
            if matches!(env.state, ConfigState::ConfigMode) {
                let save = env.server.as_ref().map(|s| s.finished()).unwrap_or(false);
                env.leave_config_mode(save);
            } else {
                env.stop_server();
            }
            break;
        }

        // Check if the user finished setup or the session timed out.
        if matches!(env.state, ConfigState::ConfigMode) {
            if let Some(ref server) = env.server {
                if server.finished() {
                    log::info!("User finished setup — saving config");
                    env.leave_config_mode(true);
                    continue;
                }
                if server.timed_out() {
                    log::info!("Config mode timed out");
                    env.leave_config_mode(false);
                    continue;
                }
            }
        }

        // Retry config-mode entry after a transient display-app failure.
        if let ConfigState::PendingConfigMode { retry_at } = env.state {
            if Instant::now() >= retry_at {
                log::info!("Retrying config mode entry");
                env.begin_config_mode(&fallback_ip);
                continue;
            }
        }

        // While waiting for the first remote sync, watch the index so the
        // slideshow resumes as soon as photos show up.
        if matches!(env.state, ConfigState::FirstSync)
            && next_first_sync_poll.elapsed() >= FIRST_SYNC_POLL_INTERVAL
        {
            next_first_sync_poll = Instant::now();
            if photos_available(&env.config.photos_dir) {
                log::info!("First-sync photos available — resuming slideshow");
                env.resume_slideshow();
                env.state = ConfigState::Normal;
            }
        }

        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => env.handle_event(event, &fallback_ip),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AspectRatioMode, RemoteSourceConfig};
    use std::collections::HashMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn sample_config(photos_dir: PathBuf) -> Config {
        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.test".to_string());

        Config {
            photos_dir,
            display_socket_path: PathBuf::from("/run/photo-frame/photo-frame.sock"),
            control_socket_path: PathBuf::from("/run/photo-frame/control.sock"),
            native_resolution: "1024x768".to_string(),
            aspect_ratio_mode: AspectRatioMode::Fill,
            batch_delete_size: 10,
            log_max_size: 2048,
            log_max_files: 3,
            remote_sources: vec![RemoteSourceConfig {
                source_type: "immich".to_string(),
                name: "test-source".to_string(),
                params,
                check_interval_seconds: 3600,
            }],
        }
    }

    #[test]
    fn initial_state_prefers_existing_photos() {
        assert_eq!(initial_state(3, false), StartupState::Slideshow);
        assert_eq!(initial_state(3, true), StartupState::Slideshow);
    }

    #[test]
    fn initial_state_waits_for_first_remote_sync() {
        // No photos yet, but remote sources will produce some: do not show the
        // QR screen, show the first-sync status instead.
        assert_eq!(initial_state(0, true), StartupState::FirstSync);
    }

    #[test]
    fn initial_state_enters_config_mode_on_fresh_frame() {
        assert_eq!(initial_state(0, false), StartupState::ConfigMode);
    }

    #[test]
    fn photos_available_reads_the_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!photos_available(dir.path()));

        fs::write(dir.path().join("index-0-0.csv"), "").unwrap();
        assert!(!photos_available(dir.path()));

        fs::write(dir.path().join("index-0-2.csv"), "").unwrap();
        fs::remove_file(dir.path().join("index-0-0.csv")).unwrap();
        assert!(photos_available(dir.path()));
    }

    #[test]
    fn saving_sources_updates_the_in_memory_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let photos_dir = dir.path().join("photos");
        fs::create_dir(&photos_dir).unwrap();
        let config = sample_config(photos_dir);
        let mut env =
            ConfigEnvironment::new(config, &dir.path().join("control.sock"), path.clone());

        let staged = vec![RemoteSourceConfig {
            source_type: "dropbox".to_string(),
            name: "Saved Dropbox".to_string(),
            params: HashMap::from([("access_token".to_string(), "sl.x".to_string())]),
            check_interval_seconds: 600,
        }];

        env.save_staged_sources(staged).unwrap();

        // Both the file and the live config now see the new source, so a
        // second config-mode session starts with it staged.
        let loaded = Config::from_file(&path).unwrap();
        assert_eq!(loaded.remote_sources.len(), 1);
        assert_eq!(loaded.remote_sources[0].name, "Saved Dropbox");
        assert_eq!(env.config.remote_sources.len(), 1);
        assert_eq!(env.config.remote_sources[0].name, "Saved Dropbox");
    }

    #[test]
    fn saving_an_empty_source_list_removes_all_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let photos_dir = dir.path().join("photos");
        fs::create_dir(&photos_dir).unwrap();
        let config = sample_config(photos_dir);
        let mut env =
            ConfigEnvironment::new(config, &dir.path().join("control.sock"), path.clone());

        env.save_staged_sources(Vec::new()).unwrap();

        let loaded = Config::from_file(&path).unwrap();
        assert!(loaded.remote_sources.is_empty());
        assert!(env.config.remote_sources.is_empty());
    }

    #[test]
    fn save_config_atomic_round_trips_and_sets_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let photos_dir = dir.path().join("photos");
        fs::create_dir(&photos_dir).unwrap();
        let config = sample_config(photos_dir);

        save_config_atomic(&path, &config).unwrap();

        let loaded = Config::from_file(&path).unwrap();
        assert_eq!(loaded.photos_dir, config.photos_dir.canonicalize().unwrap());
        assert_eq!(loaded.native_resolution, "1024x768");
        assert_eq!(loaded.batch_delete_size, 10);
        assert_eq!(loaded.remote_sources.len(), 1);
        assert_eq!(loaded.remote_sources[0].source_type, "immich");
        assert_eq!(loaded.remote_sources[0].name, "test-source");
        assert_eq!(
            loaded.remote_sources[0].params.get("url").unwrap(),
            "https://example.test"
        );

        #[cfg(unix)]
        {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // The temporary file must not be left behind after a successful save.
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(names
            .iter()
            .all(|name| !name.to_string_lossy().ends_with(".tmp")));
    }

    #[cfg(unix)]
    #[test]
    fn save_config_atomic_preserves_original_on_failure() {
        // Root bypasses directory permissions, so this failure mode cannot be
        // triggered when tests run as root (e.g. in some containers).
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "original config").unwrap();
        let config = sample_config(dir.path().to_path_buf());

        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o500);
        fs::set_permissions(dir.path(), perms).unwrap();

        let result = save_config_atomic(&path, &config);

        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir.path(), perms).unwrap();

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original config");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
