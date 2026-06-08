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

mod app;
mod config;
mod display;
mod import;
mod index;
mod logger;

use config::Config;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Acquire an exclusive PID lock at /tmp/photo-frame.lock.
/// Returns the lock file (must be kept alive for the lock to hold).
fn acquire_pid_lock() -> Result<std::fs::File, String> {
    let lock_path = std::path::Path::new("/tmp/photo-frame.lock");

    // Check for stale lock file from a previous (crashed) instance.
    if lock_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(lock_path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                let our_pid = std::process::id();
                if pid != our_pid {
                    let running = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
                    if running {
                        return Err(format!(
                            "Another instance of photo-frame-manager is already running (PID {})",
                            pid
                        ));
                    } else {
                        eprintln!("Removing stale lock file from PID {}", pid);
                        let _ = std::fs::remove_file(lock_path);
                    }
                }
            }
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(lock_path)
        .map_err(|e| format!("Failed to open lock file: {}", e))?;

    let pid = std::process::id();
    writeln!(file, "{}", pid).map_err(|e| format!("Failed to write PID: {}", e))?;

    let fd = file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "Another instance of photo-frame-manager is already running (lock file in use): {}",
            err
        ));
    }

    Ok(file)
}

fn print_help(name: &str) {
    println!("Digital photo frame manager for Raspberry Pi");
    println!();
    println!("Usage: {} [OPTIONS] <config.toml>", name);
    println!();
    println!("Arguments:");
    println!("  <config.toml>    Path to the TOML configuration file");
    println!();
    println!("Options:");
    println!("  --import-dir <dir>   Import photos from a local directory and exit");
    println!("  -h, --help           Print this help message and exit");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse optional flags
    let mut import_dir: Option<PathBuf> = None;
    let mut config_path_arg: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        if args[i] == "-h" || args[i] == "--help" {
            print_help(&args[0]);
            std::process::exit(0);
        } else if args[i] == "--import-dir" {
            if i + 1 >= args.len() {
                eprintln!("Error: --import-dir requires an argument");
                eprintln!("Usage: {} [OPTIONS] <config.toml>", args[0]);
                std::process::exit(1);
            }
            import_dir = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else if args[i].starts_with("-") {
            eprintln!("Error: unknown option {}", args[i]);
            eprintln!("Usage: {} [OPTIONS] <config.toml>", args[0]);
            std::process::exit(1);
        } else {
            config_path_arg = Some(args[i].clone());
            i += 1;
        }
    }

    let config_path = match config_path_arg {
        Some(p) => PathBuf::from(p),
        None => {
            print_help(&args[0]);
            std::process::exit(1);
        }
    };

    // Acquire PID lock before doing anything else
    let _lock_file = match acquire_pid_lock() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };
    let config = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config: {}", e);
            std::process::exit(1);
        }
    };

    // Initialize logger
    if let Err(e) = logger::TmpfsLogger::init(
        PathBuf::from("/tmp/photo-frame.log"),
        config.log_max_size,
        config.log_max_files,
    ) {
        eprintln!("Failed to initialize logger: {}", e);
        std::process::exit(1);
    }

    log::info!("Starting photo-frame-manager");
    log::info!("{}", config);

    // Ensure photos directory exists
    if let Err(e) = std::fs::create_dir_all(&config.photos_dir) {
        log::error!("Failed to create photos directory: {}", e);
        std::process::exit(1);
    }

    // Initialize or find index
    let (index_path, metadata) = match index::init_index(&config.photos_dir) {
        Ok(result) => result,
        Err(e) => {
            log::error!("Failed to initialize index: {}", e);
            std::process::exit(1);
        }
    };
    log::info!(
        "Index: {} (start_line={}, valid_count={})",
        index_path.display(),
        metadata.start_line,
        metadata.valid_count
    );

    // Compact index if ghost ratio > 50%
    let metadata = if metadata.ghost_ratio() > 0.5 {
        log::info!(
            "Compacting index (ghost ratio: {:.2})",
            metadata.ghost_ratio()
        );
        match index::compact_index(&config.photos_dir, &metadata) {
            Ok(new_meta) => new_meta,
            Err(e) => {
                log::error!("Failed to compact index: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        metadata
    };

    // Build deduplication set
    let dedup_set = match index::build_dedup_set(&index_path, &metadata) {
        Ok(set) => {
            log::info!("Loaded {} unique photo hashes", set.len());
            Arc::new(Mutex::new(set))
        }
        Err(e) => {
            log::error!("Failed to build dedup set: {}", e);
            std::process::exit(1);
        }
    };

    // Optional one-time import from a local directory
    if let Some(dir) = import_dir {
        let abs_dir = match dir.canonicalize() {
            Ok(d) => d,
            Err(e) => {
                log::error!(
                    "Failed to resolve import directory {}: {}",
                    dir.display(),
                    e
                );
                std::process::exit(1);
            }
        };
        if abs_dir.exists() && abs_dir.is_dir() {
            log::info!("Importing photos from: {}", abs_dir.display());
            if let Err(e) = import::import_from_directory(
                &abs_dir,
                &config.photos_dir,
                &config.photos_dir,
                &dedup_set,
                &config,
            ) {
                log::error!("Directory import failed: {}", e);
            }
        } else {
            log::error!(
                "Import directory does not exist or is not a directory: {}",
                abs_dir.display()
            );
            std::process::exit(1);
        }
    }

    // Shared shutdown flag
    let shutdown = Arc::new(AtomicBool::new(false));

    // Set up signal handling
    let mut signals = match signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ]) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to set up signal handler: {}", e);
            std::process::exit(1);
        }
    };

    // Spawn display thread
    let display_shutdown = shutdown.clone();
    let display_socket = config.socket_path.clone();
    let display_photos_dir = config.photos_dir.clone();
    let _display_handle = std::thread::spawn(move || {
        if let Err(e) =
            app::run_display_loop(&display_photos_dir, &display_socket, display_shutdown)
        {
            log::error!("Display loop error: {}", e);
        }
    });

    // Spawn USB watcher thread
    let usb_photos_dir = config.photos_dir.clone();
    let usb_index_dir = config.photos_dir.clone();
    let usb_dedup_set = dedup_set.clone();
    let usb_config = config.clone();
    let usb_shutdown = shutdown.clone();
    let _usb_handle = std::thread::spawn(move || {
        if let Err(e) = import::watch_usb_mounts(
            usb_photos_dir,
            usb_index_dir,
            usb_dedup_set,
            usb_config,
            usb_shutdown,
        ) {
            log::error!("USB watcher error: {}", e);
        }
    });

    // Wait for signal
    for sig in signals.forever() {
        match sig {
            signal_hook::consts::SIGTERM | signal_hook::consts::SIGINT => {
                log::info!("Received signal {}, shutting down", sig);
                shutdown.store(true, Ordering::Relaxed);
                break;
            }
            _ => {}
        }
    }

    // Give threads a brief moment to see the flag and clean up.
    // We do NOT join() because worker threads may be blocked in I/O
    // (socket reads, inotify waits) and could hang indefinitely.
    // The OS cleans up Unix sockets and file descriptors on process exit.
    std::thread::sleep(Duration::from_millis(200));

    log::info!("Shutdown complete");
}
