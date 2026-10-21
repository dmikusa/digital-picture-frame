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

use crate::config::Config;
use crate::import::usb::{import_single_photo, is_enospc, rotate_for_space};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// A single remote photo discovered during a sync check.
pub struct RemotePhoto {
    pub remote_id: String,
    pub filename: String,
    pub size_bytes: Option<u64>,
}

/// Implemented by each remote photo source (Dropbox, Google Drive, etc.).
pub trait RemotePhotoSource: Send {
    fn connect(config: &HashMap<String, String>) -> io::Result<Self>
    where
        Self: Sized;

    /// List photos changed since the given cursor.
    /// Returns the photos and a new cursor for the next call.
    fn list_changes(&mut self, cursor: Option<&str>) -> io::Result<(Vec<RemotePhoto>, String)>;

    /// Download a remote photo to the given local path.
    fn download(&mut self, remote_id: &str, dest: &Path) -> io::Result<()>;
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

struct SourceState {
    name: String,
    source_type: String,
    interval_secs: u64,
    params: HashMap<String, String>,
}

/// Runs the remote-source sync loop in a dedicated thread.
///
/// `config_path` is watched for changes so sources added through config mode
/// are picked up without restarting the manager.
pub fn run_sync_scheduler(
    config: Config,
    config_path: PathBuf,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut config = config;
    let mut cursor_path = config.photos_dir.join("sync-cursors.json");
    let mut cursors: HashMap<String, String> = load_cursors(&cursor_path);
    // Unix seconds of the last sync attempt per source. Kept in memory so a
    // failed attempt waits for the next interval instead of retrying every
    // second. The cursor file still only records successful syncs.
    let mut last_attempts: HashMap<String, u64> = HashMap::new();
    let mut reloader = ConfigReloader::new(config_path);
    let mut status = SyncStatus::default();
    status.retain_configured(&config);
    status.refresh_last_sync(&cursors);
    status.save(&status_path(&config));

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Remote sync scheduler shutting down");
            break;
        }

        // Pick up config-mode changes before deciding what is due. A parse or
        // validation error leaves the previous config in place so a partially
        // written file cannot stop syncing.
        if let Some(result) = reloader.poll() {
            match result {
                Ok(new_config) => {
                    log::info!(
                        "Reloaded config from {} ({} remote source(s))",
                        reloader.path().display(),
                        new_config.remote_sources.len()
                    );
                    if new_config.photos_dir != config.photos_dir {
                        // Cursors live next to the photos they describe.
                        cursor_path = new_config.photos_dir.join("sync-cursors.json");
                        cursors = load_cursors(&cursor_path);
                    }
                    config = new_config;
                    // Pick up added sources and drop removed ones right away.
                    status.retain_configured(&config);
                    status.refresh_last_sync(&cursors);
                    status.save(&status_path(&config));
                }
                Err(e) => {
                    log::error!("Failed to reload config, keeping previous config: {}", e);
                }
            }
        }

        let sources: Vec<SourceState> = config
            .remote_sources
            .iter()
            .map(|s| SourceState {
                name: s.name.clone(),
                source_type: s.source_type.clone(),
                interval_secs: s.check_interval_seconds,
                params: s.params.clone(),
            })
            .collect();

        for source in &sources {
            let last_sync = cursors.get(&source.name).and_then(|c| parse_cursor_time(c));
            let last_attempt = last_attempts.get(&source.name).copied();

            if !is_due(last_sync, last_attempt, source.interval_secs, unix_now()) {
                continue;
            }

            let attempt_at = unix_now();
            log::info!(
                r#"Syncing remote source "{}" ({})"#,
                source.name,
                source.source_type
            );
            last_attempts.insert(source.name.clone(), attempt_at);
            let result = sync_source(source, &config, &dedup_set, &mut cursors);
            let error = match &result {
                Ok(()) => None,
                Err(e) => {
                    log::error!(r#"Sync failed for "{}": {}"#, source.name, e);
                    Some(e.to_string())
                }
            };
            if result.is_ok() {
                save_cursors(&cursor_path, &cursors);
            }
            // Publish the result for the admin UI after every attempt.
            status.record_attempt(&source.name, attempt_at, error.as_deref());
            status.refresh_last_sync(&cursors);
            status.save(&status_path(&config));
        }

        // Sleep until the next source is due, or 60 seconds when idle.
        let now = unix_now();
        let next_wake = sources
            .iter()
            .map(|s| {
                let last_sync = cursors.get(&s.name).and_then(|c| parse_cursor_time(c));
                let last_attempt = last_attempts.get(&s.name).copied();
                seconds_until_due(last_sync, last_attempt, s.interval_secs, now)
            })
            .min()
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(60));

