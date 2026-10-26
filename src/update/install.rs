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

//! Install, rollback, GC, and boot-guard operations.
//!
//! Every operation follows the same rule: verify first, then stage, then flip
//! the `current` symlink with a single `rename()`. If a health check fails the
//! symlink goes back to the previous release.

use crate::update::archive::{self, MAX_BUNDLE_BYTES};
use crate::update::audit::{append_audit, AuditRecord};
use crate::update::checksum::{
    normalize_sha256, parse_sidecar, read_small, sha256_file, verify_sha256,
};
use crate::update::health::{restart_services, HealthCheck, Services};
use crate::update::signature::{verify_signature, MAX_SIGNATURE_ENVELOPE_BYTES};
use crate::update::state::{sync_dir, Pending, State};
use crate::update::{compare_versions, is_safe_version, Paths, Result, TrustedKey, UpdateError};
use chrono::{SecondsFormat, Utc};
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::Url;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_SIDECAR_BYTES: u64 = 8 * 1024;
const MAX_REDIRECTS: usize = 5;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const GC_RECENT_KEEP: usize = 4;

/// Options for [`install`].
pub struct InstallOptions<'a> {
    /// Install root, state directory, and audit log.
    pub paths: &'a Paths,
    /// Local bundle path or `https://` URL.
    pub source: &'a str,
    /// Caller-supplied checksum; required for URLs, optional for local files.
    pub expected_sha256: Option<&'a str>,
    /// Trusted signing keys (the compile-time list in production).
    pub trusted_keys: &'a [TrustedKey],
    /// Service names to restart after activation.
    pub services: Services,
    /// Health check configuration.
    pub health: HealthCheck,
    /// Skip systemctl restarts (the health check still runs as configured).
    pub no_restart: bool,
}

impl<'a> InstallOptions<'a> {
    /// Test-friendly defaults: no systemctl, no health check.
    pub fn new(paths: &'a Paths, source: &'a str, trusted_keys: &'a [TrustedKey]) -> Self {
        Self {
            paths,
            source,
            expected_sha256: None,
            trusted_keys,
            services: Services::default(),
            health: HealthCheck::skip(),
            no_restart: true,
        }
    }
}

/// Result of a successful install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Newly active version.
    pub version: String,
    /// Sequence carried by the bundle.
    pub sequence: u64,
    /// Trusted key that signed the bundle.
    pub key_id: String,
    /// Sanitized source label.
    pub source: String,
    /// Release that was active before the install.
    pub previous: Option<String>,
}

/// Result of a garbage collection pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// Release versions that were removed.
    pub removed: Vec<String>,
    /// Release versions that remain.
    pub kept: Vec<String>,
}

/// One installed release, as shown by `list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Version (release directory name).
    pub version: String,
    /// Absolute path.
    pub path: PathBuf,
    /// Recursive size in bytes.
    pub size: u64,
    /// Whether this is the active release.
    pub current: bool,
    /// Whether this is the recorded fallback release.
    pub previous: bool,
}

/// Result of `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    /// Persisted state.
    pub state: State,
    /// Version the `current` symlink points at, if it exists.
    pub current_link: Option<String>,
    /// Whether `current` resolves to an existing release directory.
    pub current_link_valid: bool,
    /// Number of installed releases.
    pub release_count: usize,
}

