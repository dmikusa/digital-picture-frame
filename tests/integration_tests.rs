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

use photo_frame_manager::app;
use photo_frame_manager::config::{AspectRatioMode, Config};
use photo_frame_manager::import;
use photo_frame_manager::index;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn has_imagemagick() -> bool {
    std::process::Command::new("magick")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || std::process::Command::new("convert")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

fn make_config(photos_dir: PathBuf, socket_path: PathBuf) -> Config {
    Config {
        photos_dir,
        socket_path,
        native_resolution: "32x32".to_string(),
        aspect_ratio_mode: AspectRatioMode::Fit,
        batch_delete_size: 20,
        log_max_size: 262144,
        log_max_files: 2,
    }
}

/// A minimal 1x1 black-pixel JPEG that ImageMagick can process.
fn minimal_jpeg_bytes() -> Vec<u8> {
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x02, 0x02,
        0x02, 0x03, 0x02, 0x02, 0x02, 0x03, 0x03, 0x03, 0x03, 0x04, 0x06, 0x04, 0x04, 0x04, 0x04,
        0x04, 0x08, 0x06, 0x06, 0x05, 0x06, 0x09, 0x08, 0x0A, 0x0A, 0x09, 0x08, 0x09, 0x09, 0x0A,
        0x0C, 0x0F, 0x0C, 0x09, 0x0A, 0x0B, 0x0E, 0x0B, 0x09, 0x09, 0x0D, 0x11, 0x0D, 0x0E, 0x0F,
        0x10, 0x10, 0x11, 0x10, 0x0A, 0x0C, 0x12, 0x13, 0x12, 0x10, 0x13, 0x0F, 0x10, 0x10, 0x10,
        0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4,
        0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A,
        0x0B, 0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05,
        0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12,
        0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1,
        0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09,
        0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36,
        0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55,
        0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74,
        0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92,
        0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8,
        0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
        0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1,
        0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6,
        0xF7, 0xF8, 0xF9, 0xFA, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7B,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        0x80, 0x80, 0x80, 0x80, 0xFF, 0xD9,
    ]
}

