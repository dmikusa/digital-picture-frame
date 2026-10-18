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

use crate::config::{AspectRatioMode, Config};
use crate::index::{self, IndexWriter};
use crc32fast::Hasher;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Events emitted by the USB watcher for the config mode state machine.
#[derive(Debug, Clone)]
pub enum MountEvent {
    Inserted(PathBuf),
    Removed(PathBuf),
    ImportComplete(PathBuf),
}

/// Watches `/media` for USB drive mounts and triggers imports.
/// If `event_tx` is provided, sends `MountEvent`s for config mode integration.
pub fn watch_usb_mounts(
    photos_dir: PathBuf,
    index_dir: PathBuf,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    config: Config,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    event_tx: Option<mpsc::Sender<MountEvent>>,
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

    // Import drives that are already mounted at startup. Attaching a filesystem
    // over an existing directory does not emit a Create event, so without this a
    // drive that is mounted before the manager starts would never be imported.
    sweep_mounts(
        &mut active_mounts,
        &photos_dir,
        &index_dir,
        &dedup_set,
        &config,
        &event_tx,
    );

    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            log::info!("USB watcher shutting down");
            break;
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => match event.kind {
                notify::EventKind::Create(notify::event::CreateKind::Folder) => {
                    for path in event.paths {
                        if path.is_dir() && !active_mounts.contains(&path) {
                            active_mounts.insert(path.clone());
                            spawn_import(
                                path,
                                photos_dir.clone(),
                                index_dir.clone(),
                                dedup_set.clone(),
                                config.clone(),
                                event_tx.clone(),
                            );
                        }
                    }
                }
                notify::EventKind::Remove(notify::event::RemoveKind::Folder) => {
                    for path in &event.paths {
                        active_mounts.remove(path);
                        log::info!("USB unmount detected: {}", path.display());
                        if let Some(ref tx) = event_tx {
                            let _ = tx.send(MountEvent::Removed(path.clone()));
                        }
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

/// Scan `/media` for mount points that have not been imported yet and start an
/// import for each. Covers drives that are already mounted when the watcher
/// starts.
fn sweep_mounts(
    active_mounts: &mut HashSet<PathBuf>,
    photos_dir: &Path,
    index_dir: &Path,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
    config: &Config,
    event_tx: &Option<mpsc::Sender<MountEvent>>,
) {
    let entries = match fs::read_dir("/media") {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Failed to scan /media for mounts: {}", e);
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && is_mount_point(&path) && !active_mounts.contains(&path) {
            active_mounts.insert(path.clone());
            spawn_import(
                path,
                photos_dir.to_path_buf(),
                index_dir.to_path_buf(),
                dedup_set.clone(),
                config.clone(),
                event_tx.clone(),
            );
        }
    }
}

/// Spawn the import thread for a detected mount and notify the state machine.
fn spawn_import(
    path: PathBuf,
    photos_dir: PathBuf,
    index_dir: PathBuf,
    dedup_set: Arc<Mutex<HashSet<u64>>>,
    config: Config,
    event_tx: Option<mpsc::Sender<MountEvent>>,
) {
    log::info!("USB mount detected: {}", path.display());
    if let Some(ref tx) = event_tx {
        let _ = tx.send(MountEvent::Inserted(path.clone()));
    }

    let import_tx = event_tx;
    std::thread::spawn(move || {
        // The mountpoint directory is created before the filesystem is actually
        // attached, so wait until something is mounted before scanning.
        wait_for_mount(&path);

        if let Err(e) = import_from_mount(&path, &photos_dir, &index_dir, dedup_set, &config) {
            log::error!("Import failed for {}: {}", path.display(), e);
        }
        log::info!("Import complete for {}", path.display());

        if let Some(tx) = import_tx {
            let _ = tx.send(MountEvent::ImportComplete(path));
        }
    });
}

/// Return true if `path` is a mount point, detected by comparing its device with
/// its parent's device. Used to avoid scanning an empty mountpoint directory
/// before the filesystem has been attached.
fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Some(parent) = path.parent() else {
        return false;
    };
    match (fs::metadata(path), fs::metadata(parent)) {
        (Ok(child), Ok(parent)) => child.dev() != parent.dev(),
        _ => false,
    }
}

/// Block until `path` is a mount point, or until `MOUNT_READY_TIMEOUT` elapses.
fn wait_for_mount(path: &Path) {
    const MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL_INTERVAL: Duration = Duration::from_millis(50);

    let deadline = Instant::now() + MOUNT_READY_TIMEOUT;
    while Instant::now() < deadline {
        if is_mount_point(path) {
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    log::warn!(
        "{} is not a mount point after {:?}; scanning anyway",
        path.display(),
        MOUNT_READY_TIMEOUT
    );
}

/// Import all JPEGs from a directory (USB mount or local folder).
pub fn import_from_directory(
    dir: &Path,
    photos_dir: &Path,
    index_dir: &Path,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
    config: &Config,
) -> io::Result<()> {
    // Resolve to an absolute path so downstream syscalls are not affected
    // by the process's current working directory.
    let abs_dir = dir.canonicalize()?;
    let images = find_images(&abs_dir);
    log::info!(
        "Found {} image file(s) under {}",
        images.len(),
        abs_dir.display()
    );
    let mut imported = 0;
    let mut skipped = 0;

    for photo_path in images {
        match import_single_photo(&photo_path, photos_dir, index_dir, dedup_set, config) {
            Ok(true) => imported += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                log::warn!("Failed to import {}: {}", photo_path.display(), e);
            }
        }
    }

    log::info!(
        "Import summary from {}: {} imported, {} skipped (duplicates)",
        abs_dir.display(),
        imported,
        skipped
    );
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
    import_from_directory(mount_point, photos_dir, index_dir, &dedup_set, config)
}

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "heif", "heifs", "heic", "heics"];

/// Names that only ever hold OS metadata and should never be imported. The
/// leading-dot rule covers macOS (`._*`, `.DS_Store`, `.fseventsd`, `.Trashes`,
/// `.Spotlight-V100`, ...); the rest are the Windows equivalents.
fn is_ignored_name(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("$RECYCLE.BIN")
        || name.eq_ignore_ascii_case("System Volume Information")
}

/// Find all image files under a directory, recursively.
fn find_images(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    collect_images(dir, &mut result);
    result
}

fn collect_images(dir: &Path, result: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            // Without this, an unreadable directory looks like an empty one.
            log::warn!("Failed to read directory {}: {}", dir.display(), e);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                log::warn!("Failed to read entry in {}: {}", dir.display(), e);
                continue;
            }
        };

        let path = entry.path();

        // Skip OS metadata: macOS AppleDouble sidecars (`._IMG_1234.jpeg`), and
        // hidden/system directories (`.fseventsd`, `.Trashes`, ...).
        if is_ignored_name(&entry.file_name().to_string_lossy()) {
            continue;
        }

        if path.is_dir() {
            collect_images(&path, result);
        } else if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if IMAGE_EXTENSIONS.contains(&ext.as_ref()) {
                result.push(path);
            }
        }
    }
}

