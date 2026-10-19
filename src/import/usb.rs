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
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Serializes photo imports across the whole process. USB mounts spawn import
/// threads while the remote sync scheduler imports on its own thread; both
/// mutate the same index and dedup set. `IndexWriter` captures metadata when it
/// is opened and `sync_metadata` renames the file based on that snapshot, so two
/// concurrent imports can rename over each other and lose entries. The
/// check-then-insert on the dedup set is racy for the same reason.
static IMPORT_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the process-wide import lock.
///
/// A panic in an earlier import must not wedge imports forever, so a poisoned
/// lock is recovered instead of propagating the panic.
fn lock_imports() -> std::sync::MutexGuard<'static, ()> {
    IMPORT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

    // Mount points we have already started an import for, and when we first saw
    // them. The timestamp distinguishes a mount that is still settling from one
    // that has gone away.
    let mut active_mounts: HashMap<PathBuf, Instant> = HashMap::new();

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

    // Reconcile periodically. A manual `mount`/`umount`, or a re-mount over an
    // existing mountpoint directory, does not emit an inotify event under
    // /media, so polling is the only reliable way to notice it.
    const SWEEP_INTERVAL: Duration = Duration::from_secs(2);
    let mut last_sweep = Instant::now();

    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            log::info!("USB watcher shutting down");
            break;
        }

        if last_sweep.elapsed() >= SWEEP_INTERVAL {
            sweep_mounts(
                &mut active_mounts,
                &photos_dir,
                &index_dir,
                &dedup_set,
                &config,
                &event_tx,
            );
            last_sweep = Instant::now();
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => match event.kind {
                notify::EventKind::Create(notify::event::CreateKind::Folder) => {
                    for path in event.paths {
                        if path.is_dir() && !active_mounts.contains_key(&path) {
                            active_mounts.insert(path.clone(), Instant::now());
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

/// Reconcile `active_mounts` with what is actually mounted under `/media`:
/// forget mounts that have gone away and start imports for new ones. Covers
/// drives already mounted at startup, manual `mount`/`umount`, and re-mounts
/// over an existing mountpoint directory (none of which emit a Create event).
fn sweep_mounts(
    active_mounts: &mut HashMap<PathBuf, Instant>,
    photos_dir: &Path,
    index_dir: &Path,
    dedup_set: &Arc<Mutex<HashSet<u64>>>,
    config: &Config,
    event_tx: &Option<mpsc::Sender<MountEvent>>,
) {
    // Forget mounts that are no longer present. Ignore paths seen very recently
    // so we don't race a mount that is still being attached.
    const STALE_AFTER: Duration = Duration::from_secs(8);
    let stale: Vec<PathBuf> = active_mounts
        .iter()
        .filter(|(path, since)| since.elapsed() >= STALE_AFTER && !is_mount_point(path))
        .map(|(path, _)| path.clone())
        .collect();
    for path in stale {
        active_mounts.remove(&path);
        log::info!("USB unmounted: {}", path.display());
        if let Some(ref tx) = event_tx {
            let _ = tx.send(MountEvent::Removed(path));
        }
    }

    let entries = match fs::read_dir("/media") {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Failed to scan /media for mounts: {}", e);
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && is_mount_point(&path) && !active_mounts.contains_key(&path) {
            active_mounts.insert(path.clone(), Instant::now());
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
    let scan = scan_images(&abs_dir);
    log::info!(
        "Found {} image file(s) under {} ({} skipped: not images or OS metadata)",
        scan.images.len(),
        abs_dir.display(),
        scan.skipped
    );
    let skipped = scan.skipped;
    let mut imported = 0;
    let mut duplicates = 0;
    let mut failed = 0;

    for photo_path in scan.images {
        match import_single_photo(&photo_path, photos_dir, index_dir, dedup_set, config) {
            Ok(true) => imported += 1,
            Ok(false) => duplicates += 1,
            Err(e) => {
                failed += 1;
                log::warn!("Failed to import {}: {}", photo_path.display(), e);
            }
        }
    }

    log::info!(
        "Import summary from {}: {} imported, {} duplicates, {} failed, {} skipped (non-images)",
        abs_dir.display(),
        imported,
        duplicates,
        failed,
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

/// Result of scanning a directory tree for importable images.
struct ScanResult {
    /// Image files that will be imported.
    images: Vec<PathBuf>,
    /// Entries that were skipped: OS metadata and files that are not images.
    skipped: usize,
}

/// Find all image files under a directory, recursively, along with a count of
/// the entries that were skipped.
fn scan_images(dir: &Path) -> ScanResult {
    let mut result = ScanResult {
        images: Vec::new(),
        skipped: 0,
    };
    collect_images(dir, &mut result);
    result
}

fn collect_images(dir: &Path, result: &mut ScanResult) {
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
            result.skipped += 1;
            continue;
        }

        if path.is_dir() {
            collect_images(&path, result);
        } else if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if IMAGE_EXTENSIONS.contains(&ext.as_ref()) {
                result.images.push(path);
            } else {
                result.skipped += 1;
            }
        } else {
            // A file with no extension.
            result.skipped += 1;
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
    // Hold the import lock for the whole operation: dedup check, conversion,
    // rotation, index append, and dedup insert. Scanning happens before this is
    // called, so the lock is only held per photo.
    let _import_guard = lock_imports();

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

    // Convert and copy. If the photos partition is full, delete the oldest
    // photos and retry until the conversion succeeds or there is nothing left
    // to delete.
    let (width, height) = config.resolution();
    let mode = &config.aspect_ratio_mode;
    convert_with_rotation(
        || convert_image(src_path, &dest_path, width, height, mode),
        index_dir,
        config.batch_delete_size,
    )?;

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

    let mut dest = photos_dir
        .join(year)
        .join(month)
        .join(day)
        .join(format!("{}_{}", seq_str, original_name));

    // ImageMagick picks the output format from the destination extension, so a
    // HEIC/HEIF source would otherwise be re-encoded as HEIC. The display app's
    // stb_image decoder cannot read HEIC, so normalize every import to JPEG. The
    // original filename (extension included) is kept separately in the index.
    dest.set_extension("jpg");
    dest
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
        let lower = stderr.to_lowercase();
        // ImageMagick reports write failures on stdout/stderr with a human
        // readable errno string, not a Rust io::Error, so translate it here.
        if lower.contains("no space left on device") || lower.contains("disk quota exceeded") {
            log::warn!(
                "ImageMagick could not write {}: {}",
                dest.display(),
                stderr.trim()
            );
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        return Err(io::Error::other(format!("ImageMagick failed: {}", stderr)));
    }

    Ok(())
}

/// True when an error means the photos partition is out of space.
fn is_enospc(e: &io::Error) -> bool {
    if e.raw_os_error() == Some(libc::ENOSPC) {
        return true;
    }

    // Some failure paths only preserve the message (for example a shelled-out
    // tool wrapped with `io::Error::other`), so check that too.
    let message = e.to_string().to_lowercase();
    message.contains("no space left on device") || message.contains("disk quota exceeded")
}

/// Run `convert`, rotating out the oldest photos whenever it fails with ENOSPC.
///
/// Returns the number of photos deleted. Rotation always makes progress or
/// errors out, so this cannot loop forever: each pass consumes valid index
/// entries, and we stop as soon as the index or the filesystem is empty.
fn convert_with_rotation<F>(
    mut convert: F,
    index_dir: &Path,
    batch_delete_size: usize,
) -> io::Result<usize>
where
    F: FnMut() -> io::Result<()>,
{
    let mut deleted_total = 0;

    loop {
        match convert() {
            Ok(()) => return Ok(deleted_total),
            Err(e) if !is_enospc(&e) => return Err(e),
            Err(e) => {
                log::warn!("Disk full while converting ({}), attempting rotation", e);

                let (_index_path, meta) = index::init_index(index_dir)?;
                if meta.valid_count == 0 {
                    return Err(io::Error::other(format!(
                        "Conversion failed and there are no photos left to delete: {}",
                        e
                    )));
                }

                let (_new_meta, deleted) =
                    index::delete_oldest(index_dir, &meta, batch_delete_size)?;
                log::info!("Deleted {} old photos to free space", deleted);
                if deleted == 0 {
                    return Err(io::Error::other(format!(
                        "Conversion failed and rotation freed no space: {}",
                        e
                    )));
                }

                deleted_total += deleted;
            }
        }
    }
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
    fn test_scan_images() {
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

        let scan = scan_images(tmpdir.path());
        assert_eq!(scan.images.len(), 5);
        assert!(
            scan.images.iter().all(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                !is_ignored_name(&name)
            }),
            "OS metadata files should be skipped: {:?}",
            scan.images
        );
        // notaphoto.txt, ._photo5.jpeg, .DS_Store, and the four ignored dirs.
        assert_eq!(scan.skipped, 7);
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
    fn test_is_enospc_recognizes_raw_os_error_and_message() {
        assert!(is_enospc(&io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(is_enospc(&io::Error::other(
            "ImageMagick failed: unable to write image: No space left on device"
        )));
        assert!(is_enospc(&io::Error::other(
            "write failed: Disk quota exceeded"
        )));
        assert!(!is_enospc(&io::Error::other(
            "ImageMagick failed: corrupt image"
        )));
        assert!(!is_enospc(&io::Error::from_raw_os_error(libc::EACCES)));
    }

    /// Create `count` placeholder photos plus an index that references them.
    fn create_index_with_photos(dir: &Path, count: usize) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for i in 0..count {
            let path = dir.join(format!("{i:05}_photo.jpg"));
            File::create(&path).unwrap();
            paths.push(path);
        }

        let meta = index::IndexMetadata {
            start_line: 0,
            valid_count: count,
        };
        let mut file = File::create(dir.join(index::build_index_filename(&meta))).unwrap();
        for (i, path) in paths.iter().enumerate() {
            writeln!(file, "{},photo{}.jpg,{}", path.display(), i, i + 1).unwrap();
        }
        file.flush().unwrap();

        paths
    }

    #[test]
    fn test_convert_with_rotation_deletes_oldest_and_retries() {
        let tmpdir = tempfile::tempdir().unwrap();
        let photos = create_index_with_photos(tmpdir.path(), 5);
        let mut calls = 0;

        let deleted = convert_with_rotation(
            || {
                calls += 1;
                if calls == 1 {
                    Err(io::Error::from_raw_os_error(libc::ENOSPC))
                } else {
                    Ok(())
                }
            },
            tmpdir.path(),
            2,
        )
        .unwrap();

        assert_eq!(calls, 2, "conversion should be retried after rotation");
        assert_eq!(deleted, 2);
        assert!(!photos[0].exists(), "oldest photo should be deleted");
        assert!(!photos[1].exists(), "second oldest photo should be deleted");
        for photo in &photos[2..] {
            assert!(photo.exists(), "newer photo should be kept: {photo:?}");
        }

        let (_path, meta) = index::init_index(tmpdir.path()).unwrap();
        assert_eq!(meta.start_line, 2);
        assert_eq!(meta.valid_count, 3);
    }

    #[test]
    fn test_convert_with_rotation_errors_when_no_photos_remain() {
        let tmpdir = tempfile::tempdir().unwrap();
        let photos = create_index_with_photos(tmpdir.path(), 2);

        let result = convert_with_rotation(
            || Err(io::Error::from_raw_os_error(libc::ENOSPC)),
            tmpdir.path(),
            5,
        );

        assert!(result.is_err(), "should give up when nothing is left");
        assert!(photos.iter().all(|photo| !photo.exists()));

        let (_path, meta) = index::init_index(tmpdir.path()).unwrap();
        assert_eq!(meta.valid_count, 0);
    }

    #[test]
    fn test_convert_with_rotation_errors_with_empty_index() {
        let tmpdir = tempfile::tempdir().unwrap();
        index::init_index(tmpdir.path()).unwrap();

        let result = convert_with_rotation(
            || Err(io::Error::from_raw_os_error(libc::ENOSPC)),
            tmpdir.path(),
            20,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_convert_with_rotation_propagates_non_enospc_errors() {
        let tmpdir = tempfile::tempdir().unwrap();
        let photos = create_index_with_photos(tmpdir.path(), 2);
        let mut calls = 0;

        let result = convert_with_rotation(
            || {
                calls += 1;
                Err(io::Error::other("ImageMagick failed: corrupt image"))
            },
            tmpdir.path(),
            1,
        );

        assert!(result.is_err());
        assert_eq!(calls, 1, "non-ENOSPC errors must not trigger rotation");
        assert!(photos.iter().all(|photo| photo.exists()));
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

    #[test]
    fn test_build_dest_path_normalizes_to_jpg() {
        let photos_dir = PathBuf::from("/photos");
        let mtime = UNIX_EPOCH + Duration::from_secs(1609459200); // 2021-01-01

        for src in [
            "/usb/photo.HEIC",
            "/usb/photo.heif",
            "/usb/photo.png",
            "/usb/photo",
        ] {
            let dest = build_dest_path(Path::new(src), &photos_dir, mtime);
            assert_eq!(
                dest.extension().and_then(|ext| ext.to_str()),
                Some("jpg"),
                "source {src} should convert to JPEG, got {}",
                dest.display()
            );
            assert!(
                dest.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("photo"),
                "source {src} should keep its name, got {}",
                dest.display()
            );
        }
    }
}