/// Install a verified bundle.
pub fn install(options: &InstallOptions) -> Result<InstallReport> {
    let paths = options.paths;
    let mut state = State::load(&paths.state_file())?;

    let staging_root = create_staging(paths)?;
    let fetched = fetch_bundle(options, &staging_root)?;

    let extract_path = unique_path(&staging_root, "extract");
    let mut extract_guard = CleanupDir::new(extract_path.clone());
    archive::extract_bundle(&fetched.path, &extract_path)?;

    let manifest = archive::read_manifest(&extract_path)?;
    manifest.require_arch()?;
    archive::verify_bundle_tree(&extract_path, &manifest)?;

    // Policy checks come after integrity checks but before anything is
    // activated. A bad or old bundle must leave the device untouched.
    let release_dir = paths.release_dir(&manifest.version);
    if release_dir.exists() {
        return Err(UpdateError::Unsupported(format!(
            "release {} already exists in {}",
            manifest.version,
            paths.releases_dir().display()
        )));
    }
    if manifest.sequence < state.sequence {
        return Err(UpdateError::Unsupported(format!(
            "bundle sequence {} is lower than installed sequence {}; refusing downgrade",
            manifest.sequence, state.sequence
        )));
    }
    if let (Some(min_version), Some(current)) =
        (manifest.min_version.as_deref(), state.current.as_deref())
    {
        if compare_versions(current, min_version) == Ordering::Less {
            return Err(UpdateError::Unsupported(format!(
                "bundle requires at least version {}, but {} is installed",
                min_version, current
            )));
        }
    }

    let releases = paths.releases_dir();
    fs::create_dir_all(&releases)
        .map_err(|e| UpdateError::Io(format!("failed to create {}: {}", releases.display(), e)))?;
    let mut release_guard = CleanupDir::new(release_dir.clone());
    fs::rename(&extract_path, &release_dir).map_err(|e| {
        UpdateError::Io(format!(
            "failed to move staging release to {}: {}",
            release_dir.display(),
            e
        ))
    })?;
    extract_guard.disarm();
    sync_dir(&releases)?;

    // Record the in-flight install before flipping, so a power loss between
    // the flip and the health check is visible to the boot guard.
    let previous = state.current.clone();
    state.pending = Some(Pending {
        from: previous.clone(),
        to: manifest.version.clone(),
        sequence: manifest.sequence,
        key_id: fetched.key_id.clone(),
        source: fetched.source.clone(),
        started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    });
    state.save(&paths.state_file())?;
    // The release is now referenced by the pending marker; keep it even if a
    // later step fails so the guard has the full picture.
    release_guard.disarm();

    activate(paths, &manifest.version)?;

    // `--no-restart` only skips systemctl; an explicitly configured health
    // check still runs so tests (and dev builds) can exercise the revert path.
    let health = if options.no_restart {
        options.health.check()
    } else {
        restart_services(&options.services).and_then(|_| options.health.check())
    };

    match health {
        Ok(()) => {
            state.current = Some(manifest.version.clone());
            if let Some(previous) = &previous {
                if previous != &manifest.version {
                    state.previous = Some(previous.clone());
                }
            }
            state.last_good = Some(manifest.version.clone());
            state.sequence = state.sequence.max(manifest.sequence);
            state.pending = None;
            state.save(&paths.state_file())?;

            audit(
                paths,
                &AuditRecord {
                    action: "install",
                    from: previous.as_deref(),
                    to: Some(&manifest.version),
                    sequence: Some(manifest.sequence),
                    key_id: Some(&fetched.key_id),
                    source: Some(&fetched.source),
                    result: "success",
                    detail: None,
                },
            );
            // GC must never turn a good update into a failed one.
            if let Err(err) = gc(paths) {
                eprintln!("warning: failed to garbage-collect old releases: {}", err);
            }

            Ok(InstallReport {
                version: manifest.version,
                sequence: manifest.sequence,
                key_id: fetched.key_id.clone(),
                source: fetched.source.clone(),
                previous,
            })
        }
        Err(err) => {
            let failure = format!("update to {} failed: {}", manifest.version, err);
            if let Some(previous) = previous.as_deref() {
                if paths.release_dir(previous).is_dir() {
                    activate(paths, previous)?;
                    if !options.no_restart {
                        if let Err(restart_err) = restart_services(&options.services) {
                            eprintln!(
                                "warning: failed to restart services after revert: {}",
                                restart_err
                            );
                        }
                    }
                    state.current = Some(previous.to_string());
                    state.pending = None;
                    state.save(&paths.state_file())?;
                    audit(
                        paths,
                        &AuditRecord {
                            action: "install",
                            from: Some(previous),
                            to: Some(&manifest.version),
                            sequence: Some(manifest.sequence),
                            key_id: Some(&fetched.key_id),
                            source: Some(&fetched.source),
                            result: "rolled_back",
                            detail: Some(&failure),
                        },
                    );
                    return Err(UpdateError::Health(format!(
                        "{}; reverted to {}",
                        failure, previous
                    )));
                }
            }

            // No usable fallback: there was no previous release, so there is
            // no symlink target that can be trusted. Remove it and record the
            // failure rather than leaving a broken release active.
            if let Err(remove_err) = fs::remove_file(paths.current_link()) {
                if remove_err.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "warning: failed to remove {}: {}",
                        paths.current_link().display(),
                        remove_err
                    );
                }
            }
            state.current = None;
            state.pending = None;
            state.save(&paths.state_file())?;
            audit(
                paths,
                &AuditRecord {
                    action: "install",
                    from: None,
                    to: Some(&manifest.version),
                    sequence: Some(manifest.sequence),
                    key_id: Some(&fetched.key_id),
                    source: Some(&fetched.source),
                    result: "failed",
                    detail: Some(&failure),
                },
            );
            Err(UpdateError::Health(format!(
                "{}; no previous release to revert to",
                failure
            )))
        }
    }
}

