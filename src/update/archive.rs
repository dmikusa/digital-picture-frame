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

//! Release bundle format: manifest parsing and safe `.tar.gz` extraction.
//!
//! Extraction is allowlist-only: regular files and directories, relative paths,
//! no `..`, no links, no devices. Nothing is written until the signature and
//! checksum have already been verified by the caller.

use crate::update::checksum::{is_sha256_hex, sha256_file, verify_sha256};
use crate::update::{is_safe_version, Result, UpdateError};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use tar::Archive;

/// Bundle format understood by this build.
pub const BUNDLE_FORMAT: u32 = 1;
/// Largest bundle accepted from disk or over HTTPS (100 MiB).
pub const MAX_BUNDLE_BYTES: u64 = 100 * 1024 * 1024;
/// Largest total size of extracted files (512 MiB), a decompression guard.
pub const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;
/// Largest `manifest.json` accepted.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Most entries a bundle may contain.
pub const MAX_ENTRIES: usize = 10_000;

/// Binaries every app-only bundle must contain.
pub const REQUIRED_BINARIES: &[&str] = &[
    "bin/photo-frame-manager",
    "bin/photo-frame-display",
    "bin/photo-frame-update",
];

/// `manifest.json` inside a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Bundle format; only [`BUNDLE_FORMAT`] is accepted.
    pub format: u32,
    /// Version string; also the release directory name.
    pub version: String,
    /// Monotonic sequence, the anti-rollback floor.
    pub sequence: u64,
    /// Target architecture (`arm64` or `aarch64`).
    pub arch: String,
    /// Oldest installed version this bundle supports.
    #[serde(default)]
    pub min_version: Option<String>,
    /// Every installed file with its SHA-256.
    #[serde(default)]
    pub files: Vec<ManifestFile>,
}

/// One file recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Relative path inside the bundle.
    pub path: String,
    /// Lowercase hex SHA-256 of the file contents.
    pub sha256: String,
}

impl Manifest {
    /// Parse and structurally validate a manifest.
    pub fn from_slice(bytes: &[u8]) -> Result<Manifest> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(UpdateError::Verification(format!(
                "manifest is larger than {} bytes",
                MAX_MANIFEST_BYTES
            )));
        }
        let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| {
            UpdateError::Verification(format!("manifest.json is not valid JSON: {}", e))
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.format != BUNDLE_FORMAT {
            return Err(UpdateError::Unsupported(format!(
                "bundle format {} is not supported (expected {})",
                self.format, BUNDLE_FORMAT
            )));
        }
        if !is_safe_version(&self.version) {
            return Err(UpdateError::Unsupported(format!(
                "bundle version '{}' is not a safe release name",
                self.version
            )));
        }
        if let Some(min_version) = &self.min_version {
            if !is_safe_version(min_version) {
                return Err(UpdateError::Unsupported(format!(
                    "bundle min_version '{}' is not a valid version",
                    min_version
                )));
            }
        }
        for file in &self.files {
            sanitize_entry_path(Path::new(&file.path))?;
            if !is_sha256_hex(&file.sha256) {
                return Err(UpdateError::Verification(format!(
                    "manifest entry '{}' does not have a valid SHA-256",
                    file.path
                )));
            }
        }
        Ok(())
    }

    /// Reject bundles built for another architecture.
    pub fn require_arch(&self) -> Result<()> {
        match self.arch.as_str() {
            "arm64" | "aarch64" => Ok(()),
            other => Err(UpdateError::Unsupported(format!(
                "bundle targets architecture '{}', this updater requires arm64/aarch64",
                other
            ))),
        }
    }
}