/// Import a single photo. Returns Ok(true) if imported, Ok(false) if skipped (duplicate).
pub(crate) fn import_single_photo(
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
    let mtime = fs::metadata(src_path)?
        .modified()
        .unwrap_or(SystemTime::now());
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
                let (_new_meta, deleted) =
                    index::delete_oldest(index_dir, &meta, config.batch_delete_size)?;
                log::info!("Deleted {} old photos to free space", deleted);
                // Retry the conversion
                if let Err(e2) = convert_image(src_path, &dest_path, width, height, mode) {
                    return Err(io::Error::other(format!(
                        "Conversion failed after rotation: {}",
                        e2
                    )));
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
    let line_number = writer.append(&dest_path.to_string_lossy(), &original_name, hash)?;
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
    } else if Command::new("convert").arg("--version").output().is_ok() {
        "convert"
    } else {
        return Err(io::Error::other(
            "ImageMagick not found in PATH (tried 'magick' and 'convert')",
        ));
    };

    let mut cmd = Command::new(magick_cmd);
    cmd.arg(src);
    if matches!(mode, AspectRatioMode::Fill) {
        cmd.arg("-resize")
            .arg(format!("{}x{}^", width, height))
            .arg("-gravity")
            .arg("center")
            .arg("-extent")
            .arg(format!("{}x{}", width, height));
    } else {
        cmd.arg("-resize").arg(format!("{}x{}", width, height));
    }
    cmd.arg(dest);

    unsafe {
        cmd.pre_exec(|| {
            libc::nice(10);
            Ok(())
        });
    }

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!("ImageMagick failed: {}", stderr)));
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
    fn test_find_images() {
        let tmpdir = tempfile::tempdir().unwrap();
        File::create(tmpdir.path().join("photo1.jpg")).unwrap();
        File::create(tmpdir.path().join("photo2.JPEG")).unwrap();
        File::create(tmpdir.path().join("photo3.heif")).unwrap();
        File::create(tmpdir.path().join("photo4.HEIC")).unwrap();
        File::create(tmpdir.path().join("notaphoto.txt")).unwrap();

        let subdir = tmpdir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        File::create(subdir.join("nested.jpg")).unwrap();

        // OS metadata must be skipped, even when it carries an image extension.
        File::create(tmpdir.path().join("._photo5.jpeg")).unwrap();
        File::create(tmpdir.path().join(".DS_Store")).unwrap();
        for junk_dir in [
            ".fseventsd",
            ".Trashes",
            "$RECYCLE.BIN",
            "System Volume Information",
        ] {
            let dir = tmpdir.path().join(junk_dir);
            fs::create_dir(&dir).unwrap();
            File::create(dir.join("junk.jpg")).unwrap();
        }

        let images = find_images(tmpdir.path());
        assert_eq!(images.len(), 5);
        assert!(
            images.iter().all(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                !is_ignored_name(&name)
            }),
            "OS metadata files should be skipped: {images:?}"
        );
    }

    #[test]
    fn test_is_mount_point_for_plain_directory() {
        let tmpdir = tempfile::tempdir().unwrap();
        assert!(
            !is_mount_point(tmpdir.path()),
            "a fresh temp directory should not look like a mount point"
        );
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