/// Activate a release by atomically replacing the `current` symlink.
pub fn activate(paths: &Paths, version: &str) -> Result<()> {
    if !is_safe_version(version) {
        return Err(UpdateError::Unsupported(format!(
            "'{}' is not a valid release name",
            version
        )));
    }
    fs::create_dir_all(&paths.root).map_err(|e| {
        UpdateError::Io(format!("failed to create {}: {}", paths.root.display(), e))
    })?;

    let temp_link = paths.root.join(format!(".current-{}", nonce()));
    let _ = fs::remove_file(&temp_link);
    let target = Path::new("releases").join(version);
    symlink(&target, &temp_link).map_err(|e| {
        UpdateError::Io(format!(
            "failed to create temporary symlink {}: {}",
            temp_link.display(),
            e
        ))
    })?;

    if let Err(err) = fs::rename(&temp_link, paths.current_link()) {
        let _ = fs::remove_file(&temp_link);
        return Err(UpdateError::Io(format!(
            "failed to activate release {}: {}",
            version, err
        )));
    }
    sync_dir(&paths.root)?;
    Ok(())
}

/// Version selected by the `current` symlink.
pub fn current_version(paths: &Paths) -> Result<Option<String>> {
    match fs::read_link(paths.current_link()) {
        Ok(target) => Ok(target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(UpdateError::Io(format!(
            "failed to read {}: {}",
            paths.current_link().display(),
            err
        ))),
    }
}

/// Keep `current`, `previous`, and the four most recently installed releases.
pub fn gc(paths: &Paths) -> Result<GcReport> {
    let state = State::load(&paths.state_file())?;
    let mut releases = scan_releases(paths)?;
    releases.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| compare_versions(&right.0, &left.0))
    });

    let mut keep: HashSet<String> = HashSet::new();
    if let Some(current) = &state.current {
        keep.insert(current.clone());
    }
    if let Some(previous) = &state.previous {
        keep.insert(previous.clone());
    }
    for (version, _) in releases.iter().take(GC_RECENT_KEEP) {
        keep.insert(version.clone());
    }

    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for (version, _) in &releases {
        if keep.contains(version) {
            kept.push(version.clone());
        } else {
            let path = paths.release_dir(version);
            fs::remove_dir_all(&path).map_err(|e| {
                UpdateError::Io(format!("failed to remove {}: {}", path.display(), e))
            })?;
            removed.push(version.clone());
        }
    }
    sync_dir(&paths.releases_dir())?;
    Ok(GcReport { removed, kept })
}

/// Installed releases, newest first.
pub fn list_releases(paths: &Paths) -> Result<Vec<ReleaseInfo>> {
    let state = State::load(&paths.state_file())?;
    let current = match &state.current {
        Some(current) => Some(current.clone()),
        None => current_version(paths)?,
    };
    let mut releases = scan_releases(paths)?;
    releases.sort_by(|left, right| {
        compare_versions(&right.0, &left.0).then_with(|| right.0.cmp(&left.0))
    });

    let mut out = Vec::with_capacity(releases.len());
    for (version, _) in releases {
        let path = paths.release_dir(&version);
        let size = dir_size(&path)?;
        out.push(ReleaseInfo {
            current: current.as_deref() == Some(version.as_str()),
            previous: state.previous.as_deref() == Some(version.as_str()),
            version,
            path,
            size,
        });
    }
    Ok(out)
}

/// Report state plus the health of the `current` symlink.
pub fn status(paths: &Paths) -> Result<StatusReport> {
    let state = State::load(&paths.state_file())?;
    let current_link = current_version(paths)?;
    let current_link_valid = current_link
        .as_deref()
        .map(|version| paths.release_dir(version).is_dir())
        .unwrap_or(false);
    let release_count = scan_releases(paths)?.len();
    Ok(StatusReport {
        state,
        current_link,
        current_link_valid,
        release_count,
    })
}

/// The bundle as verified on local disk, plus provenance for audit.
struct FetchedBundle {
    path: PathBuf,
    /// Downloaded files are removed when the install finishes.
    temporary: Option<PathBuf>,
    key_id: String,
    source: String,
}

impl Drop for FetchedBundle {
    fn drop(&mut self) {
        if let Some(path) = &self.temporary {
            let _ = fs::remove_file(path);
        }
    }
}

fn fetch_bundle(options: &InstallOptions, staging: &Path) -> Result<FetchedBundle> {
    let source = options.source;
    let lower = source.to_ascii_lowercase();
    if lower.contains("://") {
        if !lower.starts_with("https://") {
            return Err(UpdateError::Unsupported(format!(
                "refusing to install from '{}': only https:// URLs are accepted",
                display_source(source)
            )));
        }
        fetch_remote(
            source,
            options.expected_sha256,
            options.trusted_keys,
            staging,
        )
    } else {
        fetch_local(
            source,
            options.expected_sha256,
            options.trusted_keys,
            staging,
        )
    }
}

