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
    interval: Duration,
    #[allow(dead_code)]
    params: HashMap<String, String>,
    last_sync: Option<Instant>,
}

/// Runs the remote-source sync loop in a dedicated thread.
pub fn run_sync_scheduler(
    config: Config,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let cursor_path = config.photos_dir.join("sync-cursors.json");
    let mut cursors: HashMap<String, String> = load_cursors(&cursor_path);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Remote sync scheduler shutting down");
            break;
        }

        let sources: Vec<SourceState> = config
            .remote_sources
            .iter()
            .map(|s| {
                let last_sync = cursors
                    .get(&s.name)
                    .and_then(|c| parse_cursor_time(c))
                    .and_then(|secs| Instant::now().checked_sub(secs));
                SourceState {
                    name: s.name.clone(),
                    source_type: s.source_type.clone(),
                    interval: Duration::from_secs(s.check_interval_seconds),
                    params: s.params.clone(),
                    last_sync,
                }
            })
            .collect();

        for source in &sources {
            if !should_sync(source) && cursors.contains_key(&source.name) {
                continue;
            }

            log::info!(
                r#"Syncing remote source "{}" ({})"#,
                source.name,
                source.source_type
            );
            if let Err(e) = sync_source(source, &config, &dedup_set, &mut cursors) {
                log::error!(r#"Sync failed for "{}": {}"#, source.name, e);
            }
            save_cursors(&cursor_path, &cursors);
        }

        // Sleep until the next source is due, or 60 seconds, whichever is sooner
        let next_wake = sources
            .iter()
            .filter_map(|s| {
                let elapsed = s.last_sync.map(|t| t.elapsed()).unwrap_or(s.interval);
                if elapsed >= s.interval {
                    None
                } else {
                    Some(s.interval - elapsed)
                }
            })
            .min()
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

fn should_sync(source: &SourceState) -> bool {
    source
        .last_sync
        .map(|t| t.elapsed() >= source.interval)
        .unwrap_or(true) // never synced
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
        other => {
            log::warn!(
                r#"Unknown source type "{}" for "{}" — skipping"#,
                other,
                source.name
            );
            cursors.insert(
                source.name.clone(),
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string(),
            );
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

fn parse_cursor_time(cursor: &str) -> Option<Duration> {
    cursor.parse::<u64>().ok().map(Duration::from_secs)
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
}