        let wake_deadline = Instant::now() + next_wake;
        while Instant::now() < wake_deadline {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            // Wake immediately for a config change so newly added sources
            // start syncing without waiting out the current interval.
            if reloader.modified() {
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Config reloading
// ---------------------------------------------------------------------------

/// Watches the config file so the scheduler can reload it without a restart.
pub struct ConfigReloader {
    path: PathBuf,
    last_mtime: Option<SystemTime>,
    last_len: Option<u64>,
}

impl ConfigReloader {
    pub fn new(path: PathBuf) -> Self {
        let (last_mtime, last_len) = file_signature(&path);
        ConfigReloader {
            path,
            last_mtime,
            last_len,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True when the file's mtime or size differs from the last poll.
    pub fn modified(&self) -> bool {
        signature_changed((self.last_mtime, self.last_len), file_signature(&self.path))
    }

    /// Reload the config if the file changed. Returns `None` when unchanged.
    /// On failure the caller keeps its previous config; the new signature is
    /// still recorded so a broken file does not retry on every iteration.
    pub fn poll(&mut self) -> Option<Result<Config, String>> {
        if !self.modified() {
            return None;
        }
        let (mtime, len) = file_signature(&self.path);
        self.last_mtime = mtime;
        self.last_len = len;
        Some(Config::from_file(&self.path))
    }
}

fn file_signature(path: &Path) -> (Option<SystemTime>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(meta) => (meta.modified().ok(), Some(meta.len())),
        Err(_) => (None, None),
    }
}

/// A change is only actionable when the file exists and its signature moved.
fn signature_changed(
    previous: (Option<SystemTime>, Option<u64>),
    current: (Option<SystemTime>, Option<u64>),
) -> bool {
    current.0.is_some() && current != previous
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The most recent time a source was synced or attempted, in Unix seconds.
fn last_touch(last_sync_unix: Option<u64>, last_attempt_unix: Option<u64>) -> Option<u64> {
    match (last_sync_unix, last_attempt_unix) {
        (Some(sync), Some(attempt)) => Some(sync.max(attempt)),
        (Some(sync), None) => Some(sync),
        (None, Some(attempt)) => Some(attempt),
        (None, None) => None,
    }
}

/// A source is due when it has never synced, or when a full interval has
/// elapsed since the most recent successful sync or failed attempt.
fn is_due(
    last_sync_unix: Option<u64>,
    last_attempt_unix: Option<u64>,
    interval_secs: u64,
    now_unix: u64,
) -> bool {
    match last_touch(last_sync_unix, last_attempt_unix) {
        None => true,
        Some(last) => now_unix >= last.saturating_add(interval_secs),
    }
}

/// Seconds to wait before the source is due again, with a one-second floor to
/// keep the scheduler from spinning.
fn seconds_until_due(
    last_sync_unix: Option<u64>,
    last_attempt_unix: Option<u64>,
    interval_secs: u64,
    now_unix: u64,
) -> u64 {
    match last_touch(last_sync_unix, last_attempt_unix) {
        None => 1,
        Some(last) => last
            .saturating_add(interval_secs)
            .saturating_sub(now_unix)
            .max(1),
    }
}

fn sync_source(
    source: &SourceState,
    config: &Config,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
    cursors: &mut HashMap<String, String>,
) -> io::Result<()> {
    let cursor = cursors.get(&source.name).map(|s| s.as_str());
    match source.source_type.as_str() {
        "dropbox" => {
            let mut src = crate::import::dropbox::DropboxSource::connect(&source.params)?;
            let (photos, new_cursor) = src.list_changes(cursor)?;
            download_and_import(&mut src, &photos, config, dedup_set)?;
            cursors.insert(source.name.clone(), new_cursor);
        }
        "google_drive" => {
            let mut src = crate::import::gdrive::GoogleDriveSource::connect(&source.params)?;
            let (photos, new_cursor) = src.list_changes(cursor)?;
            download_and_import(&mut src, &photos, config, dedup_set)?;
            cursors.insert(source.name.clone(), new_cursor);
        }
        other => {
            log::warn!(
                r#"Unknown source type "{}" for "{}" — skipping"#,
                other,
                source.name
            );
            cursors.insert(source.name.clone(), unix_now().to_string());
        }
    }

    Ok(())
}

/// Download and import a batch of photos.
///
/// Returns an error if any download failed so the caller does not advance the
/// cursor: a cursor is a promise that everything up to that point was handled,
/// and advancing it after a failed download would skip those photos forever.
/// Import or conversion failures *after* a successful download are logged and
/// skipped instead — one corrupt image would otherwise pin the cursor and
/// block every future sync.
fn download_and_import(
    src: &mut dyn RemotePhotoSource,
    photos: &[RemotePhoto],
    config: &Config,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
) -> io::Result<()> {
    let tmp_dir = sync_tmp_dir(config);
    // Creating the staging directory can also fail with ENOSPC, so it goes
    // through the same rotate-and-retry path as the downloads.
    run_with_rotation(
        || prepare_tmp_dir(&tmp_dir),
        &config.photos_dir,
        config.batch_delete_size,
    )?;

    let mut failed: Vec<String> = Vec::new();

    for (index, photo) in photos.iter().enumerate() {
        // The importer derives `original_name` from the file name, so the
        // staged file keeps the remote name. The per-item directory avoids
        // collisions instead of renaming the file.
        let dest = temp_path(&tmp_dir, &photo.filename, index);

        // A full photos partition would otherwise fail the download forever:
        // the normal import rotation only runs after a download succeeds. Free
        // space by deleting the oldest photos and retry the download.
        let downloaded = run_with_rotation(
            || {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                src.download(&photo.remote_id, &dest)
            },
            &config.photos_dir,
            config.batch_delete_size,
        );

        if let Err(e) = downloaded {
            log::warn!(r#"Download failed for "{}": {}"#, photo.filename, e);
            cleanup_temp_file(&dest);
            failed.push(format!("{}: {}", photo.filename, e));
            continue;
        }

        match import_single_photo(
            &dest,
            &config.photos_dir,
            &config.photos_dir,
            dedup_set,
            config,
        ) {
            Ok(true) => {
                log::info!(r#"Imported "{}""#, photo.filename);
            }
            Ok(false) => {
                log::debug!(r#"Skipped duplicate "{}""#, photo.filename);
            }
            Err(e) => {
                log::warn!(r#"Failed to import "{}": {}"#, photo.filename, e);
            }
        }

        cleanup_temp_file(&dest);
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} of {} download(s) failed: {}",
            failed.len(),
            photos.len(),
            failed.join("; ")
        )))
    }
}

/// Run `op`, rotating out the oldest photos whenever it fails with ENOSPC.
///
/// Remote downloads stage files on the photos partition, so a full partition
/// fails them before the normal import rotation can run. Rotation always makes
/// progress or errors out: each pass deletes valid index entries, and we stop
/// as soon as rotation frees nothing (no photos left). Returns the total number
/// of photos deleted.
fn run_with_rotation<F>(mut op: F, photos_dir: &Path, batch_delete_size: usize) -> io::Result<usize>
where
    F: FnMut() -> io::Result<()>,
{
    let mut deleted_total = 0;

    loop {
        match op() {
            Ok(()) => return Ok(deleted_total),
            Err(e) if !is_enospc(&e) => return Err(e),
            Err(e) => {
                log::warn!("Disk full ({}), attempting rotation", e);

                let deleted = rotate_for_space(photos_dir, batch_delete_size)?;
                if deleted == 0 {
                    return Err(io::Error::other(format!(
                        "Operation failed and rotation freed no space: {}",
                        e
                    )));
                }
                log::info!("Deleted {} old photos to free space", deleted);
                deleted_total += deleted;
            }
        }
    }
}

/// In-flight downloads live under the photos partition instead of tmpfs so a
/// large batch does not consume RAM. The directory is hidden and outside the
/// index, so the display loop never sees it.
fn sync_tmp_dir(config: &Config) -> PathBuf {
    config.photos_dir.join(".sync-tmp")
}

/// Create the temp directory and clear anything left behind by a crash or an
/// interrupted sync, including the per-item subdirectories.
fn prepare_tmp_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Reduce a remote filename to a single path component so a hostile or unusual
/// name cannot escape the temp directory.
fn safe_filename(filename: &str) -> String {
    Path::new(filename)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "photo".to_string())
}

/// Staging path for one photo: `tmp_dir/{index}/{safe_name}`.
///
/// The subdirectory keeps two photos with the same name from colliding while
/// the file itself keeps the remote name, because `import_single_photo` stores
/// `file_name()` as the index's `original_name`.
fn temp_path(tmp_dir: &Path, filename: &str, index: usize) -> PathBuf {
    tmp_dir
        .join(index.to_string())
        .join(safe_filename(filename))
}

/// Remove a staged file and its now-empty per-item directory.
fn cleanup_temp_file(dest: &Path) {
    let _ = std::fs::remove_file(dest);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

// ---------------------------------------------------------------------------
// Sync status file
// ---------------------------------------------------------------------------

/// Outcome of the most recent attempt for a source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncState {
    #[default]
    Ok,
    Error,
}

/// Per-source status published to `<photos_dir>/sync-status.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// Unix seconds of the last successful cursor timestamp, or null.
    pub last_sync: Option<u64>,
    /// Unix seconds of the last attempt, or null if never attempted.
    pub last_attempt: Option<u64>,
    pub state: SyncState,
    pub error: Option<String>,
}

/// The whole status document. Shapes the JSON as `{"sources": {name: {...}}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub sources: BTreeMap<String, SourceStatus>,
}

fn status_path(config: &Config) -> PathBuf {
    config.photos_dir.join("sync-status.json")
}

impl SyncStatus {
    /// Keep only sources that are currently configured, adding an empty entry
    /// for each new one so the admin UI sees it before its first attempt.
    pub fn retain_configured(&mut self, config: &Config) {
        let configured: HashSet<&str> = config
            .remote_sources
            .iter()
            .map(|source| source.name.as_str())
            .collect();
        self.sources
            .retain(|name, _| configured.contains(name.as_str()));
        for source in &config.remote_sources {
            self.sources.entry(source.name.clone()).or_default();
        }
    }

    /// Pick up the last successful sync timestamp stored in the cursor file.
    pub fn refresh_last_sync(&mut self, cursors: &HashMap<String, String>) {
        for (name, status) in self.sources.iter_mut() {
            if let Some(timestamp) = cursors
                .get(name)
                .and_then(|cursor| parse_cursor_time(cursor))
            {
                status.last_sync = Some(timestamp);
            }
        }
    }

    /// Record one attempt. `error` is `None` for a successful sync.
    pub fn record_attempt(&mut self, name: &str, attempt_at: u64, error: Option<&str>) {
        let status = self.sources.entry(name.to_string()).or_default();
        status.last_attempt = Some(attempt_at);
        match error {
            Some(message) => {
                status.state = SyncState::Error;
                status.error = Some(message.to_string());
            }
            None => {
                status.state = SyncState::Ok;
                status.error = None;
            }
        }
    }

    /// Write the file atomically (temp file + rename) so the admin server never
    /// reads a half-written document.
    pub fn save(&self, path: &Path) {
        let json = match serde_json::to_string(self) {
            Ok(json) => json,
            Err(e) => {
                log::warn!("Failed to serialize sync status: {}", e);
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &json) {
            log::warn!("Failed to write sync status {}: {}", tmp.display(), e);
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            log::warn!("Failed to publish sync status {}: {}", path.display(), e);
        }
    }
}

// ---------------------------------------------------------------------------
// Cursor file helpers
// ---------------------------------------------------------------------------

fn load_cursors(path: &Path) -> HashMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_cursors(path: &Path, cursors: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string(cursors) {
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::write(&tmp, &json);
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Cursors written by the remote sources are Unix timestamps in seconds.
fn parse_cursor_time(cursor: &str) -> Option<u64> {
    cursor.parse::<u64>().ok()
}

/// Parse an ISO 8601 timestamp like "2015-05-12T15:50:38Z" to a Unix timestamp.
/// Used by both Dropbox and Google Drive sources.
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z').unwrap_or(s);
    if s.len() < 19 {
        return None;
    }
    let year: i32 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let min: u32 = s[14..16].parse().ok()?;
    let sec: u32 = s[17..19].parse().ok()?;

    let days_before_month: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut days = (year - 1970) as i64 * 365;
    days += ((year - 1 - 1968) / 4) as i64;
    days -= ((year - 1 - 1900) / 100) as i64;
    days += ((year - 1 - 1600) / 400) as i64;
    days += days_before_month[(month - 1) as usize] as i64;
    if month > 2 && ((year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)) {
        days += 1;
    }
    days += (day - 1) as i64;

    Some((days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso8601() {
        assert_eq!(parse_iso8601("2021-01-01T00:00:00Z"), Some(1609459200));
        assert_eq!(parse_iso8601("2015-05-12T15:50:38Z"), Some(1431445838));
        assert!(parse_iso8601("invalid").is_none());
    }

    #[test]
    fn test_is_due_never_synced() {
        assert!(is_due(None, None, 3600, 1_000));
    }

    #[test]
    fn test_is_due_synced_recently() {
        assert!(!is_due(Some(1_000), None, 3600, 1_500));
    }

    #[test]
    fn test_is_due_interval_elapsed() {
        assert!(!is_due(Some(1_000), None, 3600, 4_599));
        assert!(is_due(Some(1_000), None, 3600, 4_600));
        assert!(is_due(Some(1_000), None, 3600, 9_999));
    }

    #[test]
    fn test_is_due_after_failed_attempt() {
        // The last successful sync is old, but the most recent attempt just
        // failed; wait for the next interval rather than retrying immediately.
        assert!(!is_due(Some(1_000), Some(4_000), 3600, 4_500));
        assert!(is_due(Some(1_000), Some(4_000), 3600, 7_600));
    }

    #[test]
    fn test_seconds_until_due() {
        // Due time is last successful sync + interval.
        assert_eq!(seconds_until_due(Some(1_000), None, 3600, 1_000), 3600);
        // A newer attempt pushes the due time forward.
        assert_eq!(
            seconds_until_due(Some(1_000), Some(2_000), 3600, 4_600),
            1_000
        );
        // Already overdue: wake in one second rather than spinning.
        assert_eq!(seconds_until_due(Some(1_000), None, 3600, 5_000), 1);
        // Never synced: due immediately, but still wait one second.
        assert_eq!(seconds_until_due(None, None, 3600, 1_000), 1);
    }

    #[test]
    fn test_parse_cursor_time() {
        assert_eq!(parse_cursor_time("1609459200"), Some(1_609_459_200));
        assert_eq!(parse_cursor_time("2021-01-01T00:00:00Z"), None);
    }

    #[test]
    fn test_safe_filename_blocks_traversal() {
        assert_eq!(safe_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_filename("/abs/photo.jpg"), "photo.jpg");
        assert_eq!(safe_filename("dup.jpg"), "dup.jpg");
        // Names with no usable component fall back to a fixed stem.
        assert_eq!(safe_filename(""), "photo");
        assert_eq!(safe_filename(".."), "photo");
    }

    #[test]
    fn test_temp_path_preserves_name_and_avoids_collisions() {
        let tmp = Path::new("/photos/.sync-tmp");
        assert_eq!(
            temp_path(tmp, "foo.jpg", 0),
            PathBuf::from("/photos/.sync-tmp/0/foo.jpg")
        );
        assert_eq!(
            temp_path(tmp, "../../etc/passwd", 1),
            PathBuf::from("/photos/.sync-tmp/1/passwd")
        );
        // Same name, different items: separate directories, same file name.
        assert_ne!(temp_path(tmp, "dup.jpg", 0), temp_path(tmp, "dup.jpg", 1));
        assert_eq!(temp_path(tmp, "dup.jpg", 1).file_name().unwrap(), "dup.jpg");
    }

    use std::collections::HashMap;

    struct FakeSource {
        fail_downloads: bool,
        list: Vec<RemotePhoto>,
    }

    impl RemotePhotoSource for FakeSource {
        fn connect(_config: &HashMap<String, String>) -> io::Result<Self> {
            Ok(FakeSource {
                fail_downloads: false,
                list: Vec::new(),
            })
        }

        fn list_changes(
            &mut self,
            _cursor: Option<&str>,
        ) -> io::Result<(Vec<RemotePhoto>, String)> {
            Ok((std::mem::take(&mut self.list), "1".to_string()))
        }

        fn download(&mut self, _remote_id: &str, dest: &Path) -> io::Result<()> {
            if self.fail_downloads {
                return Err(io::Error::other("simulated download failure"));
            }
            std::fs::write(dest, b"fake image bytes")
        }
    }

    /// Records the file name the importer would see for each download.
    struct RecordingSource {
        seen: Vec<String>,
    }

    impl RemotePhotoSource for RecordingSource {
        fn connect(_config: &HashMap<String, String>) -> io::Result<Self> {
            Ok(RecordingSource { seen: Vec::new() })
        }

        fn list_changes(
            &mut self,
            _cursor: Option<&str>,
        ) -> io::Result<(Vec<RemotePhoto>, String)> {
            Ok((Vec::new(), "1".to_string()))
        }

        fn download(&mut self, _remote_id: &str, dest: &Path) -> io::Result<()> {
            self.seen.push(
                dest.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            std::fs::write(dest, b"fake image bytes")
        }
    }

    #[test]
    fn test_download_preserves_original_filename_for_same_named_photos() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let dedup_set = Arc::new(Mutex::new(HashSet::new()));
        let mut source = RecordingSource { seen: Vec::new() };
        let photos = vec![photo("foo.jpg"), photo("foo.jpg")];

        download_and_import(&mut source, &photos, &config, &dedup_set).unwrap();

        // Both are staged separately but keep the remote name, so the index
        // records "foo.jpg" rather than a prefixed temp name.
        assert_eq!(source.seen, vec!["foo.jpg", "foo.jpg"]);
        let tmp = dir.path().join(".sync-tmp");
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            0,
            "per-item directories should be cleaned up"
        );
    }

    fn test_config(photos_dir: &Path) -> Config {
        Config {
            photos_dir: photos_dir.to_path_buf(),
            display_socket_path: PathBuf::from("/tmp/display.sock"),
            control_socket_path: PathBuf::from("/tmp/control.sock"),
            native_resolution: "32x32".to_string(),
            aspect_ratio_mode: Default::default(),
            batch_delete_size: 20,
            log_max_size: 262_144,
            log_max_files: 2,
            remote_sources: Vec::new(),
        }
    }

    fn test_config_with_sources(photos_dir: &Path, names: &[&str]) -> Config {
        let mut config = test_config(photos_dir);
        config.remote_sources = names
            .iter()
            .map(|name| crate::config::RemoteSourceConfig {
                source_type: "dropbox".to_string(),
                name: (*name).to_string(),
                params: HashMap::new(),
                check_interval_seconds: 86_400,
            })
            .collect();
        config
    }

    fn read_status(path: &Path) -> SyncStatus {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn test_sync_status_round_trip_and_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-status.json");

        let mut status = SyncStatus::default();
        status.sources.insert(
            "My Dropbox".to_string(),
            SourceStatus {
                last_sync: Some(100),
                last_attempt: Some(200),
                state: SyncState::Ok,
                error: None,
            },
        );
        status.sources.insert(
            "Old Drive".to_string(),
            SourceStatus {
                last_sync: None,
                last_attempt: Some(50),
                state: SyncState::Error,
                error: Some("boom".to_string()),
            },
        );
        status.save(&path);

        // The admin server depends on this exact shape.
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["sources"]["My Dropbox"]["last_sync"], 100);
        assert_eq!(raw["sources"]["My Dropbox"]["last_attempt"], 200);
        assert_eq!(raw["sources"]["My Dropbox"]["state"], "ok");
        assert!(raw["sources"]["My Dropbox"]["error"].is_null());
        assert_eq!(raw["sources"]["Old Drive"]["state"], "error");
        assert_eq!(raw["sources"]["Old Drive"]["error"], "boom");
        assert!(raw["sources"]["Old Drive"]["last_sync"].is_null());

        // Sources removed from config disappear; new ones get a blank entry.
        let config = test_config_with_sources(dir.path(), &["My Dropbox", "New Source"]);
        let mut reloaded = read_status(&path);
        reloaded.retain_configured(&config);
        assert!(reloaded.sources.contains_key("My Dropbox"));
        assert!(reloaded.sources.contains_key("New Source"));
        assert!(!reloaded.sources.contains_key("Old Drive"));
        assert_eq!(
            reloaded.sources["New Source"],
            SourceStatus::default(),
            "a new source starts with no attempts"
        );
        assert_eq!(reloaded.sources["My Dropbox"].last_sync, Some(100));

        reloaded.save(&path);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "atomic save must not leave a temp file"
        );
        let final_status = read_status(&path);
        assert_eq!(final_status.sources.len(), 2);
    }

    #[test]
    fn test_sync_status_records_attempts_and_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-status.json");
        let config = test_config_with_sources(dir.path(), &["My Dropbox"]);
        let mut status = SyncStatus::default();
        status.retain_configured(&config);
        assert_eq!(status.sources["My Dropbox"].last_attempt, None);

        status.record_attempt("My Dropbox", 1_000, Some("network down"));
        assert_eq!(status.sources["My Dropbox"].state, SyncState::Error);
        assert_eq!(
            status.sources["My Dropbox"].error.as_deref(),
            Some("network down")
        );

        let mut cursors = HashMap::new();
        cursors.insert("My Dropbox".to_string(), "1609459200".to_string());
        status.refresh_last_sync(&cursors);
        assert_eq!(status.sources["My Dropbox"].last_sync, Some(1_609_459_200));

        status.record_attempt("My Dropbox", 2_000, None);
        status.refresh_last_sync(&cursors);
        status.save(&path);

        let saved = read_status(&path);
        assert_eq!(saved.sources["My Dropbox"].last_attempt, Some(2_000));
        assert_eq!(saved.sources["My Dropbox"].last_sync, Some(1_609_459_200));
        assert_eq!(saved.sources["My Dropbox"].state, SyncState::Ok);
        assert!(saved.sources["My Dropbox"].error.is_none());

        // The cursor timestamp format is what the file is read back with.
        cursors.insert("My Dropbox".to_string(), "not-a-timestamp".to_string());
        status.refresh_last_sync(&cursors);
        assert_eq!(status.sources["My Dropbox"].last_sync, Some(1_609_459_200));
    }

    #[test]
    fn test_status_path_lives_in_photos_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        assert_eq!(status_path(&config), dir.path().join("sync-status.json"));
    }

    fn photo(name: &str) -> RemotePhoto {
        RemotePhoto {
            remote_id: format!("/photos/{name}"),
            filename: name.to_string(),
            size_bytes: None,
        }
    }

    #[test]
    fn test_download_failure_returns_error_so_cursor_does_not_advance() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let dedup_set = Arc::new(Mutex::new(HashSet::new()));
        let mut source = FakeSource {
            fail_downloads: true,
            list: Vec::new(),
        };
        let photos = vec![photo("a.jpg"), photo("b.jpg")];

        let err = download_and_import(&mut source, &photos, &config, &dedup_set)
            .expect_err("a failed download must be reported");
        assert!(
            err.to_string().contains("2 of 2 download(s) failed"),
            "{err}"
        );

        // No partial files are left behind.
        let tmp = dir.path().join(".sync-tmp");
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
    }