fn fetch_local(
    source: &str,
    expected_sha256: Option<&str>,
    trusted_keys: &[TrustedKey],
    staging: &Path,
) -> Result<FetchedBundle> {
    let path = PathBuf::from(source);
    let metadata = fs::metadata(&path)
        .map_err(|e| UpdateError::Io(format!("failed to read bundle {}: {}", path.display(), e)))?;
    if !metadata.is_file() {
        return Err(UpdateError::Verification(format!(
            "bundle {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_BUNDLE_BYTES {
        return Err(UpdateError::Unsupported(format!(
            "bundle {} is larger than {} bytes",
            path.display(),
            MAX_BUNDLE_BYTES
        )));
    }

    let sidecar_path = path_with_suffix(&path, ".sha256");
    let signature_path = path_with_suffix(&path, ".sha256.sig");
    let sidecar_bytes = read_small(&sidecar_path, MAX_SIDECAR_BYTES, "checksum sidecar")?;
    let signature_bytes = read_small(
        &signature_path,
        MAX_SIGNATURE_ENVELOPE_BYTES,
        "signature envelope",
    )?;
    let verified = verify_signature(trusted_keys, &sidecar_bytes, &signature_bytes)?;
    let sidecar = parse_sidecar(&sidecar_bytes)?;

    // Copy into root-only staging and verify *that* copy. Hashing the original
    // and extracting it later would leave a TOCTOU window where the file could
    // be swapped after verification.
    let staged = unique_path(staging, "local");
    copy_limited(&path, &staged, MAX_BUNDLE_BYTES)?;
    let actual = sha256_file(&staged)?;
    if let Err(err) =
        verify_sha256(&sidecar.sha256, &actual, &path.display().to_string()).and_then(|()| {
            match expected_sha256 {
                Some(expected) => {
                    let expected = normalize_sha256(expected)?;
                    verify_sha256(&expected, &actual, "caller-supplied --sha256")
                }
                None => Ok(()),
            }
        })
    {
        let _ = fs::remove_file(&staged);
        return Err(err);
    }

    Ok(FetchedBundle {
        path: staged.clone(),
        temporary: Some(staged),
        key_id: verified.key_id,
        source: display_source(source),
    })
}

fn fetch_remote(
    source: &str,
    expected_sha256: Option<&str>,
    trusted_keys: &[TrustedKey],
    staging: &Path,
) -> Result<FetchedBundle> {
    let expected = expected_sha256.ok_or_else(|| {
        UpdateError::Usage("--sha256 is required when installing from an HTTPS URL".to_string())
    })?;
    let expected = normalize_sha256(expected)?;

    let url = Url::parse(source)
        .map_err(|e| UpdateError::Usage(format!("invalid bundle URL '{}': {}", source, e)))?;
    if url.scheme() != "https" {
        return Err(UpdateError::Unsupported(format!(
            "refusing to install from '{}': only https:// URLs are accepted",
            source
        )));
    }

    let client = build_client()?;
    let sidecar_bytes = fetch_small(
        &client,
        &url_with_suffix(&url, ".sha256"),
        MAX_SIDECAR_BYTES,
        "checksum sidecar",
    )?;
    let signature_bytes = fetch_small(
        &client,
        &url_with_suffix(&url, ".sha256.sig"),
        MAX_SIGNATURE_ENVELOPE_BYTES,
        "signature envelope",
    )?;
    let verified = verify_signature(trusted_keys, &sidecar_bytes, &signature_bytes)?;
    let sidecar = parse_sidecar(&sidecar_bytes)?;

    let path = unique_path(staging, "download");
    download(&client, &url, &path)?;
    let actual = sha256_file(&path)?;
    verify_sha256(&sidecar.sha256, &actual, &display_source(source))?;
    verify_sha256(&expected, &actual, "caller-supplied --sha256")?;

    Ok(FetchedBundle {
        path: path.clone(),
        temporary: Some(path),
        key_id: verified.key_id,
        source: display_source(source),
    })
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .redirect(Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("refusing to follow a redirect to a non-HTTPS URL")
            } else if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| UpdateError::Network(format!("failed to build HTTP client: {}", e)))
}

fn copy_limited(source: &Path, dest: &Path, max: u64) -> Result<()> {
    let result = (|| -> Result<()> {
        let mut input = File::open(source)
            .map_err(|e| UpdateError::Io(format!("failed to open {}: {}", source.display(), e)))?;
        let mut output = File::create(dest)
            .map_err(|e| UpdateError::Io(format!("failed to create {}: {}", dest.display(), e)))?;
        let mut total: u64 = 0;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = input.read(&mut buffer).map_err(|e| {
                UpdateError::Io(format!("failed to read {}: {}", source.display(), e))
            })?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > max {
                return Err(UpdateError::Unsupported(format!(
                    "bundle {} is larger than {} bytes",
                    source.display(),
                    max
                )));
            }
            output.write_all(&buffer[..read]).map_err(|e| {
                UpdateError::Io(format!("failed to write {}: {}", dest.display(), e))
            })?;
        }
        output
            .sync_all()
            .map_err(|e| UpdateError::Io(format!("failed to flush {}: {}", dest.display(), e)))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn download(client: &Client, url: &Url, dest: &Path) -> Result<()> {
    let mut response = client
        .get(url.clone())
        .send()
        .map_err(|e| UpdateError::Network(format!("failed to download {}: {}", url, e)))?;
    if !response.status().is_success() {
        return Err(UpdateError::Network(format!(
            "failed to download {}: HTTP {}",
            url,
            response.status()
        )));
    }
    if let Some(length) = response.content_length() {
        if length > MAX_BUNDLE_BYTES {
            return Err(UpdateError::Unsupported(format!(
                "bundle at {} is larger than {} bytes",
                url, MAX_BUNDLE_BYTES
            )));
        }
    }

    let mut file = File::create(dest)
        .map_err(|e| UpdateError::Io(format!("failed to create {}: {}", dest.display(), e)))?;
    let mut total: u64 = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|e| UpdateError::Network(format!("failed to download {}: {}", url, e)))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_BUNDLE_BYTES {
            let _ = fs::remove_file(dest);
            return Err(UpdateError::Unsupported(format!(
                "bundle at {} is larger than {} bytes",
                url, MAX_BUNDLE_BYTES
            )));
        }
        file.write_all(&buffer[..read])
            .map_err(|e| UpdateError::Io(format!("failed to write {}: {}", dest.display(), e)))?;
    }
    file.sync_all()
        .map_err(|e| UpdateError::Io(format!("failed to flush {}: {}", dest.display(), e)))?;
    Ok(())
}

fn fetch_small(client: &Client, url: &Url, max: u64, what: &str) -> Result<Vec<u8>> {
    let mut response = client
        .get(url.clone())
        .send()
        .map_err(|e| UpdateError::Network(format!("failed to fetch {} {}: {}", what, url, e)))?;
    if !response.status().is_success() {
        return Err(UpdateError::Network(format!(
            "failed to fetch {} {}: HTTP {}",
            what,
            url,
            response.status()
        )));
    }
    if let Some(length) = response.content_length() {
        if length > max {
            return Err(UpdateError::Verification(format!(
                "{} at {} is larger than {} bytes",
                what, url, max
            )));
        }
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read = response.read(&mut buffer).map_err(|e| {
            UpdateError::Network(format!("failed to fetch {} {}: {}", what, url, e))
        })?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() as u64 > max {
            return Err(UpdateError::Verification(format!(
                "{} at {} is larger than {} bytes",
                what, url, max
            )));
        }
    }
    Ok(bytes)
}

fn create_staging(paths: &Paths) -> Result<PathBuf> {
    let staging = paths.staging_dir();
    fs::create_dir_all(&staging)
        .map_err(|e| UpdateError::Io(format!("failed to create {}: {}", staging.display(), e)))?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).map_err(|e| {
        UpdateError::Io(format!(
            "failed to set mode on {}: {}",
            staging.display(),
            e
        ))
    })?;
    Ok(staging)
}

fn scan_releases(paths: &Paths) -> Result<Vec<(String, SystemTime)>> {
    let dir = paths.releases_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(UpdateError::Io(format!(
                "failed to read {}: {}",
                dir.display(),
                err
            )))
        }
    };

    let mut releases = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|e| UpdateError::Io(format!("failed to read {}: {}", dir.display(), e)))?;
        let file_type = entry.file_type().map_err(|e| {
            UpdateError::Io(format!(
                "failed to inspect {}: {}",
                entry.path().display(),
                e
            ))
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_safe_version(&name) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        releases.push((name, modified));
    }
    Ok(releases)
}

