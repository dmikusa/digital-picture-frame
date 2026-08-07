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
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
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
    _config: &Config,
    _dedup_set: &Arc<Mutex<HashSet<u64>>>,
    cursors: &mut HashMap<String, String>,
) -> io::Result<()> {
    let _new_cursor: String;

    let other = source.source_type.as_str();
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
