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

//! Update state: `state.json` with atomic, mode-0600 writes.
//!
//! `sequence` is the anti-rollback floor, not the sequence of whatever release
//! happens to be active. Rollbacks deliberately leave it alone.

use crate::update::{is_safe_version, Result, UpdateError};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// Persisted update state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Release currently selected by the `current` symlink.
    #[serde(default)]
    pub current: Option<String>,
    /// Release to fall back to (the one that was active before `current`).
    #[serde(default)]
    pub previous: Option<String>,
    /// Most recent release that passed its health check.
    #[serde(default)]
    pub last_good: Option<String>,
    /// In-flight install that has not been confirmed yet.
    #[serde(default)]
    pub pending: Option<Pending>,
    /// Highest sequence ever installed; installs below it are refused.
    #[serde(default)]
    pub sequence: u64,
}

/// An install that has been activated but not yet confirmed by a health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// Release that was active before this install.
    #[serde(default)]
    pub from: Option<String>,
    /// Release being activated.
    pub to: String,
    /// Sequence carried by the pending bundle.
    pub sequence: u64,
    /// Trusted key ID that signed the bundle.
    #[serde(default)]
    pub key_id: String,
    /// Local path or URL the bundle came from.
    #[serde(default)]
    pub source: String,
    /// RFC 3339 timestamp of when activation began.
    #[serde(default)]
    pub started_at: String,
}

impl State {
    /// Load state, treating a missing file as the empty default.
    pub fn load(path: &Path) -> Result<State> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(err) => {
                return Err(UpdateError::State(format!(
                    "failed to read {}: {}",
                    path.display(),
                    err
                )))
            }
        };
        if metadata.len() > MAX_STATE_BYTES {
            return Err(UpdateError::State(format!(
                "{} is larger than {} bytes",
                path.display(),
                MAX_STATE_BYTES
            )));
        }
        let bytes = fs::read(path)
            .map_err(|e| UpdateError::State(format!("failed to read {}: {}", path.display(), e)))?;
        let state: State = serde_json::from_slice(&bytes).map_err(|e| {
            UpdateError::State(format!(
                "{} is not valid update state: {}",
                path.display(),
                e
            ))
        })?;
        state.validate()?;
        Ok(state)
    }

    /// Persist state atomically (temp file + rename), mode 0600.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| UpdateError::State(format!("failed to serialize state: {}", e)))?;
        bytes.push(b'\n');
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                UpdateError::State(format!("failed to create {}: {}", parent.display(), e))
            })?;
            let _ =
                fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        }
        atomic_write(path, &bytes)
    }

    fn validate(&self) -> Result<()> {
        for version in [
            self.current.as_deref(),
            self.previous.as_deref(),
            self.last_good.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !is_safe_version(version) {
                return Err(UpdateError::State(format!(
                    "state contains an unsafe version name '{}'",
                    version
                )));
            }
        }
        if let Some(pending) = &self.pending {
            if !is_safe_version(&pending.to) {
                return Err(UpdateError::State(format!(
                    "pending state contains an unsafe version name '{}'",
                    pending.to
                )));
            }
            if let Some(from) = &pending.from {
                if !is_safe_version(from) {
                    return Err(UpdateError::State(format!(
                        "pending state contains an unsafe version name '{}'",
                        from
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Write `bytes` to `path` via a sibling temp file and rename.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "state.json".into());
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let temp = parent.join(temp_name);

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| {
                UpdateError::State(format!("failed to create {}: {}", temp.display(), e))
            })?;
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|e| {
                UpdateError::State(format!("failed to set mode on {}: {}", temp.display(), e))
            })?;
        file.write_all(bytes).map_err(|e| {
            UpdateError::State(format!("failed to write {}: {}", temp.display(), e))
        })?;
        file.sync_all().map_err(|e| {
            UpdateError::State(format!("failed to flush {}: {}", temp.display(), e))
        })?;
        fs::rename(&temp, path).map_err(|e| {
            UpdateError::State(format!(
                "failed to rename {} to {}: {}",
                temp.display(),
                path.display(),
                e
            ))
        })?;
        sync_dir(parent)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Flush a directory entry so a rename survives power loss.
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    let dir = fs::File::open(path)
        .map_err(|e| UpdateError::Io(format!("failed to open {}: {}", path.display(), e)))?;
    dir.sync_all()
        .map_err(|e| UpdateError::Io(format!("failed to flush {}: {}", path.display(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn pending(to: &str) -> Pending {
        Pending {
            from: Some("0.1.0".to_string()),
            to: to.to_string(),
            sequence: 7,
            key_id: "test-2026".to_string(),
            source: "/tmp/bundle.tar.gz".to_string(),
            started_at: "2026-10-26T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = State {
            current: Some("0.2.1".to_string()),
            previous: Some("0.2.0".to_string()),
            last_good: Some("0.2.1".to_string()),
            pending: Some(pending("0.3.0")),
            sequence: 42,
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), state);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn test_state_missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::load(&dir.path().join("absent.json")).unwrap();
        assert_eq!(state, State::default());
    }

    #[test]
    fn test_state_rejects_unsafe_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(
            &path,
            br#"{"current":"../../etc","previous":null,"last_good":null,"pending":null,"sequence":1}"#,
        )
        .unwrap();
        assert!(matches!(
            State::load(&path).unwrap_err(),
            UpdateError::State(_)
        ));
    }

    #[test]
    fn test_state_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            State::load(&path).unwrap_err(),
            UpdateError::State(_)
        ));
    }

    #[test]
    fn test_atomic_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        State::default().save(&path).unwrap();
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("state.json")]);
    }
}