fn dir_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| UpdateError::Io(format!("failed to inspect {}: {}", path.display(), e)))?;
    if metadata.file_type().is_file() {
        return Ok(metadata.len());
    }
    if !metadata.file_type().is_dir() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in fs::read_dir(path)
        .map_err(|e| UpdateError::Io(format!("failed to read {}: {}", path.display(), e)))?
    {
        let entry = entry
            .map_err(|e| UpdateError::Io(format!("failed to read {}: {}", path.display(), e)))?;
        total += dir_size(&entry.path())?;
    }
    Ok(total)
}

fn audit(paths: &Paths, record: &AuditRecord) {
    if let Err(err) = append_audit(&paths.log_path, record) {
        eprintln!("warning: failed to append to audit log: {}", err);
    }
}

/// A staging directory that is removed on drop unless the caller disarms it.
struct CleanupDir {
    path: PathBuf,
    armed: bool,
}

impl CleanupDir {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CleanupDir {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn url_with_suffix(url: &Url, suffix: &str) -> Url {
    let mut next = url.clone();
    next.set_path(&format!("{}{}", url.path(), suffix));
    next
}

fn unique_path(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!("{}-{}", label, nonce()))
}

fn nonce() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )
}

/// Strip URL query/fragment so a token cannot reach the audit log or state.
fn display_source(source: &str) -> String {
    source
        .split(['?', '#'])
        .next()
        .unwrap_or(source)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::archive::{Manifest, ManifestFile, REQUIRED_BINARIES};
    use crate::update::checksum::sha256_hex;
    use crate::update::keys::TrustedKey;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::os::unix::fs::PermissionsExt;
    use tar::{Builder, Header};

    struct TestKey {
        signing: SigningKey,
        trusted: TrustedKey,
    }

    fn test_key(id: &'static str, seed: u8) -> TestKey {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        TestKey {
            trusted: TrustedKey {
                id,
                bytes: signing.verifying_key().to_bytes(),
            },
            signing,
        }
    }

    fn keys_of(key: &TestKey) -> &[TrustedKey] {
        std::slice::from_ref(&key.trusted)
    }

    struct TestBundle {
        path: PathBuf,
    }

    fn write_bundle(
        dir: &Path,
        key: &TestKey,
        version: &str,
        sequence: u64,
        min_version: Option<&str>,
    ) -> TestBundle {
        let files: Vec<(String, Vec<u8>)> = REQUIRED_BINARIES
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    format!("#!/bin/sh\n# {} {}\n", name, version).into_bytes(),
                )
            })
            .collect();

        let manifest = Manifest {
            format: 1,
            version: version.to_string(),
            sequence,
            arch: "arm64".to_string(),
            min_version: min_version.map(|value| value.to_string()),
            files: files
                .iter()
                .map(|(path, data)| ManifestFile {
                    path: path.clone(),
                    sha256: sha256_hex(data),
                })
                .collect(),
        };

        let bundle_path = dir.join(format!("photo-frame-{}.tar.gz", version));
        {
            let file = File::create(&bundle_path).unwrap();
            let mut builder = Builder::new(GzEncoder::new(file, Compression::default()));
            let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
            let mut header = Header::new_gnu();
            header.set_size(manifest_bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, "manifest.json", manifest_bytes.as_slice())
                .unwrap();
            for (path, data) in &files {
                let mut header = Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, data.as_slice())
                    .unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }

        let bundle_hash = sha256_file(&bundle_path).unwrap();
        let filename = bundle_path.file_name().unwrap().to_string_lossy();
        let sidecar = format!("{}  {}\n", bundle_hash, filename);
        fs::write(path_with_suffix(&bundle_path, ".sha256"), &sidecar).unwrap();

        let signature = key.signing.sign(sidecar.as_bytes());
        let envelope = serde_json::to_vec(&serde_json::json!({
            "key_id": key.trusted.id,
            "signature": URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        }))
        .unwrap();
        fs::write(path_with_suffix(&bundle_path, ".sha256.sig"), envelope).unwrap();

        TestBundle { path: bundle_path }
    }

    struct TestEnv {
        _dir: tempfile::TempDir,
        paths: Paths,
    }

    fn test_env() -> TestEnv {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("opt");
        let state = dir.path().join("state");
        let log = dir.path().join("log/update.log");
        TestEnv {
            paths: Paths::new(root, state).with_log_path(log),
            _dir: dir,
        }
    }

    fn install_bundle(
        env: &TestEnv,
        key: &TestKey,
        bundle: &TestBundle,
        health: HealthCheck,
    ) -> Result<InstallReport> {
        let mut options =
            InstallOptions::new(&env.paths, bundle.path.to_str().unwrap(), keys_of(key));
        options.health = health;
        install(&options)
    }

    fn audit_text(env: &TestEnv) -> String {
        fs::read_to_string(&env.paths.log_path).unwrap_or_default()
    }

    #[test]
    fn test_local_install_flips_symlink_and_writes_state() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &key, "0.2.1", 42, Some("0.1.0"));

        let report = install_bundle(&env, &key, &bundle, HealthCheck::skip()).unwrap();
        assert_eq!(report.version, "0.2.1");
        assert_eq!(report.sequence, 42);
        assert_eq!(report.key_id, "test-2026");

        let target = fs::read_link(env.paths.current_link()).unwrap();
        assert_eq!(target, PathBuf::from("releases/0.2.1"));
        assert!(env
            .paths
            .release_dir("0.2.1")
            .join("bin/photo-frame-manager")
            .is_file());
        let mode = fs::metadata(
            env.paths
                .release_dir("0.2.1")
                .join("bin/photo-frame-update"),
        )
        .unwrap()
        .permissions()
        .mode()
            & 0o777;
        assert_eq!(mode, 0o755);

        let state = State::load(&env.paths.state_file()).unwrap();
        assert_eq!(state.current.as_deref(), Some("0.2.1"));
        assert_eq!(state.previous, None);
        assert_eq!(state.last_good.as_deref(), Some("0.2.1"));
        assert_eq!(state.sequence, 42);
        assert_eq!(state.pending, None);

        let audit = audit_text(&env);
        assert!(audit.contains("action=install"), "{audit}");
        assert!(audit.contains("result=success"), "{audit}");
        assert!(audit.contains("key_id=test-2026"), "{audit}");

        // Downloads and extraction scratch space are cleaned up.
        assert_eq!(fs::read_dir(env.paths.staging_dir()).unwrap().count(), 0);
    }

    #[test]
    fn test_install_records_previous_release() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let first = write_bundle(env._dir.path(), &key, "0.1.0", 1, None);
        let second = write_bundle(env._dir.path(), &key, "0.2.1", 2, Some("0.1.0"));

        install_bundle(&env, &key, &first, HealthCheck::skip()).unwrap();
        install_bundle(&env, &key, &second, HealthCheck::skip()).unwrap();

        let state = State::load(&env.paths.state_file()).unwrap();
        assert_eq!(state.current.as_deref(), Some("0.2.1"));
        assert_eq!(state.previous.as_deref(), Some("0.1.0"));
        assert_eq!(state.sequence, 2);
        assert_eq!(
            fs::read_link(env.paths.current_link()).unwrap(),
            PathBuf::from("releases/0.2.1")
        );
    }

    #[test]
    fn test_install_rejects_sequence_downgrade() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let newer = write_bundle(env._dir.path(), &key, "0.2.1", 10, None);
        let older = write_bundle(env._dir.path(), &key, "0.1.0", 9, None);

        install_bundle(&env, &key, &newer, HealthCheck::skip()).unwrap();
        let err = install_bundle(&env, &key, &older, HealthCheck::skip()).unwrap_err();
        match err {
            UpdateError::Unsupported(message) => assert!(message.contains("refusing downgrade")),
            other => panic!("unexpected error: {other:?}"),
        }

        let state = State::load(&env.paths.state_file()).unwrap();
        assert_eq!(state.current.as_deref(), Some("0.2.1"));
        assert_eq!(state.sequence, 10);
        assert!(!env.paths.release_dir("0.1.0").exists());
    }

    #[test]
    fn test_install_rejects_min_version() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let old = write_bundle(env._dir.path(), &key, "0.1.0", 1, None);
        let new = write_bundle(env._dir.path(), &key, "0.2.1", 2, Some("0.5.0"));

        install_bundle(&env, &key, &old, HealthCheck::skip()).unwrap();
        let err = install_bundle(&env, &key, &new, HealthCheck::skip()).unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_install_rejects_bad_signature() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &key, "0.2.1", 1, None);

        // Tamper with the bundle after the sidecar was signed.
        let bytes = fs::read(&bundle.path).unwrap();
        let mut tampered = bytes.clone();
        tampered.extend_from_slice(b"junk");
        fs::write(&bundle.path, tampered).unwrap();

        let err = install_bundle(&env, &key, &bundle, HealthCheck::skip()).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
        assert!(!env.paths.current_link().exists());
        assert!(!env.paths.release_dir("0.2.1").exists());
    }

    #[test]
    fn test_install_rejects_unknown_key() {
        let env = test_env();
        let signing_key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &signing_key, "0.2.1", 1, None);
        let other_key = test_key("other-2026", 9);

        let err = install_bundle(&env, &other_key, &bundle, HealthCheck::skip()).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_install_rejects_caller_sha256_mismatch() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &key, "0.2.1", 1, None);
        let mut options =
            InstallOptions::new(&env.paths, bundle.path.to_str().unwrap(), keys_of(&key));
        let wrong = "0".repeat(64);
        options.expected_sha256 = Some(&wrong);
        let err = install(&options).unwrap_err();
        match err {
            UpdateError::Verification(message) => assert!(message.contains("--sha256")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_install_rejects_non_https_source() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let mut options = InstallOptions::new(
            &env.paths,
            "http://example.com/bundle.tar.gz",
            keys_of(&key),
        );
        let hash = "0".repeat(64);
        options.expected_sha256 = Some(&hash);
        let err = install(&options).unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_install_url_requires_sha256() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let options = InstallOptions::new(
            &env.paths,
            "https://example.com/bundle.tar.gz",
            keys_of(&key),
        );
        let err = install(&options).unwrap_err();
        assert!(matches!(err, UpdateError::Usage(_)), "{err:?}");
    }

    #[test]
    fn test_install_fails_closed_without_trusted_keys() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &key, "0.2.1", 1, None);
        let mut options = InstallOptions::new(&env.paths, bundle.path.to_str().unwrap(), &[]);
        options.health = HealthCheck::skip();
        let err = install(&options).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_install_reverts_when_unhealthy() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let first = write_bundle(env._dir.path(), &key, "0.1.0", 1, None);
        let second = write_bundle(env._dir.path(), &key, "0.2.1", 2, Some("0.1.0"));

        install_bundle(&env, &key, &first, HealthCheck::skip()).unwrap();
        let err = install_bundle(&env, &key, &second, HealthCheck::forced(false)).unwrap_err();
        assert!(matches!(err, UpdateError::Health(_)), "{err:?}");

        assert_eq!(
            fs::read_link(env.paths.current_link()).unwrap(),
            PathBuf::from("releases/0.1.0")
        );
        let state = State::load(&env.paths.state_file()).unwrap();
        assert_eq!(state.current.as_deref(), Some("0.1.0"));
        assert_eq!(state.previous, None);
        assert_eq!(state.sequence, 1);
        assert_eq!(state.pending, None);
        assert!(audit_text(&env).contains("result=rolled_back"));
    }

    #[test]
    fn test_install_without_previous_clears_current_on_failure() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &key, "0.2.1", 1, None);
        let err = install_bundle(&env, &key, &bundle, HealthCheck::forced(false)).unwrap_err();
        assert!(matches!(err, UpdateError::Health(_)), "{err:?}");
        assert!(!env.paths.current_link().exists());
        let state = State::load(&env.paths.state_file()).unwrap();
        assert_eq!(state.current, None);
        assert_eq!(state.pending, None);
        assert!(audit_text(&env).contains("result=failed"));
    }

    #[test]
    fn test_install_existing_version_is_rejected() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let bundle = write_bundle(env._dir.path(), &key, "0.2.1", 1, None);
        install_bundle(&env, &key, &bundle, HealthCheck::skip()).unwrap();
        let err = install_bundle(&env, &key, &bundle, HealthCheck::skip()).unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    fn seed_release(env: &TestEnv, version: &str) {
        let dir = env.paths.release_dir(version);
        fs::create_dir_all(dir.join("bin")).unwrap();
        for name in REQUIRED_BINARIES {
            fs::write(dir.join(*name), b"stub").unwrap();
        }
    }

    #[test]
    fn test_gc_keeps_current_previous_and_four_recent() {
        let env = test_env();
        for index in 1..=8 {
            seed_release(&env, &format!("0.0.{}", index));
            std::thread::sleep(Duration::from_millis(10));
        }
        let state = State {
            current: Some("0.0.1".to_string()),
            previous: Some("0.0.2".to_string()),
            last_good: Some("0.0.1".to_string()),
            pending: None,
            sequence: 8,
        };
        state.save(&env.paths.state_file()).unwrap();

        let report = gc(&env.paths).unwrap();
        let mut removed = report.removed.clone();
        removed.sort();
        assert_eq!(removed, vec!["0.0.3".to_string(), "0.0.4".to_string()]);
        for version in ["0.0.1", "0.0.2", "0.0.5", "0.0.6", "0.0.7", "0.0.8"] {
            assert!(
                env.paths.release_dir(version).is_dir(),
                "{} missing",
                version
            );
        }
        for version in ["0.0.3", "0.0.4"] {
            assert!(!env.paths.release_dir(version).exists(), "{} kept", version);
        }
    }

    #[test]
    fn test_list_and_status() {
        let env = test_env();
        let key = test_key("test-2026", 7);
        let first = write_bundle(env._dir.path(), &key, "0.1.0", 1, None);
        let second = write_bundle(env._dir.path(), &key, "0.2.1", 2, Some("0.1.0"));
        install_bundle(&env, &key, &first, HealthCheck::skip()).unwrap();
        install_bundle(&env, &key, &second, HealthCheck::skip()).unwrap();

        let releases = list_releases(&env.paths).unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].version, "0.2.1");
        assert!(releases[0].current);
        assert!(releases[1].previous);
        assert!(releases[0].size > 0);

        let report = status(&env.paths).unwrap();
        assert_eq!(report.state.current.as_deref(), Some("0.2.1"));
        assert_eq!(report.current_link.as_deref(), Some("0.2.1"));
        assert!(report.current_link_valid);
        assert_eq!(report.release_count, 2);
    }

    #[test]
    fn test_activate_rejects_unsafe_version() {
        let env = test_env();
        let err = activate(&env.paths, "../etc").unwrap_err();
        assert!(matches!(err, UpdateError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn test_display_source_strips_query() {
        assert_eq!(
            display_source("https://example.com/a.tar.gz?token=secret#x"),
            "https://example.com/a.tar.gz"
        );
        assert_eq!(display_source("/tmp/bundle.tar.gz"), "/tmp/bundle.tar.gz");
    }
}
