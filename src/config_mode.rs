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

use crate::admin_server::{AdminServer, AUTH_USERNAME};
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

/// How long to wait before retrying a setup screen that could not be rendered
/// or shown. Retrying never stops the admin server, so login stays possible.
const SCREEN_RETRY_DELAY: Duration = Duration::from_secs(2);

/// How long to wait before retrying a `RESUME` that the display app did not
/// accept (usually because it closed the control socket).
const RESUME_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Log a failing `RESUME` at roughly every 30 seconds, not every 2 seconds, so
/// a long display outage cannot fill the tmpfs log.
const RESUME_LOG_EVERY: u32 = 15;

/// Fallback credentials file, written for as long as config mode is active.
///
/// The password only exists on the QR screen otherwise, so a display app that
/// cannot show it would lock the user out of setup.
pub const SETUP_CREDENTIALS_PATH: &str = "/tmp/photo-frame-setup.txt";

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

/// Where to return after an import ends or a config session closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryState {
    Slideshow,
    FirstSync,
    ConfigMode,
}

/// Recovery after an import stops, for example because the USB was removed.
///
/// With nothing to display, a frame that has never been configured goes back
/// to the setup screen so the user can recover.
fn recovery_after_import(valid_count: usize, has_remote_sources: bool) -> RecoveryState {
    match (valid_count > 0, has_remote_sources) {
        (true, _) => RecoveryState::Slideshow,
        (false, true) => RecoveryState::FirstSync,
        (false, false) => RecoveryState::ConfigMode,
    }
}

/// Recovery after config mode ends (finish, USB removal, or idle timeout).
///
/// Setup is deliberately not reopened: the user either saved a config or
/// cancelled the session, so bouncing straight back into the QR screen would be
/// surprising. Remote sources still get the first-sync screen.
fn recovery_after_config(valid_count: usize, has_remote_sources: bool) -> RecoveryState {
    match (valid_count > 0, has_remote_sources) {
        (true, _) => RecoveryState::Slideshow,
        (false, true) => RecoveryState::FirstSync,
        (false, false) => RecoveryState::Slideshow,
    }
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

/// Number of valid photos currently in the index.
fn photo_count(photos_dir: &Path) -> usize {
    crate::index::init_index(photos_dir)
        .map(|(_, meta)| meta.valid_count)
        .unwrap_or(0)
}

/// Whether the index currently reports at least one photo.
fn photos_available(photos_dir: &Path) -> bool {
    photo_count(photos_dir) > 0
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
        write_private_file(&tmp_path, &toml_str).and_then(|()| std::fs::rename(&tmp_path, path));

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }

    result
}

/// Write `contents` to `path`, creating it with mode 0600.
fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;

    // Explicitly set the mode as well: the open mode is filtered through the
    // process umask, and we always want private files to end up as 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }

    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

/// Human-readable contents of the setup-credentials fallback file.
///
/// It lists the same reachable URL and password that the QR screen shows, so
/// the user can still finish setup when the screen cannot be displayed.
pub(crate) fn setup_credentials_text(fallback_ip: &str, port: u16, password: &str) -> String {
    format!(
        "Photo Frame setup\n\
         =================\n\
         \n\
         URL:      http://{fallback_ip}:{port}\n\
         Also try: http://{mdns}:{port}\n\
         Username: {username}\n\
         Password: {password}\n\
         \n\
         This file is removed when config mode exits.\n",
        mdns = qr::MDNS_HOST,
        username = AUTH_USERNAME,
    )
}

/// Write the fallback setup credentials to `path` with mode 0600.
///
/// Any existing file is removed first so a stale file left by a crashed run
/// (possibly owned by a different user) cannot block the write or keep a
/// password that no longer works.
pub(crate) fn write_setup_credentials(
    path: &Path,
    fallback_ip: &str,
    port: u16,
    password: &str,
) -> io::Result<()> {
    remove_setup_credentials(path)?;
    write_private_file(path, &setup_credentials_text(fallback_ip, port, password))
}

