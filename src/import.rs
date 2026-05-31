use crate::config::{AspectRatioMode, Config};
use crate::index::{self, IndexWriter};
use crc32fast::Hasher;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Watches `/media` for USB drive mounts and triggers imports.
pub fn watch_usb_mounts(
    photos_dir: PathBuf,
    index_dir: PathBuf,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    config: Config,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher = Watcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        NotifyConfig::default().with_poll_interval(Duration::from_secs(1)),
    )
    .map_err(|e| io::Error::other(e.to_string()))?;

    watcher
        .watch(Path::new("/media"), RecursiveMode::NonRecursive)
        .map_err(|e| io::Error::other(e.to_string()))?;

    log::info!("Watching /media for USB mounts");

    let mut active_mounts: HashSet<PathBuf> = HashSet::new();

    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            log::info!("USB watcher shutting down");
            break;
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => match event.kind {
                notify::EventKind::Create(notify::event::CreateKind::Folder) => {
                    let paths: Vec<PathBuf> = event.paths.clone();
                    for path in paths {
                        if path.is_dir() && !active_mounts.contains(&path) {
                            log::info!("USB mount detected: {}", path.display());
                            active_mounts.insert(path.clone());
                            let photos_dir = photos_dir.clone();
                            let index_dir = index_dir.clone();
                            let dedup_set = dedup_set.clone();
                            let config = config.clone();
                            std::thread::spawn(move || {
                                if let Err(e) = import_from_mount(&path, &photos_dir, &index_dir, dedup_set, &config) {
                                    log::error!("Import failed for {}: {}", path.display(), e);
                                }
                                log::info!("Import complete for {}", path.display());
                            });
                        }
                    }
                }
                notify::EventKind::Remove(notify::event::RemoveKind::Folder) => {
                    for path in &event.paths {
                        active_mounts.remove(path);
                        log::info!("USB unmount detected: {}", path.display());
                    }
                }
                _ => {}
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No event within timeout, loop back and check shutdown
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                log::warn!("USB watcher channel disconnected");
                break;
            }
        }
    }

    Ok(())
}

/// Import all JPEGs from a mounted USB drive.
fn import_from_mount(
    mount_point: &Path,
    photos_dir: &Path,
    index_dir: &Path,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    config: &Config,
) -> io::Result<()> {
    let photos = find_jpegs(mount_point);
    let mut imported = 0;
    let mut skipped = 0;

    for photo_path in photos {
        match import_single_photo(&photo_path, photos_dir, index_dir, &dedup_set, config) {
            Ok(true) => imported += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                log::warn!("Failed to import {}: {}", photo_path.display(), e);
            }
        }
    }

    log::info!(
        "Import summary: {} imported, {} skipped (duplicates), {} errors",
        imported,
        skipped,
        0
    );
    Ok(())
}

/// Find all JPEG files under a directory, recursively.
fn find_jpegs(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                result.extend(find_jpegs(&path));
            } else if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if ext == "jpg" || ext == "jpeg" {
                    result.push(path);
                }
            }
        }
    }
    result
}

/// Import a single photo. Returns Ok(true) if imported, Ok(false) if skipped (duplicate).
fn import_single_photo(
    src_path: &Path,
    photos_dir: &Path,
    index_dir: &Path,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
    config: &Config,
) -> io::Result<bool> {
    // Compute hash
    let hash = compute_file_hash(src_path)?;

    // Check deduplication
    {
        let set = dedup_set.lock().unwrap();
        if set.contains(&hash) {
            log::debug!("Skipping duplicate: {}", src_path.display());
            return Ok(false);
        }
    }

    // Determine destination path based on file mtime
    let mtime = fs::metadata(src_path)?.modified().unwrap_or(SystemTime::now());
    let dest_path = build_dest_path(src_path, photos_dir, mtime);

    // Ensure parent directory exists
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Convert and copy
    let (width, height) = config.resolution();
    let mode = &config.aspect_ratio_mode;
    match convert_image(src_path, &dest_path, width, height, mode) {
        Ok(()) => {}
        Err(e) => {
            // If ENOSPC, try to free space and retry once
            if e.kind() == io::ErrorKind::WriteZero {
                log::warn!("Disk full, attempting rotation");
                let (_index_path, meta) = index::init_index(index_dir)?;
                let (_new_meta, deleted) = index::delete_oldest(index_dir, &meta, config.batch_delete_size)?;
                log::info!("Deleted {} old photos to free space", deleted);
                // Retry the conversion
                if let Err(e2) = convert_image(src_path, &dest_path, width, height, mode) {
                    return Err(io::Error::other(
                        format!("Conversion failed after rotation: {}", e2),
                    ));
                }
            } else {
                return Err(e);
            }
        }
    }

    // Append to index
    let original_name = src_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let (_index_path, meta) = index::init_index(index_dir)?;
    let mut writer = IndexWriter::open(index_dir, meta)?;
    let line_number = writer.append(
        &dest_path.to_string_lossy(),
        &original_name,
        hash,
    )?;
    writer.sync_metadata()?;

    // Add to dedup set
    {
        let mut set = dedup_set.lock().unwrap();
        set.insert(hash);
    }

    log::info!(
        "Imported {} -> {} (line {})",
        src_path.display(),
        dest_path.display(),
        line_number
    );

    Ok(true)
}