fn create_index_with_entries(dir: &std::path::Path, entries: &[(&str, &str, u64)]) {
    let meta = index::IndexMetadata {
        start_line: 0,
        valid_count: entries.len(),
    };
    let filename = index::build_index_filename(&meta);
    let path = dir.join(&filename);
    let mut file = File::create(&path).unwrap();
    for (path_str, original_name, hash) in entries {
        writeln!(file, "{},{},{}", path_str, original_name, hash).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Display loop end-to-end: index → mock server receives IMG commands
// ---------------------------------------------------------------------------
#[test]
fn test_display_loop_streams_to_mock_server() {
    let tmpdir = tempfile::tempdir().unwrap();
    let socket_path = tmpdir.path().join("test.sock");
    let photos_dir = tmpdir.path().join("photos");
    fs::create_dir_all(&photos_dir).unwrap();

    let entries: &[(&str, &str, u64)] = &[
        ("/photos/00001_a.jpg", "a.jpg", 100),
        ("/photos/00002_b.jpg", "b.jpg", 200),
        ("/photos/00003_c.jpg", "c.jpg", 300),
        ("/photos/00004_d.jpg", "d.jpg", 400),
        ("/photos/00005_e.jpg", "e.jpg", 500),
    ];
    create_index_with_entries(&photos_dir, entries);

    let listener = UnixListener::bind(&socket_path).unwrap();
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let server_handle = thread::spawn(move || match listener.accept() {
        Ok((stream, _)) => {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                match line {
                    Ok(l) => received_clone.lock().unwrap().push(l),
                    Err(_) => break,
                }
            }
        }
        Err(_) => {}
    });

    // Give the server a moment to be ready
    thread::sleep(Duration::from_millis(50));

    let shutdown = Arc::new(AtomicBool::new(false));
    let display_shutdown = shutdown.clone();
    let display_photos = photos_dir.clone();
    let display_socket = socket_path.clone();

    let display_handle = thread::spawn(move || {
        let _ = app::run_display_loop(&display_photos, &display_socket, display_shutdown);
    });

    // Let it send a few images
    thread::sleep(Duration::from_millis(500));
    shutdown.store(true, Ordering::Relaxed);

    let _ = display_handle.join();
    let _ = server_handle.join();

    let msgs = received.lock().unwrap();
    assert!(
        !msgs.is_empty(),
        "Should have received at least one IMG command"
    );
    for msg in msgs.iter() {
        assert!(
            msg.starts_with("IMG "),
            "Message should start with 'IMG ': got '{}'",
            msg
        );
    }
}

// ---------------------------------------------------------------------------
// Display loop handles empty index gracefully
// ---------------------------------------------------------------------------
#[test]
fn test_display_loop_empty_index() {
    let tmpdir = tempfile::tempdir().unwrap();
    let socket_path = tmpdir.path().join("test.sock");
    let photos_dir = tmpdir.path().join("photos");
    fs::create_dir_all(&photos_dir).unwrap();

    // init_index creates an empty index-0-0.csv
    let _ = index::init_index(&photos_dir).unwrap();

    let _listener = UnixListener::bind(&socket_path).unwrap();

    let shutdown = Arc::new(AtomicBool::new(false));
    let display_shutdown = shutdown.clone();
    let display_photos = photos_dir.clone();
    let display_socket = socket_path.clone();

    let display_handle = thread::spawn(move || {
        let _ = app::run_display_loop(&display_photos, &display_socket, display_shutdown);
    });

    // The display loop should wait (sleep 5s) when the index is empty,
    // but we shut it down quickly so it shouldn't hang.
    thread::sleep(Duration::from_millis(200));
    shutdown.store(true, Ordering::Relaxed);

    let _ = display_handle.join();
}

// ---------------------------------------------------------------------------
// Display loop wraps at EOF and continues streaming
// ---------------------------------------------------------------------------
#[test]
fn test_display_loop_wraps_at_eof() {
    let tmpdir = tempfile::tempdir().unwrap();
    let socket_path = tmpdir.path().join("test.sock");
    let photos_dir = tmpdir.path().join("photos");
    fs::create_dir_all(&photos_dir).unwrap();

    let entries: &[(&str, &str, u64)] = &[
        ("/photos/00001_a.jpg", "a.jpg", 100),
        ("/photos/00002_b.jpg", "b.jpg", 200),
    ];
    create_index_with_entries(&photos_dir, entries);

    let listener = UnixListener::bind(&socket_path).unwrap();
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let server_handle = thread::spawn(move || match listener.accept() {
        Ok((stream, _)) => {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                match line {
                    Ok(l) => received_clone.lock().unwrap().push(l),
                    Err(_) => break,
                }
            }
        }
        Err(_) => {}
    });

    thread::sleep(Duration::from_millis(50));

    let shutdown = Arc::new(AtomicBool::new(false));
    let display_shutdown = shutdown.clone();
    let display_photos = photos_dir.clone();
    let display_socket = socket_path.clone();

    let display_handle = thread::spawn(move || {
        let _ = app::run_display_loop(&display_photos, &display_socket, display_shutdown);
    });

    // Wait long enough for the loop to wrap at least once (2 entries take
    // minimal time each, wrap should happen quickly).
    thread::sleep(Duration::from_millis(1000));
    shutdown.store(true, Ordering::Relaxed);

    let _ = display_handle.join();
    let _ = server_handle.join();

    let msgs = received.lock().unwrap();
    assert!(
        msgs.len() >= 2,
        "Should have received at least 2 IMG commands (got {})",
        msgs.len()
    );
    for msg in msgs.iter() {
        assert!(msg.starts_with("IMG "), "Bad message: '{}'", msg);
    }
}