/// Remove the fallback credentials file, treating "already gone" as success.
///
/// Called when config mode stops. `main` also calls it directly on shutdown,
/// because worker threads are not joined and may not reach their own cleanup.
pub fn remove_setup_credentials(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A setup screen that still needs to be shown on the display.
///
/// Render or control-socket failures no longer abort config mode; they leave
/// this pending record behind so the screen is retried while the admin server
/// (and the credentials fallback file) keep working.
struct PendingScreen {
    fallback_ip: String,
    port: u16,
    password: String,
    retry_at: Instant,
}

/// A `RESUME` that the display app has not accepted yet.
///
/// The display app can close its control socket (restart, crash, or a broken
/// pipe on write), leaving the QR or status screen on the panel. Keep retrying
/// until the command lands instead of warning once and giving up.
struct PendingResume {
    retry_at: Instant,
    /// Failed attempts so far, used only for log throttling.
    attempts: u32,
}

/// Owns the config-mode state machine.  Communicates with the USB-watcher via
/// the shared `MountEvent` channel.
pub struct ConfigEnvironment {
    state: ConfigState,
    control: ControlClient,
    config: Config,
    config_path: PathBuf,
    /// Where the fallback setup credentials are written while config mode runs.
    credentials_path: PathBuf,
    /// Where the setup QR screen is rendered before it is sent to the display.
    screen_path: PathBuf,
    server: Option<Arc<AdminServer>>,
    server_handle: Option<JoinHandle<()>>,
    server_shutdown: Arc<AtomicBool>,
    pending_screen: Option<PendingScreen>,
    resume_pending: Option<PendingResume>,
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
            credentials_path: PathBuf::from(SETUP_CREDENTIALS_PATH),
            screen_path: PathBuf::from(qr::DEFAULT_CONFIG_SCREEN_PATH),
            server: None,
            server_handle: None,
            server_shutdown: Arc::new(AtomicBool::new(false)),
            pending_screen: None,
            resume_pending: None,
        }
    }

    /// Enter config mode: start the admin server, write the credentials
    /// fallback, then try to display the QR code.
    ///
    /// Only a failure to create the server is fatal here. A screen that cannot
    /// be rendered or shown is logged and retried, but the state still becomes
    /// [`ConfigState::ConfigMode`] so the user can log in using the URL and
    /// password in the fallback file.
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

        // A new session is about to pause the display again, so a queued
        // RESUME from an earlier session is obsolete.
        self.resume_pending = None;

        // Write the fallback before touching the display app. The password only
        // exists on screen otherwise, so this file is the safety net when the
        // QR screen cannot be shown.
        match write_setup_credentials(&self.credentials_path, fallback_ip, port, &password) {
            Ok(()) => log::info!(
                "Setup credentials for http://{fallback_ip}:{port} written to {} (mode 0600)",
                self.credentials_path.display()
            ),
            Err(e) => log::warn!(
                "Could not write setup credentials to {}: {e}",
                self.credentials_path.display()
            ),
        }

        let pending = PendingScreen {
            fallback_ip: fallback_ip.to_string(),
            port,
            password,
            retry_at: Instant::now(),
        };
        if !self.show_setup_screen(&pending) {
            self.pending_screen = Some(PendingScreen {
                retry_at: Instant::now() + SCREEN_RETRY_DELAY,
                ..pending
            });
        }

        log::info!(
            "Config mode active: http://{fallback_ip}:{port} (or http://{}:{port})",
            qr::MDNS_HOST
        );
        self.state = ConfigState::ConfigMode;

        Ok(())
    }

    /// Render and show the setup QR screen. Returns whether it was displayed.
    ///
    /// Failures are logged, never propagated: the admin server must stay
    /// reachable even when the display app is not.
    fn show_setup_screen(&mut self, screen: &PendingScreen) -> bool {
        let qr_path = match qr::render_config_screen_to(
            &self.config,
            &screen.fallback_ip,
            qr::MDNS_HOST,
            screen.port,
            &screen.password,
            &self.screen_path,
        ) {
            Ok(path) => path,
            Err(e) => {
                log::warn!("Could not render setup screen: {e}");
                return false;
            }
        };

        if let Err(e) = self
            .control
            .clear()
            .and_then(|()| self.control.show(&qr_path))
        {
            log::warn!(
                "Could not display setup screen: {e}; setup is still available at \
                 http://{}:{} (credentials: {})",
                screen.fallback_ip,
                screen.port,
                self.credentials_path.display()
            );
            return false;
        }

        true
    }

    /// Retry a setup screen that could not be shown earlier, if it is due.
    ///
    /// The admin server and fallback file are left untouched, so this can run
    /// while the user is logged in.
    fn retry_setup_screen(&mut self) {
        if !matches!(self.state, ConfigState::ConfigMode) {
            return;
        }
        let Some(pending) = self.pending_screen.take() else {
            return;
        };
        if Instant::now() < pending.retry_at {
            self.pending_screen = Some(pending);
            return;
        }

        // Re-create the fallback if it is missing (a transient write failure at
        // entry, or something cleaned up /tmp). Without it the user is locked
        // out whenever the screen also cannot be shown.
        if !self.credentials_path.exists() {
            match write_setup_credentials(
                &self.credentials_path,
                &pending.fallback_ip,
                pending.port,
                &pending.password,
            ) {
                Ok(()) => log::info!(
                    "Restored setup credentials at {}",
                    self.credentials_path.display()
                ),
                Err(e) => log::warn!(
                    "Could not restore setup credentials {}: {e}",
                    self.credentials_path.display()
                ),
            }
        }

        if !self.show_setup_screen(&pending) {
            self.pending_screen = Some(PendingScreen {
                retry_at: Instant::now() + SCREEN_RETRY_DELAY,
                ..pending
            });
        }
    }

    /// Try to enter config mode, falling back to a retry when the admin server
    /// cannot be created. Never leaves the state machine without a server retry.
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

    /// Stop the admin server and remove the fallback credentials, if any.
    fn stop_server(&mut self) {
        self.server_shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.join();
        }
        self.server = None;
        self.pending_screen = None;

        if let Err(e) = remove_setup_credentials(&self.credentials_path) {
            log::warn!(
                "Could not remove setup credentials {}: {e}",
                self.credentials_path.display()
            );
        }
    }

    /// Leave config mode: stop the admin server and recover to the right
    /// screen. If `save` is true, write the staged remote sources to
    /// config.toml first.
    fn leave_config_mode(&mut self, save: bool, fallback_ip: &str) {
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

        let recovery = recovery_after_config(
            photo_count(&self.config.photos_dir),
            !self.config.remote_sources.is_empty(),
        );
        self.recover_to(recovery, fallback_ip);
    }

    /// Apply a recovery decision, updating the display and the state.
    fn recover_to(&mut self, recovery: RecoveryState, fallback_ip: &str) {
        match recovery {
            RecoveryState::Slideshow => {
                self.state = ConfigState::Normal;
                self.resume_slideshow();
            }
            RecoveryState::FirstSync => {
                self.state = ConfigState::FirstSync;
                self.show_first_sync_screen();
            }
            RecoveryState::ConfigMode => self.begin_config_mode(fallback_ip),
        }
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

    /// Send `RESUME` and report whether the display app accepted it.
    ///
    /// A failure queues a retry instead of only warning, because a dead control
    /// socket would otherwise leave the QR or status screen up forever.
    fn resume_slideshow(&mut self) -> bool {
        match self.control.resume() {
            Ok(()) => {
                if let Some(pending) = self.resume_pending.take() {
                    log::info!(
                        "Slideshow resumed after {} failed attempt(s)",
                        pending.attempts
                    );
                }
                true
            }
            Err(e) => {
                let attempts = self
                    .resume_pending
                    .as_ref()
                    .map_or(1, |pending| pending.attempts + 1);
                if attempts == 1 || attempts.is_multiple_of(RESUME_LOG_EVERY) {
                    log::warn!(
                        "Could not resume slideshow ({e}); retrying every {RESUME_RETRY_DELAY:?}"
                    );
                }
                self.resume_pending = Some(PendingResume {
                    retry_at: Instant::now() + RESUME_RETRY_DELAY,
                    attempts,
                });
                false
            }
        }
    }

    /// Retry a queued `RESUME` when it is due.
    ///
    /// Only runs in `Normal`: in every other state the display is intentionally
    /// showing a setup or status screen, so resuming would fight it.
    fn retry_resume(&mut self) {
        if !matches!(self.state, ConfigState::Normal) {
            return;
        }
        let Some(pending) = self.resume_pending.as_ref() else {
            return;
        };
        if Instant::now() < pending.retry_at {
            return;
        }
        self.resume_slideshow();
    }

    /// Draw a simple status screen (importing, waiting for first sync, ...).
    fn show_status_screen(&mut self, title: &str, lines: &[&str]) {
        // This screen pauses the slideshow, so any queued RESUME is obsolete.
        self.resume_pending = None;

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
                        let recovery = recovery_after_import(
                            photo_count(&self.config.photos_dir),
                            !self.config.remote_sources.is_empty(),
                        );
                        self.recover_to(recovery, fallback_ip);
                    }
                } else if matches!(
                    self.state,
                    ConfigState::ConfigMode | ConfigState::PendingConfigMode { .. }
                ) {
                    log::info!("USB removed — leaving config mode ({})", path.display());
                    self.leave_config_mode(false, fallback_ip);
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
                env.leave_config_mode(save, &fallback_ip);
            } else {
                env.stop_server();
            }

            // Leaving config mode may have queued a RESUME. Try once more before
            // exiting so the panel is not left holding the QR/status image.
            if env.resume_pending.is_some() {
                log::info!("Making a final RESUME attempt before shutdown");
                if !env.resume_slideshow() {
                    log::warn!(
                        "Slideshow may still be paused; it will recover when the display app restarts"
                    );
                }
            }
            break;
        }

        // Check if the user finished setup or the session timed out.
        if matches!(env.state, ConfigState::ConfigMode) {
            if let Some(ref server) = env.server {
                if server.finished() {
                    log::info!("User finished setup — saving config");
                    env.leave_config_mode(true, &fallback_ip);
                    continue;
                }
                if server.timed_out() {
                    log::info!("Config mode timed out");
                    env.leave_config_mode(false, &fallback_ip);
                    continue;
                }
            }
        }

        // Retry a setup screen that could not be shown earlier. This never
        // stops the admin server, so the fallback file keeps working.
        env.retry_setup_screen();

        // Retry a RESUME the display app never accepted.
        env.retry_resume();

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
    fn recovery_after_import_returns_to_setup_when_empty() {
        // A cancelled import on an unconfigured frame should reopen setup
        // rather than run a slideshow with nothing in it.
        assert_eq!(recovery_after_import(0, false), RecoveryState::ConfigMode);
        assert_eq!(recovery_after_import(0, true), RecoveryState::FirstSync);
        assert_eq!(recovery_after_import(5, false), RecoveryState::Slideshow);
        assert_eq!(recovery_after_import(5, true), RecoveryState::Slideshow);
    }

    #[test]
    fn recovery_after_config_does_not_reopen_setup() {
        // Cancelling or timing out config mode must not bounce the user
        // straight back into the QR screen.
        assert_eq!(recovery_after_config(0, false), RecoveryState::Slideshow);
        // Remote sources still deserve the first-sync screen.
        assert_eq!(recovery_after_config(0, true), RecoveryState::FirstSync);
        assert_eq!(recovery_after_config(5, false), RecoveryState::Slideshow);
        assert_eq!(recovery_after_config(5, true), RecoveryState::Slideshow);
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

    /// Build a config environment whose fallback file, screen file, and
    /// control socket all live in `dir`, with a control client that fails fast.
    fn test_env(dir: &Path, config: Config) -> ConfigEnvironment {
        let mut env =
            ConfigEnvironment::new(config, &dir.join("control.sock"), dir.join("config.toml"));
        env.credentials_path = dir.join("setup.txt");
        env.screen_path = dir.join("setup.jpg");
        env.control = ControlClient::with_timeout(
            &dir.join("missing-control.sock"),
            Duration::from_millis(50),
        );
        env
    }

    #[test]
    fn setup_credentials_file_lists_urls_and_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.txt");

        write_setup_credentials(&path, "192.168.1.208", 8100, "abc12345").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("http://192.168.1.208:8100"),
            "primary URL missing: {contents}"
        );
        assert!(
            contents.contains("http://photo-frame.local:8100"),
            "mDNS URL missing: {contents}"
        );
        assert!(
            contents.contains("Username: photo-frame"),
            "username missing: {contents}"
        );
        assert!(
            contents.contains("Password: abc12345"),
            "password missing: {contents}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn setup_credentials_file_is_private_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.txt");

        // A stale, loosely-permissioned file must be replaced and tightened.
        fs::write(&path, "stale").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_setup_credentials(&path, "10.0.0.5", 8100, "pw123456").unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("stale"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        remove_setup_credentials(&path).unwrap();
        assert!(!path.exists());

        // Removing an already-absent file is not an error.
        remove_setup_credentials(&path).unwrap();
    }

    #[test]
    fn config_mode_entry_survives_a_control_failure() {
        let dir = tempfile::tempdir().unwrap();
        let photos_dir = dir.path().join("photos");
        fs::create_dir(&photos_dir).unwrap();
        // One valid photo so leaving config mode resumes the slideshow.
        fs::write(photos_dir.join("index-0-1.csv"), "").unwrap();

        let mut env = test_env(dir.path(), sample_config(photos_dir));
        let credentials = env.credentials_path.clone();

        // The control socket is absent, so CLR/SHOW fail.
        env.enter_config_mode("127.0.0.1").unwrap();

        // Login still works: the server is up, the state is ConfigMode, and the
        // fallback file has the URL and password the screen could not show.
        assert_eq!(env.state, ConfigState::ConfigMode);
        assert!(env.server.is_some(), "admin server must stay running");
        assert!(
            env.pending_screen.is_some(),
            "a failed screen should be queued for retry"
        );

        let port = env.server.as_ref().unwrap().port();
        let contents = fs::read_to_string(&credentials).unwrap();
        assert!(contents.contains(&format!("http://127.0.0.1:{port}")));
        assert!(contents.contains(&format!("http://{}:{port}", qr::MDNS_HOST)));
        assert!(contents.contains(&format!("Username: {AUTH_USERNAME}")));
        let password = contents
            .lines()
            .find_map(|line| line.strip_prefix("Password: "))
            .expect("password line");
        assert_eq!(password.len(), 8);
        assert!(password.chars().all(|c| c.is_ascii_alphanumeric()));

        env.leave_config_mode(false, "127.0.0.1");

        assert!(
            !credentials.exists(),
            "leaving config mode removes the fallback file"
        );
        assert!(env.server.is_none());
        assert!(env.pending_screen.is_none());
        assert_eq!(env.state, ConfigState::Normal);
        assert!(
            env.resume_pending.is_some(),
            "the failed RESUME should be queued for retry"
        );
    }

    #[test]
    fn failed_setup_screen_is_retried_without_stopping_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = test_env(dir.path(), sample_config(dir.path().to_path_buf()));

        env.enter_config_mode("127.0.0.1").unwrap();
        let port = env.server.as_ref().unwrap().port();
        assert_eq!(env.state, ConfigState::ConfigMode);

        // Make the queued retry due now.
        env.pending_screen.as_mut().unwrap().retry_at = Instant::now();
        env.retry_setup_screen();

        assert_eq!(env.state, ConfigState::ConfigMode);
        assert_eq!(
            env.server.as_ref().map(|server| server.port()),
            Some(port),
            "the retry must not replace or stop the admin server"
        );
        let pending = env.pending_screen.as_ref().expect("retry stays queued");
        assert!(
            pending.retry_at > Instant::now(),
            "the next retry should be delayed"
        );
        assert!(env.credentials_path.exists());

        env.stop_server();
    }

    #[test]
    fn retrying_the_setup_screen_restores_a_missing_fallback_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = test_env(dir.path(), sample_config(dir.path().to_path_buf()));

        env.enter_config_mode("127.0.0.1").unwrap();
        remove_setup_credentials(&env.credentials_path).unwrap();
        assert!(!env.credentials_path.exists());

        env.pending_screen.as_mut().unwrap().retry_at = Instant::now();
        env.retry_setup_screen();

        assert_eq!(env.state, ConfigState::ConfigMode);
        assert!(
            env.credentials_path.exists(),
            "the retry should restore the fallback file"
        );

        env.stop_server();
    }

    #[test]
    fn failed_resume_queues_a_retry() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = test_env(dir.path(), sample_config(dir.path().to_path_buf()));
        assert_eq!(env.state, ConfigState::Normal);

        assert!(!env.resume_slideshow(), "RESUME must fail without a socket");

        let pending = env
            .resume_pending
            .as_ref()
            .expect("a failed RESUME should be queued");
        assert_eq!(pending.attempts, 1);
        assert!(
            pending.retry_at > Instant::now(),
            "the retry should be delayed, not immediate"
        );

        // Not due yet: calling the loop helper must not send another RESUME.
        env.retry_resume();
        assert_eq!(env.resume_pending.as_ref().unwrap().attempts, 1);
    }

    #[test]
    fn resume_retry_clears_when_the_display_app_returns() {
        use std::io::Read;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let mut env = test_env(dir.path(), sample_config(dir.path().to_path_buf()));
        let socket_path = dir.path().join("missing-control.sock");

        // Start with no display app: RESUME fails and is queued.
        assert!(!env.resume_slideshow());
        assert!(env.resume_pending.is_some());

        // The display app comes back and binds its control socket.
        let listener = UnixListener::bind(&socket_path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 32];
            let n = stream.read(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        env.resume_pending.as_mut().unwrap().retry_at = Instant::now();
        env.retry_resume();

        assert!(
            env.resume_pending.is_none(),
            "a successful RESUME clears the retry"
        );
        assert_eq!(handle.join().unwrap(), "RESUME\n");
    }

    #[test]
    fn entering_config_mode_clears_a_pending_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = test_env(dir.path(), sample_config(dir.path().to_path_buf()));

        assert!(!env.resume_slideshow());
        assert!(env.resume_pending.is_some());

        env.enter_config_mode("127.0.0.1").unwrap();

        assert!(
            env.resume_pending.is_none(),
            "config mode pauses the display again"
        );
        env.stop_server();
    }

    #[test]
    fn status_screen_cancels_a_pending_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = test_env(dir.path(), sample_config(dir.path().to_path_buf()));

        assert!(!env.resume_slideshow());
        assert!(env.resume_pending.is_some());

        env.show_status_screen("Importing photos...", &["Please wait."]);

        assert!(
            env.resume_pending.is_none(),
            "showing a status screen pauses the slideshow"
        );
    }
}
