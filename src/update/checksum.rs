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

//! SHA-256 helpers and `sha256sum` sidecar parsing.

use crate::update::{Result, UpdateError};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Length of a lowercase hex SHA-256 digest.
pub const SHA256_HEX_LEN: usize = 64;

const READ_BUFFER: usize = 64 * 1024;

/// A parsed `<bundle>.sha256` sidecar: `sha256sum` format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    /// Lowercase hex digest.
    pub sha256: String,
    /// Filename recorded by `sha256sum` (not used to locate the bundle).
    pub filename: String,
}

/// Hash a byte slice and return lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    to_hex(&hasher.finalize())
}

/// Hash a file, streaming so a large bundle never sits in memory.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|e| UpdateError::Io(format!("failed to open {}: {}", path.display(), e)))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| UpdateError::Io(format!("failed to read {}: {}", path.display(), e)))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// True when `value` looks like a SHA-256 hex digest.
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == SHA256_HEX_LEN && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Validate and normalize a caller-provided digest.
pub fn normalize_sha256(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if !is_sha256_hex(trimmed) {
        return Err(UpdateError::Usage(format!(
            "'{}' is not a 64-character SHA-256 hex digest",
            value
        )));
    }
    Ok(trimmed.to_ascii_lowercase())
}

/// Parse the exact bytes of a `.sha256` file.
///
/// Exactly one non-empty `sha256sum` line is accepted. The optional leading
/// `*` printed by `sha256sum -b` is stripped from the filename.
pub fn parse_sidecar(bytes: &[u8]) -> Result<Sidecar> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        UpdateError::Verification("checksum sidecar is not valid UTF-8".to_string())
    })?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let line = lines
        .next()
        .ok_or_else(|| UpdateError::Verification("checksum sidecar is empty".to_string()))?;
    if lines.next().is_some() {
        return Err(UpdateError::Verification(
            "checksum sidecar must contain exactly one line".to_string(),
        ));
    }

    let mut fields = line.split_whitespace();
    let digest = fields
        .next()
        .ok_or_else(|| UpdateError::Verification("checksum sidecar is empty".to_string()))?;
    let filename = fields.next().ok_or_else(|| {
        UpdateError::Verification("checksum sidecar is missing the filename column".to_string())
    })?;
    if fields.next().is_some() {
        return Err(UpdateError::Verification(
            "checksum sidecar has too many columns".to_string(),
        ));
    }
    if !is_sha256_hex(digest) {
        return Err(UpdateError::Verification(format!(
            "checksum sidecar digest '{}' is not a SHA-256 hex digest",
            digest
        )));
    }

    let filename = filename.strip_prefix('*').unwrap_or(filename);
    if filename.is_empty() {
        return Err(UpdateError::Verification(
            "checksum sidecar filename is empty".to_string(),
        ));
    }

    Ok(Sidecar {
        sha256: digest.to_ascii_lowercase(),
        filename: filename.to_string(),
    })
}

/// Compare two digests, reporting a verification error on mismatch.
pub fn verify_sha256(expected: &str, actual: &str, source: &str) -> Result<()> {
    if expected.eq_ignore_ascii_case(actual) {
        Ok(())
    } else {
        Err(UpdateError::Verification(format!(
            "SHA-256 mismatch for {}: expected {}, got {}",
            source, expected, actual
        )))
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Read a small sidecar/envelope file with a size cap.
pub(crate) fn read_small(path: &Path, max_bytes: u64, what: &str) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        UpdateError::Io(format!("failed to read {} {}: {}", what, path.display(), e))
    })?;
    if !metadata.is_file() {
        return Err(UpdateError::Verification(format!(
            "{} {} is not a regular file",
            what,
            path.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(UpdateError::Verification(format!(
            "{} {} is larger than {} bytes",
            what,
            path.display(),
            max_bytes
        )));
    }
    std::fs::read(path)
        .map_err(|e| UpdateError::Io(format!("failed to read {} {}: {}", what, path.display(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const DIGEST: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn test_sha256_hex_known_value() {
        assert_eq!(
            sha256_hex(b"test"),
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn test_parse_sidecar() {
        let sidecar = parse_sidecar(format!("{}  photo-frame-0.2.1.tar.gz\n", DIGEST).as_bytes())
            .expect("sidecar should parse");
        assert_eq!(sidecar.sha256, DIGEST);
        assert_eq!(sidecar.filename, "photo-frame-0.2.1.tar.gz");

        let binary_mode = parse_sidecar(format!("{} *bundle.tar.gz", DIGEST).as_bytes()).unwrap();
        assert_eq!(binary_mode.filename, "bundle.tar.gz");
    }

    #[test]
    fn test_parse_sidecar_rejects_malformed() {
        assert!(parse_sidecar(b"").is_err());
        assert!(parse_sidecar(b"not-a-hash  file.tar.gz\n").is_err());
        assert!(parse_sidecar(DIGEST.as_bytes()).is_err());
        assert!(parse_sidecar(format!("{}  a\n{}  b\n", DIGEST, DIGEST).as_bytes()).is_err());
        assert!(parse_sidecar(format!("{}  a  extra\n", DIGEST).as_bytes()).is_err());
        assert!(parse_sidecar(b"\xff\xfe").is_err());
    }

    #[test]
    fn test_verify_sha256_detects_mismatch() {
        assert!(verify_sha256(DIGEST, &DIGEST.to_uppercase(), "bundle").is_ok());
        let err = verify_sha256(DIGEST, &"0".repeat(64), "bundle").unwrap_err();
        match err {
            UpdateError::Verification(message) => assert!(message.contains("SHA-256 mismatch")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_sha256_file_matches_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"test").unwrap();
        file.sync_all().unwrap();
        assert_eq!(sha256_file(&path).unwrap(), DIGEST);
    }

    #[test]
    fn test_normalize_sha256() {
        assert_eq!(normalize_sha256(&DIGEST.to_uppercase()).unwrap(), DIGEST);
        assert!(normalize_sha256("abc").is_err());
        assert!(normalize_sha256(&format!("  {}  ", DIGEST)).is_ok());
    }

    #[test]
    fn test_is_sha256_hex() {
        assert!(is_sha256_hex(DIGEST));
        assert!(!is_sha256_hex(&DIGEST[..63]));
        assert!(!is_sha256_hex(&format!("{}g", &DIGEST[..63])));
    }
}