/// Extract a `.tar.gz` bundle into `dest`, rejecting anything unsafe.
///
/// `dest` must not already exist; the caller creates a unique staging path.
pub fn extract_bundle(bundle: &Path, dest: &Path) -> Result<()> {
    let metadata = fs::metadata(bundle).map_err(|e| {
        UpdateError::Io(format!("failed to read bundle {}: {}", bundle.display(), e))
    })?;
    if !metadata.is_file() {
        return Err(UpdateError::Verification(format!(
            "bundle {} is not a regular file",
            bundle.display()
        )));
    }
    if metadata.len() > MAX_BUNDLE_BYTES {
        return Err(UpdateError::Unsupported(format!(
            "bundle is larger than {} bytes",
            MAX_BUNDLE_BYTES
        )));
    }

    fs::create_dir(dest).map_err(|e| {
        UpdateError::Io(format!(
            "failed to create staging directory {}: {}",
            dest.display(),
            e
        ))
    })?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o700)).map_err(|e| {
        UpdateError::Io(format!(
            "failed to set mode on staging directory {}: {}",
            dest.display(),
            e
        ))
    })?;

    let file = File::open(bundle)
        .map_err(|e| UpdateError::Io(format!("failed to open {}: {}", bundle.display(), e)))?;
    let mut archive = Archive::new(GzDecoder::new(file));
    let entries = archive
        .entries()
        .map_err(|e| UpdateError::Verification(format!("bundle is not a valid tar: {}", e)))?;

    let mut total: u64 = 0;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ENTRIES {
            return Err(UpdateError::Unsupported(format!(
                "bundle contains more than {} entries",
                MAX_ENTRIES
            )));
        }
        let mut entry = entry
            .map_err(|e| UpdateError::Verification(format!("failed to read tar entry: {}", e)))?;
        let entry_type = entry.header().entry_type();
        let raw_path = entry
            .path()
            .map_err(|e| UpdateError::Verification(format!("tar entry has no valid path: {}", e)))?
            .into_owned();
        let relative = sanitize_entry_path(&raw_path)?;

        match entry_type {
            tar::EntryType::Directory => {
                let target = dest.join(&relative);
                fs::create_dir_all(&target).map_err(|e| {
                    UpdateError::Io(format!("failed to create {}: {}", target.display(), e))
                })?;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).map_err(|e| {
                    UpdateError::Io(format!("failed to set mode on {}: {}", target.display(), e))
                })?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let size = entry.header().size().map_err(|e| {
                    UpdateError::Verification(format!(
                        "tar entry '{}' has an invalid size: {}",
                        relative.display(),
                        e
                    ))
                })?;
                total = total.checked_add(size).ok_or_else(|| {
                    UpdateError::Unsupported("bundle extracted size overflowed".to_string())
                })?;
                if total > MAX_EXTRACTED_BYTES {
                    return Err(UpdateError::Unsupported(format!(
                        "bundle expands to more than {} bytes",
                        MAX_EXTRACTED_BYTES
                    )));
                }

                let target = dest.join(&relative);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        UpdateError::Io(format!("failed to create {}: {}", parent.display(), e))
                    })?;
                }
                let mode = if relative.starts_with("bin") {
                    0o755
                } else {
                    0o644
                };
                let mut out = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&target)
                    .map_err(|e| {
                        UpdateError::Io(format!("failed to create {}: {}", target.display(), e))
                    })?;
                let written = io::copy(&mut entry, &mut out).map_err(|e| {
                    UpdateError::Io(format!("failed to extract {}: {}", target.display(), e))
                })?;
                if written != size {
                    return Err(UpdateError::Verification(format!(
                        "tar entry '{}' was truncated ({} of {} bytes)",
                        relative.display(),
                        written,
                        size
                    )));
                }
                out.set_permissions(fs::Permissions::from_mode(mode))
                    .map_err(|e| {
                        UpdateError::Io(format!(
                            "failed to set mode on {}: {}",
                            target.display(),
                            e
                        ))
                    })?;
                out.sync_all().map_err(|e| {
                    UpdateError::Io(format!("failed to flush {}: {}", target.display(), e))
                })?;
            }
            other => {
                return Err(UpdateError::Unsupported(format!(
                    "bundle entry '{}' has disallowed type {:?}; only regular files and directories are allowed",
                    relative.display(),
                    other
                )));
            }
        }
    }

    Ok(())
}

/// Read and validate `<root>/manifest.json`.
pub fn read_manifest(root: &Path) -> Result<Manifest> {
    let path = root.join("manifest.json");
    let metadata = fs::symlink_metadata(&path).map_err(|e| {
        UpdateError::Verification(format!("bundle is missing manifest.json: {}", e))
    })?;
    if !metadata.file_type().is_file() {
        return Err(UpdateError::Verification(
            "manifest.json is not a regular file".to_string(),
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::Verification(format!(
            "manifest.json is larger than {} bytes",
            MAX_MANIFEST_BYTES
        )));
    }
    let bytes = fs::read(&path)
        .map_err(|e| UpdateError::Io(format!("failed to read {}: {}", path.display(), e)))?;
    Manifest::from_slice(&bytes)
}

/// Verify every manifest hash and require the three app binaries.
pub fn verify_bundle_tree(root: &Path, manifest: &Manifest) -> Result<()> {
    let mut listed = HashSet::new();
    for file in &manifest.files {
        let relative = sanitize_entry_path(Path::new(&file.path))?;
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            UpdateError::Verification(format!(
                "manifest lists '{}' but it is not in the bundle: {}",
                file.path, e
            ))
        })?;
        if !metadata.file_type().is_file() {
            return Err(UpdateError::Verification(format!(
                "manifest entry '{}' is not a regular file",
                file.path
            )));
        }
        let actual = sha256_file(&path)?;
        verify_sha256(&file.sha256, &actual, &file.path)?;
        listed.insert(file.path.as_str());
    }

    for required in REQUIRED_BINARIES {
        if !listed.contains(required) {
            return Err(UpdateError::Verification(format!(
                "manifest does not list required binary '{}'",
                required
            )));
        }
        let path = root.join(required);
        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            UpdateError::Verification(format!(
                "bundle is missing required binary '{}': {}",
                required, e
            ))
        })?;
        if !metadata.file_type().is_file() {
            return Err(UpdateError::Verification(format!(
                "required binary '{}' is not a regular file",
                required
            )));
        }
    }
    Ok(())
}

