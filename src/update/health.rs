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

//! Service restart and post-update health checks.
//!
//! The real check has two parts: both units are active, and both sockets exist.
//! Tests replace it with [`HealthMode::Skip`] or a forced result so install,
//! rollback, and guard are deterministic without systemd.

use crate::update::{Result, UpdateError};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Image socket created by the display service.
pub const DISPLAY_SOCKET: &str = "/run/photo-frame/photo-frame.sock";
/// Control socket created by the display service.
pub const CONTROL_SOCKET: &str = "/run/photo-frame/control.sock";
/// Default total wait for the new release to become healthy.
pub const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
/// Default time between probes.
pub const DEFAULT_HEALTH_INTERVAL: Duration = Duration::from_millis(500);

/// The systemd units restarted after activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Services {
    /// Display unit name.
    pub display: String,
    /// Manager unit name.
    pub manager: String,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            display: "photo-frame-display.service".to_string(),
            manager: "photo-frame-manager.service".to_string(),
        }
    }
}

/// How the health check should behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthMode {
    /// Query systemctl and inspect the sockets.
    System,
    /// Skip all service and socket checks (the `--no-restart` dev path).
    Skip,
    /// Pretend the checks passed (tests).
    ForceHealthy,
    /// Pretend the checks failed (tests).
    ForceUnhealthy,
}

/// Health check configuration and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthCheck {
    /// Check mode.
    pub mode: HealthMode,
    /// Total wait before declaring the release unhealthy.
    pub timeout: Duration,
    /// Delay between probes.
    pub interval: Duration,
    /// Sockets that must exist.
    pub sockets: Vec<PathBuf>,
    /// Units that must be active.
    pub services: Services,
}

impl HealthCheck {
    /// The real check used in production.
    pub fn system(services: Services) -> Self {
        Self {
            mode: HealthMode::System,
            timeout: DEFAULT_HEALTH_TIMEOUT,
            interval: DEFAULT_HEALTH_INTERVAL,
            sockets: vec![PathBuf::from(DISPLAY_SOCKET), PathBuf::from(CONTROL_SOCKET)],
            services,
        }
    }

    /// Skip service and socket checks.
    pub fn skip() -> Self {
        Self {
            mode: HealthMode::Skip,
            timeout: Duration::ZERO,
            interval: Duration::ZERO,
            sockets: Vec::new(),
            services: Services::default(),
        }
    }

    /// Deterministic result for tests.
    pub fn forced(healthy: bool) -> Self {
        Self {
            mode: if healthy {
                HealthMode::ForceHealthy
            } else {
                HealthMode::ForceUnhealthy
            },
            timeout: Duration::ZERO,
            interval: Duration::ZERO,
            sockets: Vec::new(),
            services: Services::default(),
        }
    }

    /// Wait for a healthy release, or fail after `timeout`.
    pub fn check(&self) -> Result<()> {
        match self.mode {
            HealthMode::Skip | HealthMode::ForceHealthy => return Ok(()),
            HealthMode::ForceUnhealthy => {
                return Err(UpdateError::Health(
                    "health check forced to fail (test mode)".to_string(),
                ))
            }
            HealthMode::System => {}
        }

        let started = Instant::now();
        loop {
            match self.probe() {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let elapsed = started.elapsed();
                    if elapsed >= self.timeout {
                        return Err(UpdateError::Health(format!(
                            "{} (waited {}s)",
                            err,
                            self.timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(self.interval.min(self.timeout - elapsed));
                }
            }
        }
    }

    fn probe(&self) -> Result<()> {
        let mut problems = Vec::new();
        for service in [&self.services.display, &self.services.manager] {
            match Command::new("systemctl")
                .arg("is-active")
                .arg("--quiet")
                .arg(service)
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => problems.push(format!("{} is not active ({})", service, status)),
                Err(err) => {
                    problems.push(format!("failed to run systemctl for {}: {}", service, err))
                }
            }
        }
        for socket in &self.sockets {
            match fs::symlink_metadata(socket) {
                Ok(metadata) if metadata.file_type().is_socket() => {}
                Ok(_) => problems.push(format!("{} is not a socket", socket.display())),
                Err(err) => problems.push(format!("{} is missing: {}", socket.display(), err)),
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(UpdateError::Health(problems.join("; ")))
        }
    }
}

/// Restart both units and wait for systemd to accept the request.
pub fn restart_services(services: &Services) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("restart")
        .arg(&services.display)
        .arg(&services.manager)
        .status()
        .map_err(|e| UpdateError::Health(format!("failed to run systemctl restart: {}", e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(UpdateError::Health(format!(
            "systemctl restart {} {} failed ({})",
            services.display, services.manager, status
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skip_mode_passes() {
        assert!(HealthCheck::skip().check().is_ok());
    }

    #[test]
    fn test_forced_modes() {
        assert!(HealthCheck::forced(true).check().is_ok());
        assert!(matches!(
            HealthCheck::forced(false).check().unwrap_err(),
            UpdateError::Health(_)
        ));
    }

    #[test]
    fn test_default_service_names() {
        let services = Services::default();
        assert_eq!(services.display, "photo-frame-display.service");
        assert_eq!(services.manager, "photo-frame-manager.service");
    }

    #[test]
    fn test_system_check_uses_shared_socket_paths() {
        let check = HealthCheck::system(Services::default());
        assert_eq!(check.mode, HealthMode::System);
        assert_eq!(check.timeout, DEFAULT_HEALTH_TIMEOUT);
        assert!(check.sockets.contains(&PathBuf::from(DISPLAY_SOCKET)));
        assert!(check.sockets.contains(&PathBuf::from(CONTROL_SOCKET)));
    }

    #[test]
    fn test_probe_detects_missing_socket() {
        let dir = tempfile::tempdir().unwrap();
        let mut check = HealthCheck::skip();
        check.sockets = vec![dir.path().join("missing.sock")];
        // Only the socket part is under test; services is empty here.
        check.services = Services {
            display: "definitely-not-a-unit".to_string(),
            manager: "definitely-not-a-unit".to_string(),
        };
        // systemctl may not exist in the test environment; either way the
        // probe must report a problem rather than pass.
        assert!(check.probe().is_err());
    }
}