// ---------------------------------------------------------------------------
// Import pipeline: scan JPEGs, convert, write to index
// ---------------------------------------------------------------------------
#[test]
fn test_import_pipeline() {
    if !has_imagemagick() {
        eprintln!("Skipping test_import_pipeline: ImageMagick not available");
        return;
    }

    let tmpdir = tempfile::tempdir().unwrap();
    let src_dir = tmpdir.path().join("src");
    let photos_dir = tmpdir.path().join("photos");
    let socket_path = tmpdir.path().join("unused.sock");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&photos_dir).unwrap();

    // Create two distinct minimal JPEGs (different content to avoid dedup)
    let jpeg = minimal_jpeg_bytes();
    fs::write(src_dir.join("photo1.jpg"), &jpeg).unwrap();
    let mut jpeg2 = jpeg.clone();
    jpeg2.push(0);
    fs::write(src_dir.join("photo2.jpeg"), &jpeg2).unwrap();

    let config = make_config(photos_dir.clone(), socket_path);
    let dedup_set = Arc::new(Mutex::new(HashSet::new()));

    import::import_from_directory(
        &src_dir.canonicalize().unwrap(),
        &config.photos_dir,
        &config.photos_dir,
        &dedup_set,
        &config,
    )
    .unwrap();

    // Verify index was created with two entries
    let (index_path, meta) = index::init_index(&photos_dir).unwrap();
    assert_eq!(
        meta.valid_count, 2,
        "Expected 2 valid entries, got {}",
        meta.valid_count
    );
    assert_eq!(meta.start_line, 0);

    let mut reader = index::IndexReader::open(&index_path, meta).unwrap();

    let rec1 = reader.next_record().unwrap().expect("first record");
    assert!(
        rec1.original_name == "photo1.jpg" || rec1.original_name == "photo2.jpeg",
        "Unexpected original name: {}",
        rec1.original_name
    );
    assert!(rec1.path.ends_with(&rec1.original_name));

    let rec2 = reader.next_record().unwrap().expect("second record");
    assert!(
        rec2.original_name == "photo1.jpg" || rec2.original_name == "photo2.jpeg",
        "Unexpected original name: {}",
        rec2.original_name
    );
    assert!(rec2.path.ends_with(&rec2.original_name));
    assert_ne!(rec1.original_name, rec2.original_name);

    assert!(reader.next_record().unwrap().is_none());

    // Verify the converted files exist on disk
    assert!(
        std::path::Path::new(&rec1.path).exists(),
        "Converted file should exist: {}",
        rec1.path
    );
    assert!(
        std::path::Path::new(&rec2.path).exists(),
        "Converted file should exist: {}",
        rec2.path
    );
}

// ---------------------------------------------------------------------------
// Full pipeline: import then display loop streams the imported paths
// ---------------------------------------------------------------------------
#[test]
fn test_full_pipeline_import_then_display() {
    if !has_imagemagick() {
        eprintln!("Skipping test_full_pipeline_import_then_display: ImageMagick not available");
        return;
    }

    let tmpdir = tempfile::tempdir().unwrap();
    let src_dir = tmpdir.path().join("src");
    let photos_dir = tmpdir.path().join("photos");
    let socket_path = tmpdir.path().join("full.sock");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&photos_dir).unwrap();

    let jpeg = minimal_jpeg_bytes();
    fs::write(src_dir.join("photo_a.jpg"), &jpeg).unwrap();
    let mut jpeg2 = jpeg.clone();
    jpeg2.push(0);
    fs::write(src_dir.join("photo_b.jpg"), &jpeg2).unwrap();

    let config = make_config(photos_dir.clone(), socket_path.clone());
    let dedup_set = Arc::new(Mutex::new(HashSet::new()));

    import::import_from_directory(
        &src_dir.canonicalize().unwrap(),
        &config.photos_dir,
        &config.photos_dir,
        &dedup_set,
        &config,
    )
    .unwrap();

    // Now start the display loop and verify the mock server receives IMG
    // commands pointing to the imported files.
    let listener = UnixListener::bind(&socket_path).unwrap();
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let server_handle = thread::spawn(move || match listener.accept() {
        Ok((stream, _)) => {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                match line {
                    Ok(l) => received_clone.lock().unwrap().push(l),
                    Err(_) => break,
                }
            }
        }
        Err(_) => {}
    });

    thread::sleep(Duration::from_millis(50));

    let shutdown = Arc::new(AtomicBool::new(false));
    let display_shutdown = shutdown.clone();
    let display_photos = photos_dir.clone();
    let display_socket = socket_path.clone();

    let display_handle = thread::spawn(move || {
        let _ = app::run_display_loop(&display_photos, &display_socket, display_shutdown);
    });

    thread::sleep(Duration::from_millis(500));
    shutdown.store(true, Ordering::Relaxed);

    let _ = display_handle.join();
    let _ = server_handle.join();

    let msgs = received.lock().unwrap();
    assert!(!msgs.is_empty(), "Should have received IMG commands");
    for msg in msgs.iter() {
        assert!(
            msg.starts_with("IMG "),
            "Expected IMG command, got: '{}'",
            msg
        );
        // Each IMG command should reference a path under our photos dir
        let path_part = msg.strip_prefix("IMG ").unwrap_or(msg);
        assert!(
            path_part.starts_with(photos_dir.to_str().unwrap()),
            "IMG path should be under photos dir: '{}'",
            msg
        );
    }
}