    #[test]
    fn test_successful_downloads_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let dedup_set = Arc::new(Mutex::new(HashSet::new()));
        let mut source = FakeSource {
            fail_downloads: false,
            list: Vec::new(),
        };
        let photos = vec![photo("a.jpg")];

        // Import failures are tolerated (ImageMagick may not be installed and
        // this payload is not a real image); only downloads decide the result.
        download_and_import(&mut source, &photos, &config, &dedup_set).unwrap();

        let tmp = dir.path().join(".sync-tmp");
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
    }

    /// Create `count` placeholder photos plus an index that references them.
    fn create_index_with_photos(dir: &Path, count: usize) -> Vec<PathBuf> {
        use std::io::Write;

        let mut paths = Vec::new();
        for i in 0..count {
            let path = dir.join(format!("{i:05}_photo.jpg"));
            std::fs::write(&path, b"photo").unwrap();
            paths.push(path);
        }

        let meta = crate::index::IndexMetadata {
            start_line: 0,
            valid_count: count,
        };
        let mut file =
            std::fs::File::create(dir.join(crate::index::build_index_filename(&meta))).unwrap();
        for (i, path) in paths.iter().enumerate() {
            writeln!(file, "{},photo{}.jpg,{}", path.display(), i, i + 1).unwrap();
        }
        file.flush().unwrap();

        paths
    }

    /// Fails the first download with ENOSPC, then succeeds.
    struct EnospcOnceSource {
        failed_once: bool,
    }

    impl RemotePhotoSource for EnospcOnceSource {
        fn connect(_config: &HashMap<String, String>) -> io::Result<Self> {
            Ok(EnospcOnceSource { failed_once: false })
        }

        fn list_changes(
            &mut self,
            _cursor: Option<&str>,
        ) -> io::Result<(Vec<RemotePhoto>, String)> {
            Ok((Vec::new(), "1".to_string()))
        }

        fn download(&mut self, _remote_id: &str, dest: &Path) -> io::Result<()> {
            if !self.failed_once {
                self.failed_once = true;
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }
            std::fs::write(dest, b"fake image bytes")
        }
    }

    /// Always fails downloads with ENOSPC.
    struct AlwaysEnospcSource;

    impl RemotePhotoSource for AlwaysEnospcSource {
        fn connect(_config: &HashMap<String, String>) -> io::Result<Self> {
            Ok(AlwaysEnospcSource)
        }

        fn list_changes(
            &mut self,
            _cursor: Option<&str>,
        ) -> io::Result<(Vec<RemotePhoto>, String)> {
            Ok((Vec::new(), "1".to_string()))
        }

        fn download(&mut self, _remote_id: &str, _dest: &Path) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }
    }

    #[test]
    fn test_download_enospc_rotates_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.batch_delete_size = 2;
        let photos = create_index_with_photos(dir.path(), 3);
        let dedup_set = Arc::new(Mutex::new(HashSet::new()));
        let mut source = EnospcOnceSource { failed_once: false };

        download_and_import(&mut source, &[photo("new.jpg")], &config, &dedup_set).unwrap();

        assert!(!photos[0].exists(), "oldest photo should be deleted");
        assert!(!photos[1].exists(), "second oldest photo should be deleted");
        assert!(photos[2].exists(), "newest photo should be kept");
        let (_path, meta) = crate::index::init_index(dir.path()).unwrap();
        assert_eq!(meta.valid_count, 1, "only the newest photo remains");
    }

    #[test]
    fn test_download_enospc_errors_when_no_photos_remain() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        crate::index::init_index(dir.path()).unwrap();
        let dedup_set = Arc::new(Mutex::new(HashSet::new()));
        let mut source = AlwaysEnospcSource;

        let err = download_and_import(&mut source, &[photo("a.jpg")], &config, &dedup_set)
            .expect_err("an unfixable ENOSPC must be reported");
        assert!(err.to_string().contains("download(s) failed"), "{err}");
    }

    #[test]
    fn test_download_non_enospc_error_does_not_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let photos = create_index_with_photos(dir.path(), 2);
        let dedup_set = Arc::new(Mutex::new(HashSet::new()));
        let mut source = FakeSource {
            fail_downloads: true,
            list: Vec::new(),
        };

        let err = download_and_import(&mut source, &[photo("a.jpg")], &config, &dedup_set)
            .expect_err("a non-ENOSPC failure must be reported");
        assert!(
            err.to_string().contains("simulated download failure"),
            "the original error must be preserved: {err}"
        );
        assert!(
            photos.iter().all(|p| p.exists()),
            "non-ENOSPC errors must not trigger rotation"
        );
    }

    #[test]
    fn test_prepare_tmp_dir_clears_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join(".sync-tmp");
        std::fs::create_dir_all(tmp.join("0")).unwrap();
        std::fs::write(tmp.join("0-stale.jpg"), b"leftover").unwrap();
        std::fs::write(tmp.join("0/foo.jpg"), b"leftover").unwrap();

        prepare_tmp_dir(&tmp).unwrap();

        assert!(tmp.is_dir());
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            0,
            "stale files and per-item directories should be removed"
        );
    }

    fn write_config(path: &Path, photos_dir: &Path, source_names: &[&str]) {
        let mut toml = format!(
            "photos_dir = {:?}\ndisplay_socket_path = \"/tmp/sock\"\nnative_resolution = \"32x32\"\n",
            photos_dir
        );
        for name in source_names {
            toml.push_str(&format!(
                "\n[[remote_sources]]\ntype = \"dropbox\"\nname = \"{name}\"\naccess_token = \"token\"\n"
            ));
        }
        std::fs::write(path, toml).unwrap();
    }

    #[test]
    fn test_signature_changed() {
        let time = SystemTime::UNIX_EPOCH;
        assert!(!signature_changed((None, None), (None, None)));
        // A missing file is never a change, even if a previous signature exists.
        assert!(!signature_changed((Some(time), Some(10)), (None, None)));
        assert!(signature_changed((None, None), (Some(time), Some(10))));
        assert!(signature_changed(
            (Some(time), Some(10)),
            (Some(time), Some(11))
        ));
        assert!(!signature_changed(
            (Some(time), Some(10)),
            (Some(time), Some(10))
        ));
    }

    #[test]
    fn test_config_reloader_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_config(&path, photos.path(), &[]);
        std::thread::sleep(Duration::from_millis(20));

        let mut reloader = ConfigReloader::new(path.clone());
        assert!(!reloader.modified());
        assert!(reloader.poll().is_none());

        write_config(&path, photos.path(), &["One"]);
        assert!(reloader.modified(), "rewritten config should be detected");
        let reloaded = reloader
            .poll()
            .expect("change detected")
            .expect("config should parse");
        assert_eq!(reloaded.remote_sources.len(), 1);
        assert_eq!(reloaded.remote_sources[0].name, "One");

        // Polling again without touching the file is a no-op.
        assert!(!reloader.modified());
        assert!(reloader.poll().is_none());
    }

    #[test]
    fn test_config_reloader_reports_parse_errors_once() {
        let dir = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_config(&path, photos.path(), &[]);
        std::thread::sleep(Duration::from_millis(20));

        let mut reloader = ConfigReloader::new(path.clone());
        std::fs::write(&path, "this is not = valid toml\n").unwrap();

        let err = reloader
            .poll()
            .expect("change detected")
            .expect_err("invalid config should be reported");
        assert!(err.contains("Failed to parse config file"), "got: {err}");

        // The broken signature is remembered so the scheduler does not spin
        // re-parsing an unchanged file.
        assert!(reloader.poll().is_none());
    }
}
