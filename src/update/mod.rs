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

//! Signed, app-only release updates.
//!
//! The layout is deliberately small and power-loss friendly:
//!
//! ```text
//! /opt/photo-frame/
//!   releases/<version>/{manifest.json,bin/...}
//!   current -> releases/<version>      (symlink, replaced with rename)
//!   .staging/                          (same filesystem, never active)
//! /var/lib/photo-frame/update/state.json
//! /var/log/photo-frame/update.log
//! ```
//!
//! A bundle is a `.tar.gz` with `manifest.json` and `bin/photo-frame-{manager,
//! display,update}`. It is accompanied by `<bundle>.sha256` and
//! `<bundle>.sha256.sig`; the latter is an OpenSSH Ed25519 signature over the
//! exact bytes of the checksum file. Nothing is extracted or activated until
//! the signature, sidecar checksum, and caller-supplied checksum all match.

mod archive;
mod audit;
mod checksum;
mod cli;
mod health;
mod install;
mod keys;
mod signature;
mod ssh_signature;
mod state;

use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;

pub use archive::{
    extract_bundle, read_manifest, verify_bundle_tree, Manifest, ManifestFile, REQUIRED_BINARIES,
};
pub use audit::{append_audit, AuditRecord};
pub use checksum::{is_sha256_hex, parse_sidecar, sha256_file, sha256_hex, Sidecar};
pub use cli::run;
pub use health::{HealthCheck, HealthMode, Services, CONTROL_SOCKET, DISPLAY_SOCKET};
pub use install::{
    activate, current_version, gc, guard, install, list_releases, rollback, status, GcReport,
    GuardAction, GuardReport, InstallOptions, InstallReport, ReleaseInfo, RollbackReport,
    StatusReport,
};
pub use keys::{TrustedKey, TRUSTED_KEYS};
pub use signature::{verify_signature, VerifiedSignature};
pub use state::{Pending, State};

/// Production install root for versioned releases.
pub const DEFAULT_ROOT: &str = "/opt/photo-frame";
/// Production update state directory.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/photo-frame/update";
/// Production audit log (persistent storage, root-only).
pub const DEFAULT_LOG_PATH: &str = "/var/log/photo-frame/update.log";

/// Errors from update verification and installation.
///
/// Variants are intentionally coarse: callers only need to tell a rejected
/// bundle (`Unsupported`/`Verification`) from a local failure
/// (`Io`/`State`/`Health`) and from usage mistakes.
#[derive(Debug)]
pub enum UpdateError {
    /// Local filesystem failure.
    Io(String),
    /// Signature, checksum, or manifest/hash verification failed.
    Verification(String),
    /// Well-formed but unacceptable: wrong arch, downgrade, unknown format.
    Unsupported(String),
    /// Downloading a remote bundle failed.
    Network(String),
    /// The update state file is missing, corrupt, or inconsistent.
    State(String),
    /// The new release did not become healthy.
    Health(String),
    /// The command line was wrong.
    Usage(String),
}

impl UpdateError {
    /// Stable short label, used in error output and audit details.
    pub fn kind(&self) -> &'static str {
        match self {
            UpdateError::Io(_) => "io",
            UpdateError::Verification(_) => "verification",
            UpdateError::Unsupported(_) => "unsupported",
            UpdateError::Network(_) => "network",
            UpdateError::State(_) => "state",
            UpdateError::Health(_) => "health",
            UpdateError::Usage(_) => "usage",
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            UpdateError::Io(m)
            | UpdateError::Verification(m)
            | UpdateError::Unsupported(m)
            | UpdateError::Network(m)
            | UpdateError::State(m)
            | UpdateError::Health(m)
            | UpdateError::Usage(m) => m,
        };
        write!(f, "{}", message)
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(err: std::io::Error) -> Self {
        UpdateError::Io(err.to_string())
    }
}

/// Result alias for update operations.
pub type Result<T> = std::result::Result<T, UpdateError>;

/// Filesystem locations used by every update operation.
///
/// Defaults are the production paths; tests and the hidden dev flags override
/// them explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Install root, containing `releases/` and the `current` symlink.
    pub root: PathBuf,
    /// Directory holding `state.json`.
    pub state_dir: PathBuf,
    /// Append-only audit log.
    pub log_path: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            root: PathBuf::from(DEFAULT_ROOT),
            state_dir: PathBuf::from(DEFAULT_STATE_DIR),
            log_path: PathBuf::from(DEFAULT_LOG_PATH),
        }
    }
}

impl Paths {
    /// Production paths with an overridden root and state directory.
    pub fn new(root: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state_dir: state_dir.into(),
            log_path: PathBuf::from(DEFAULT_LOG_PATH),
        }
    }

    /// Override the audit log path (tests and dev use only).
    pub fn with_log_path(mut self, log_path: impl Into<PathBuf>) -> Self {
        self.log_path = log_path.into();
        self
    }

    /// Directory holding one subdirectory per installed version.
    pub fn releases_dir(&self) -> PathBuf {
        self.root.join("releases")
    }

    /// Staging area on the same filesystem as `releases/`.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(".staging")
    }

    /// The active release symlink.
    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    /// Path of the JSON state file.
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    /// Path of a release directory.
    pub fn release_dir(&self, version: &str) -> PathBuf {
        self.releases_dir().join(version)
    }
}

/// Compare dotted numeric versions. Non-numeric suffixes are ignored, so
/// `1.2.3-rc1` compares equal to `1.2.3`. Missing trailing components count as
/// zero (`1.2` == `1.2.0`).
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn segments(version: &str) -> Vec<u64> {
        version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .map(|part| {
                let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse::<u64>().unwrap_or(0)
            })
            .collect()
    }

    let mut left = segments(a);
    let mut right = segments(b);
    let len = left.len().max(right.len());
    left.resize(len, 0);
    right.resize(len, 0);
    left.cmp(&right)
}

/// A version string that is safe to use as a single directory name.
pub(crate) fn is_safe_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version != "."
        && version != ".."
        && !version.starts_with('.')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.2.1", "0.2.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.2.0", "0.2.1"), Ordering::Less);
        assert_eq!(compare_versions("0.2", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("v1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.3-rc1", "1.2.3"), Ordering::Equal);
    }

    #[test]
    fn test_is_safe_version() {
        assert!(is_safe_version("0.2.1"));
        assert!(is_safe_version("0.2.1-rc1+build5"));
        assert!(!is_safe_version(""));
        assert!(!is_safe_version("."));
        assert!(!is_safe_version(".."));
        assert!(!is_safe_version(".hidden"));
        assert!(!is_safe_version("0.2/evil"));
        assert!(!is_safe_version("0.2 1"));
    }

    #[test]
    fn test_paths_layout() {
        let paths = Paths::new("/opt/photo-frame", "/var/lib/photo-frame/update");
        assert_eq!(
            paths.releases_dir(),
            PathBuf::from("/opt/photo-frame/releases")
        );
        assert_eq!(
            paths.staging_dir(),
            PathBuf::from("/opt/photo-frame/.staging")
        );
        assert_eq!(
            paths.current_link(),
            PathBuf::from("/opt/photo-frame/current")
        );
        assert_eq!(
            paths.state_file(),
            PathBuf::from("/var/lib/photo-frame/update/state.json")
        );
        assert_eq!(
            paths.log_path,
            PathBuf::from("/var/log/photo-frame/update.log")
        );
        assert_eq!(
            paths.release_dir("0.2.1"),
            PathBuf::from("/opt/photo-frame/releases/0.2.1")
        );
    }
}