/// Compute a fast hash of the first 32KB + file size.
fn compute_file_hash(path: &Path) -> io::Result<u64> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len();

    let mut file = fs::File::open(path)?;
    let mut buffer = vec![0u8; 32 * 1024]; // 32KB
    let bytes_read = file.read(&mut buffer)?;
    buffer.truncate(bytes_read);

    let mut hasher = Hasher::new();
    hasher.update(&buffer);
    hasher.update(&size.to_le_bytes());
    Ok(hasher.finalize() as u64)
}

/// Build the destination path: photos_dir/YYYY/MM/DD/DDDDD_original_name.jpg
fn build_dest_path(src_path: &Path, photos_dir: &Path, mtime: SystemTime) -> PathBuf {
    let duration = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
    let datetime = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
        .unwrap_or_else(chrono::Utc::now);

    let year = datetime.format("%Y").to_string();
    let month = datetime.format("%m").to_string();
    let day = datetime.format("%d").to_string();

    let original_name = src_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // For now, use a timestamp-based sequence number since we don't know the CSV line yet
    // The actual sequence number will be assigned after CSV append
    let seq = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let seq_str = format!("{:05}", seq % 100000);

    photos_dir
        .join(year)
        .join(month)
        .join(day)
        .join(format!("{}_{}", seq_str, original_name))
}

/// Convert an image using ImageMagick.
fn convert_image(
    src: &Path,
    dest: &Path,
    width: u32,
    height: u32,
    mode: &AspectRatioMode,
) -> io::Result<()> {
    let magick_cmd = if Command::new("magick").arg("--version").output().is_ok() {
        "magick"
    } else {
        "convert"
    };

    let output = if matches!(mode, AspectRatioMode::Fill) {
        Command::new(magick_cmd)
            .arg(src)
            .arg("-resize")
            .arg(format!("{}x{}^", width, height))
            .arg("-gravity")
            .arg("center")
            .arg("-extent")
            .arg(format!("{}x{}", width, height))
            .arg(dest)
            .output()?
    } else {
        Command::new(magick_cmd)
            .arg(src)
            .arg("-resize")
            .arg(format!("{}x{}", width, height))
            .arg(dest)
            .output()?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(
            format!("ImageMagick failed: {}", stderr),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_compute_file_hash() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("test.jpg");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"hello world").unwrap();

        let hash1 = compute_file_hash(&path).unwrap();
        let hash2 = compute_file_hash(&path).unwrap();
        assert_eq!(hash1, hash2);

        // Different content should yield different hash
        let path2 = tmpdir.path().join("test2.jpg");
        let mut file2 = File::create(&path2).unwrap();
        file2.write_all(b"different content here").unwrap();
        let hash3 = compute_file_hash(&path2).unwrap();
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_find_jpegs() {
        let tmpdir = tempfile::tempdir().unwrap();
        File::create(tmpdir.path().join("photo1.jpg")).unwrap();
        File::create(tmpdir.path().join("photo2.JPEG")).unwrap();
        File::create(tmpdir.path().join("notaphoto.txt")).unwrap();

        let subdir = tmpdir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        File::create(subdir.join("nested.jpg")).unwrap();

        let jpegs = find_jpegs(tmpdir.path());
        assert_eq!(jpegs.len(), 3);
    }

    #[test]
    fn test_build_dest_path() {
        let photos_dir = PathBuf::from("/photos");
        let src = PathBuf::from("/usb/myphoto.jpg");
        let mtime = UNIX_EPOCH + Duration::from_secs(1609459200); // 2021-01-01
        let dest = build_dest_path(&src, &photos_dir, mtime);
        let dest_str = dest.to_string_lossy();
        assert!(dest_str.contains("/photos/2021/01/01/"));
        assert!(dest_str.contains("myphoto.jpg"));
    }
}
