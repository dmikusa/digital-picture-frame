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
use crate::import::usb::import_single_photo;
use std::collections::{HashMap, HashSet};
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
    #[allow(dead_code)]
    params: HashMap<String, String>,
}

/// Runs the remote-source sync loop in a dedicated thread.
pub fn run_sync_scheduler(
    config: Config,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let cursor_path = config.photos_dir.join("sync-cursors.json");
    let mut cursors: HashMap<String, String> = load_cursors(&cursor_path);
    // Unix seconds of the last sync attempt per source. Kept in memory so a
    // failed attempt waits for the next interval instead of retrying every
    // second. The cursor file still only records successful syncs.
    let mut last_attempts: HashMap<String, u64> = HashMap::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Remote sync scheduler shutting down");
            break;
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

            log::info!(
                r#"Syncing remote source "{}" ({})"#,
                source.name,
                source.source_type
            );
            last_attempts.insert(source.name.clone(), unix_now());
            if let Err(e) = sync_source(source, &config, &dedup_set, &mut cursors) {
                log::error!(r#"Sync failed for "{}": {}"#, source.name, e);
            }
            save_cursors(&cursor_path, &cursors);
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
        }
    }

    Ok(())
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

fn download_and_import(
    src: &mut dyn RemotePhotoSource,
    photos: &[RemotePhoto],
    config: &Config,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
) -> io::Result<()> {
    let tmp_dir = PathBuf::from("/tmp/photo-frame-sync");
    std::fs::create_dir_all(&tmp_dir)?;

    for photo in photos {
        let dest = tmp_dir.join(&photo.filename);
        if let Err(e) = src.download(&photo.remote_id, &dest) {
            log::warn!(r#"Download failed for "{}": {}"#, photo.filename, e);
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

        let _ = std::fs::remove_file(&dest);
    }

    Ok(())
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
}
