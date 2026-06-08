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

use crate::display::DisplayClient;
use crate::index::{self, IndexReader};
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Run the display loop: stream photos from the index and send them to the display app.
pub fn run_display_loop(
    index_dir: &Path,
    socket_path: &Path,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let (index_path, mut metadata) = index::init_index(index_dir)?;
    log::info!("Display loop using index: {}", index_path.display());

    // Compact index on startup if ghost ratio > 50%
    if metadata.ghost_ratio() > 0.5 {
        log::info!(
            "Compacting index (ghost ratio: {:.2})",
            metadata.ghost_ratio()
        );
        metadata = index::compact_index(index_dir, &metadata)?;
    }

    let mut reader = IndexReader::open(&index_path, metadata)?;

    // Pick a random starting line within the valid range
    let valid_count = metadata.valid_count;
    let start_line = if valid_count > 0 {
        let random_offset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as usize
            % valid_count;
        metadata.start_line + random_offset
    } else {
        metadata.start_line
    };

    if valid_count > 0 {
        reader.seek_to(start_line)?;
        log::info!("Starting display from line {}", start_line);
    }

    let mut display = DisplayClient::new(socket_path);

    // Set up file watcher for index changes
    let (notify_tx, notify_rx) = std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher = Watcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = notify_tx.send(event);
            }
        },
        NotifyConfig::default().with_poll_interval(Duration::from_secs(1)),
    )
    .map_err(|e| io::Error::other(e.to_string()))?;

    watcher
        .watch(index_dir, RecursiveMode::NonRecursive)
        .map_err(|e| io::Error::other(e.to_string()))?;

    let mut current_line = reader.current_line();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log::info!("Display loop shutting down");
            display.close();
            break;
        }

        // Check for index change notifications
        if let Ok(event) = notify_rx.try_recv() {
            match event.kind {
                notify::EventKind::Modify(_) | notify::EventKind::Create(_) => {
                    log::info!("Index file changed, reopening");
                    // Re-init index and seek to previous position
                    let (new_path, new_meta) = index::init_index(index_dir)?;
                    metadata = new_meta;
                    reader = IndexReader::open(&new_path, metadata)?;
                    if let Err(e) = reader.seek_to(current_line) {
                        log::warn!("Failed to seek to previous position: {}", e);
                        // If seek fails, just start from the beginning of valid lines
                        let _ = reader.seek_to(metadata.start_line);
                    }
                }
                _ => {}
            }
        }

        match reader.next_record() {
            Ok(Some(record)) => {
                current_line = record.line_number + 1;
                if let Err(e) = display.send_img(&record.path) {
                    log::warn!("Failed to send image to display: {}", e);
                    // Wait a bit before retrying
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            Ok(None) => {
                // EOF reached, wrap to start_line
                if metadata.valid_count > 0 {
                    log::debug!("Reached end of index, wrapping to start");
                    if let Err(e) = reader.seek_to(metadata.start_line) {
                        log::warn!("Failed to wrap to start: {}", e);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    current_line = metadata.start_line;
                } else {
                    // No valid photos, wait for new ones
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
            Err(e) => {
                log::warn!("Error reading index: {}", e);
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }

    Ok(())
}
