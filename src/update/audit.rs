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

//! Persistent, append-only audit log for update attempts.
//!
//! This lives on real storage (not tmpfs) and is mode 0600. Fields are
//! whitespace-free so a line stays parseable; URLs are stripped of their query
//! and fragment so no token can end up in the log.

use crate::update::{Result, UpdateError};
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// One audit line.
#[derive(Debug, Clone, Default)]
pub struct AuditRecord<'a> {
    /// `install`, `rollback`, `guard`, ...
    pub action: &'a str,
    /// Release that was active before the operation.
    pub from: Option<&'a str>,
    /// Release the operation targeted.
    pub to: Option<&'a str>,
    /// Bundle sequence, when known.
    pub sequence: Option<u64>,
    /// Trusted key ID that signed the bundle, when known.
    pub key_id: Option<&'a str>,
    /// Local path or URL the bundle came from.
    pub source: Option<&'a str>,
    /// `success`, `failed`, `rolled_back`, `confirmed`, `cleared`, ...
    pub result: &'a str,
    /// Optional human-readable explanation. Never put secrets here.
    pub detail: Option<&'a str>,
}

/// Append one record, creating the parent directory and log as needed.
pub fn append_audit(path: &Path, record: &AuditRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            UpdateError::Io(format!("failed to create {}: {}", parent.display(), e))
        })?;
        // Best effort: /var/log/photo-frame is root-only in production. Tests
        // may reuse a temp dir, where this simply succeeds.
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }

    let mut line = format!(
        "{} action={} from={} to={} version={} sequence={} key_id={} source={} result={}",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        sanitize(record.action),
        record.from.map(sanitize).unwrap_or_else(|| "-".to_string()),
        record.to.map(sanitize).unwrap_or_else(|| "-".to_string()),
        record.to.map(sanitize).unwrap_or_else(|| "-".to_string()),
        record
            .sequence
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        record
            .key_id
            .map(sanitize)
            .unwrap_or_else(|| "-".to_string()),
        record
            .source
            .map(sanitize_source)
            .unwrap_or_else(|| "-".to_string()),
        sanitize(record.result),
    );
    if let Some(detail) = record.detail {
        line.push_str(" detail=");
        line.push_str(&sanitize(detail));
    }
    line.push('\n');

    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| UpdateError::Io(format!("failed to open {}: {}", path.display(), e)))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| UpdateError::Io(format!("failed to set mode on {}: {}", path.display(), e)))?;
    file.write_all(line.as_bytes())
        .map_err(|e| UpdateError::Io(format!("failed to append to {}: {}", path.display(), e)))?;
    file.sync_all()
        .map_err(|e| UpdateError::Io(format!("failed to flush {}: {}", path.display(), e)))?;
    Ok(())
}

/// Remove whitespace, control characters, and the URL query/fragment.
fn sanitize_source(value: &str) -> String {
    let without_query = value.split(['?', '#']).next().unwrap_or(value);
    sanitize(without_query)
}

/// Make a value safe to embed in a single whitespace-delimited log line.
fn sanitize(value: &str) -> String {
    if value.is_empty() {
        return "-".to_string();
    }
    value
        .chars()
        .map(|c| {
            if c.is_whitespace() || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_append_audit_writes_line_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("update.log");
        append_audit(
            &path,
            &AuditRecord {
                action: "install",
                from: Some("0.1.0"),
                to: Some("0.2.1"),
                sequence: Some(42),
                key_id: Some("test-2026"),
                source: Some("/tmp/bundle.tar.gz"),
                result: "success",
                detail: None,
            },
        )
        .unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("action=install"));
        assert!(text.contains("from=0.1.0"));
        assert!(text.contains("to=0.2.1"));
        assert!(text.contains("version=0.2.1"));
        assert!(text.contains("sequence=42"));
        assert!(text.contains("key_id=test-2026"));
        assert!(text.contains("result=success"));
        assert!(text.ends_with('\n'));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn test_append_audit_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.log");
        for result in ["failed", "success"] {
            append_audit(
                &path,
                &AuditRecord {
                    action: "install",
                    result,
                    ..AuditRecord::default()
                },
            )
            .unwrap();
        }
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("from=-"));
        assert!(text.contains("sequence=-"));
    }

    #[test]
    fn test_audit_source_strips_query_and_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.log");
        append_audit(
            &path,
            &AuditRecord {
                action: "install",
                result: "failed",
                source: Some("https://example.com/bundle.tar.gz?token=secret#frag"),
                detail: Some("bad signature\nline two"),
                ..AuditRecord::default()
            },
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("source=https://example.com/bundle.tar.gz "));
        assert!(!text.contains("secret"));
        assert_eq!(text.lines().count(), 1);
    }
}