/// Reject absolute paths, `..`, and empty paths.
fn sanitize_entry_path(path: &Path) -> Result<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(UpdateError::Unsupported(format!(
                    "bundle path '{}' must not contain '..'",
                    path.display()
                )))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(UpdateError::Unsupported(format!(
                    "bundle path '{}' must be relative",
                    path.display()
                )))
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(UpdateError::Unsupported(
            "bundle contains an empty path".to_string(),
        ));
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use tar::{Builder, EntryType, Header};

    fn regular_header(data: &[u8]) -> Header {
        let mut header = Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        header
    }

    /// A header whose name bytes are written directly, bypassing the tar
    /// crate's own `..`/absolute-path checks. This is how a hostile archive
    /// would look on disk.
    fn raw_header(name: &str, size: u64, entry_type: EntryType) -> Header {
        let mut header = Header::new_gnu();
        header.set_size(size);
        header.set_mode(0o644);
        header.set_entry_type(entry_type);
        let bytes = header.as_mut_bytes();
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        header
    }

    fn extract(names_entries: Vec<(Header, Option<&'static str>, Vec<u8>)>) -> Result<PathBuf> {
        let dir = tempfile::tempdir().unwrap();
        // Keep the temp dir alive by leaking it for the test process.
        let root = dir.keep();
        let bundle = root.join("bundle.tar.gz");
        {
            let file = File::create(&bundle).unwrap();
            let mut builder = Builder::new(GzEncoder::new(file, Compression::default()));
            for (mut header, name, data) in names_entries {
                match name {
                    Some(name) => builder
                        .append_data(&mut header, name, data.as_slice())
                        .unwrap(),
                    None => builder.append(&header, data.as_slice()).unwrap(),
                }
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dest = root.join("out");
        extract_bundle(&bundle, &dest)?;
        Ok(root)
    }

    #[test]
    fn test_extract_regular_files_and_dirs() {
        let root = extract(vec![
            (
                {
                    let mut h = Header::new_gnu();
                    h.set_size(0);
                    h.set_mode(0o755);
                    h.set_entry_type(EntryType::Directory);
                    h
                },
                Some("bin/"),
                Vec::new(),
            ),
            (
                regular_header(b"#!/bin/sh\n"),
                Some("bin/photo-frame-update"),
                b"#!/bin/sh\n".to_vec(),
            ),
            (regular_header(b"{}"), Some("manifest.json"), b"{}".to_vec()),
        ])
        .unwrap();

        let updater = root.join("out/bin/photo-frame-update");
        assert_eq!(fs::read(&updater).unwrap(), b"#!/bin/sh\n");
        let mode = fs::metadata(&updater).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert!(root.join("out/manifest.json").is_file());
    }

    #[test]
    fn test_extract_rejects_parent_traversal() {
        let err = extract(vec![(
            raw_header("../evil", 4, EntryType::Regular),
            None,
            b"evil".to_vec(),
        )])
        .unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_extract_rejects_absolute_path() {
        let err = extract(vec![(
            raw_header("/tmp/evil", 4, EntryType::Regular),
            None,
            b"evil".to_vec(),
        )])
        .unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_extract_rejects_symlink() {
        let mut header = Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(EntryType::Symlink);
        header.set_link_name("../outside").unwrap();
        header.set_cksum();
        let err = extract(vec![(header, Some("bin/link"), Vec::new())]).unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_extract_rejects_hardlink() {
        let mut header = Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Link);
        header.set_link_name("bin/photo-frame-manager").unwrap();
        header.set_cksum();
        let err = extract(vec![(header, Some("bin/hardlink"), Vec::new())]).unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_extract_rejects_device() {
        let err = extract(vec![(
            raw_header("bin/device", 0, EntryType::Char),
            None,
            Vec::new(),
        )])
        .unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_extract_rejects_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("bundle.tar.gz");
        let file = File::create(&bundle).unwrap();
        let mut builder = Builder::new(GzEncoder::new(file, Compression::default()));
        builder.finish().unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        assert!(extract_bundle(&bundle, &dest).is_err());
    }

    fn manifest(version: &str, files: Vec<ManifestFile>) -> Manifest {
        Manifest {
            format: BUNDLE_FORMAT,
            version: version.to_string(),
            sequence: 1,
            arch: "arm64".to_string(),
            min_version: None,
            files,
        }
    }

    #[test]
    fn test_manifest_validation() {
        let good = manifest("0.2.1", Vec::new());
        let bytes = serde_json::to_vec(&good).unwrap();
        assert!(Manifest::from_slice(&bytes).is_ok());

        let mut wrong_format = good.clone();
        wrong_format.format = 2;
        assert!(matches!(
            Manifest::from_slice(&serde_json::to_vec(&wrong_format).unwrap()).unwrap_err(),
            UpdateError::Unsupported(_)
        ));

        let mut unsafe_version = good.clone();
        unsafe_version.version = "../0.2.1".to_string();
        assert!(matches!(
            Manifest::from_slice(&serde_json::to_vec(&unsafe_version).unwrap()).unwrap_err(),
            UpdateError::Unsupported(_)
        ));

        let mut bad_path = good.clone();
        bad_path.files = vec![ManifestFile {
            path: "../evil".to_string(),
            sha256: "0".repeat(64),
        }];
        assert!(matches!(
            Manifest::from_slice(&serde_json::to_vec(&bad_path).unwrap()).unwrap_err(),
            UpdateError::Unsupported(_)
        ));

        let mut bad_hash = good;
        bad_hash.files = vec![ManifestFile {
            path: "bin/photo-frame-update".to_string(),
            sha256: "nope".to_string(),
        }];
        assert!(matches!(
            Manifest::from_slice(&serde_json::to_vec(&bad_hash).unwrap()).unwrap_err(),
            UpdateError::Verification(_)
        ));
    }

    #[test]
    fn test_manifest_arch() {
        let mut good = manifest("0.2.1", Vec::new());
        assert!(good.require_arch().is_ok());
        good.arch = "aarch64".to_string();
        assert!(good.require_arch().is_ok());
        good.arch = "x86_64".to_string();
        assert!(matches!(
            good.require_arch().unwrap_err(),
            UpdateError::Unsupported(_)
        ));
    }

    fn write_tree(dir: &Path) -> Manifest {
        fs::create_dir_all(dir.join("bin")).unwrap();
        let names = REQUIRED_BINARIES;
        let mut files = Vec::new();
        for name in names {
            let data = format!("#!/bin/sh\n# {}\n", name);
            fs::write(dir.join(name), &data).unwrap();
            files.push(ManifestFile {
                path: (*name).to_string(),
                sha256: crate::update::sha256_hex(data.as_bytes()),
            });
        }
        fs::write(dir.join("manifest.json"), b"{}").unwrap();
        manifest("0.2.1", files)
    }

    #[test]
    fn test_verify_bundle_tree_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = write_tree(dir.path());
        verify_bundle_tree(dir.path(), &manifest).unwrap();
    }

    #[test]
    fn test_verify_bundle_tree_detects_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = write_tree(dir.path());
        fs::write(dir.path().join("bin/photo-frame-update"), b"tampered").unwrap();
        let err = verify_bundle_tree(dir.path(), &manifest).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_verify_bundle_tree_requires_all_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = write_tree(dir.path());
        let mut missing = manifest.clone();
        missing
            .files
            .retain(|file| file.path != "bin/photo-frame-update");
        let err = verify_bundle_tree(dir.path(), &missing).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_verify_bundle_tree_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = write_tree(dir.path());
        let path = dir.path().join("bin/photo-frame-manager");
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("photo-frame-update", &path).unwrap();
        let err = verify_bundle_tree(dir.path(), &manifest).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_read_manifest_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_manifest(dir.path()).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_extract_rejects_oversized_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.tar.gz");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_BUNDLE_BYTES + 1).unwrap();
        let err = extract_bundle(&path, &dir.path().join("out")).unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_extract_garbage_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.tar.gz");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"not a tarball").unwrap();
        drop(file);
        let err = extract_bundle(&path, &dir.path().join("out")).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }
}
