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

use chrono::Utc;
use flate2::write::GzEncoder;
use flate2::Compression;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct TmpfsLogger {
    log_path: PathBuf,
    max_size: usize,
    max_files: usize,
    state: Mutex<LoggerState>,
}

struct LoggerState {
    current_size: usize,
}

impl TmpfsLogger {
    pub fn new(log_path: PathBuf, max_size: usize, max_files: usize) -> io::Result<Self> {
        let current_size = if log_path.exists() {
            fs::metadata(&log_path)?.len() as usize
        } else {
            0
        };

        Ok(TmpfsLogger {
            log_path,
            max_size,
            max_files,
            state: Mutex::new(LoggerState { current_size }),
        })
    }

    pub fn init(log_path: PathBuf, max_size: usize, max_files: usize) -> Result<(), String> {
        let logger = Self::new(log_path, max_size, max_files)
            .map_err(|e| format!("Failed to create logger: {}", e))?;
        log::set_boxed_logger(Box::new(logger))
            .map_err(|e| format!("Failed to set logger: {}", e))?;
        log::set_max_level(LevelFilter::Info);
        Ok(())
    }

    fn rotate(&self) -> io::Result<()> {
        // Delete the oldest file if it exists
        let oldest = format!("{}.{}.gz", self.log_path.display(), self.max_files);
        let oldest_path = PathBuf::from(&oldest);
        if oldest_path.exists() {
            fs::remove_file(&oldest_path)?;
        }

        // Shift existing files: .1.gz -> .2.gz, etc.
        for i in (1..self.max_files).rev() {
            let src = format!("{}.{}.gz", self.log_path.display(), i);
            let dst = format!("{}.{}.gz", self.log_path.display(), i + 1);
            let src_path = PathBuf::from(&src);
            if src_path.exists() {
                fs::rename(&src_path, &dst)?;
            }
        }

        // Compress current log to .1.gz
        let current_data = fs::read(&self.log_path)?;
        let gz_path = format!("{}.1.gz", self.log_path.display());
        let gz_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&gz_path)?;
        let mut encoder = GzEncoder::new(gz_file, Compression::default());
        encoder.write_all(&current_data)?;
        encoder.finish()?;

        // Truncate current log
        fs::remove_file(&self.log_path)?;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.log_path)?;

        Ok(())
    }

    fn write_log(&self, record: &Record) -> io::Result<()> {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let level = record.level();
        let message = record.args();
        let line = format!("{} {} {}\n", timestamp, level, message);
        let line_bytes = line.as_bytes();
        let line_len = line_bytes.len();

        let mut state = self.state.lock().unwrap();

        if state.current_size + line_len > self.max_size && state.current_size > 0 {
            drop(state); // release lock before rotate
            self.rotate()?;
            state = self.state.lock().unwrap();
            state.current_size = 0;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        file.write_all(line_bytes)?;
        file.flush()?;

        state.current_size += line_len;

        Ok(())
    }
}

impl Log for TmpfsLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            if let Err(e) = self.write_log(record) {
                eprintln!("Logger error: {}", e);
            }
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::RecordBuilder;
    use std::sync::Arc;
    use std::thread;

    macro_rules! log_record {
        ($msg:expr) => {{
            RecordBuilder::new()
                .args(format_args!("{}", $msg))
                .level(Level::Info)
                .target("test")
                .build()
        }}
    }

    #[test]
    fn test_logger_basic() {
        let tmpdir = tempfile::tempdir().unwrap();
        let log_path = tmpdir.path().join("test.log");
        let logger = TmpfsLogger::new(log_path.clone(), 1024, 2).unwrap();

        logger.log(&log_record!("Test message 1"));
        logger.log(&log_record!("Test message 2"));

        let contents = fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("Test message 1"));
        assert!(contents.contains("Test message 2"));
    }

    #[test]
    fn test_logger_rotation() {
        let tmpdir = tempfile::tempdir().unwrap();
        let log_path = tmpdir.path().join("test.log");
        let logger = TmpfsLogger::new(log_path.clone(), 50, 2).unwrap();

        // Write enough to trigger rotation
        for i in 0..10 {
            logger.log(&log_record!(format!("Message {}", i)));
        }

        // Check that rotation happened
        let gz_path = format!("{}.1.gz", log_path.display());
        assert!(PathBuf::from(&gz_path).exists());
    }

    #[test]
    fn test_logger_thread_safety() {
        let tmpdir = tempfile::tempdir().unwrap();
        let log_path = tmpdir.path().join("test.log");
        let logger = Arc::new(TmpfsLogger::new(log_path.clone(), 4096, 2).unwrap());

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let logger = logger.clone();
                thread::spawn(move || {
                    for j in 0..10 {
                        logger.log(&log_record!(format!("Thread {} message {}", i, j)));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let contents = fs::read_to_string(&log_path).unwrap();
        let count = contents.lines().count();
        assert_eq!(count, 40);
    }
}
