use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// A record in the photo index CSV.
/// Format: path,original_name,hash
#[derive(Debug, Clone, PartialEq)]
pub struct PhotoRecord {
    pub path: String,
    pub original_name: String,
    pub hash: u64,
    pub line_number: usize,
}

/// Parsed metadata from an index filename like `index-0-150.csv`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IndexMetadata {
    pub start_line: usize,
    pub valid_count: usize,
}

impl IndexMetadata {
    pub fn total_lines(&self) -> usize {
        self.start_line + self.valid_count
    }

    pub fn ghost_ratio(&self) -> f64 {
        if self.total_lines() == 0 {
            0.0
        } else {
            self.start_line as f64 / self.total_lines() as f64
        }
    }
}

/// Finds the index file in the given directory.
/// Returns the path and parsed metadata.
pub fn find_index_file(dir: &Path) -> Option<(PathBuf, IndexMetadata)> {
    let entries = fs::read_dir(dir).ok()?;
    let mut candidates = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(meta) = parse_index_filename(&name) {
            candidates.push((entry.path(), meta));
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // If multiple exist, prefer the one with the most recent mtime
    candidates.sort_by_key(|(path, _)| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
    });
    candidates.pop()
}

/// Parse a filename like `index-0-150.csv` into metadata.
pub fn parse_index_filename(name: &str) -> Option<IndexMetadata> {
    let name = name.strip_suffix(".csv")?;
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 3 || parts[0] != "index" {
        return None;
    }
    let start_line = parts[1].parse().ok()?;
    let valid_count = parts[2].parse().ok()?;
    Some(IndexMetadata {
        start_line,
        valid_count,
    })
}

/// Build the index filename from metadata.
pub fn build_index_filename(meta: &IndexMetadata) -> String {
    format!("index-{}-{}.csv", meta.start_line, meta.valid_count)
}

/// Reads the CSV index file line-by-line, skipping ghost entries.
pub struct IndexReader {
    reader: BufReader<File>,
    path: PathBuf,
    metadata: IndexMetadata,
    current_line: usize,
    current_valid: usize,
}

impl IndexReader {
    pub fn open(path: &Path, metadata: IndexMetadata) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // Skip ghost lines by reading and discarding them
        for _ in 0..metadata.start_line {
            let mut buf = String::new();
            if reader.read_line(&mut buf)? == 0 {
                break;
            }
        }

        Ok(IndexReader {
            reader,
            path: path.to_path_buf(),
            metadata,
            current_line: metadata.start_line,
            current_valid: 0,
        })
    }

    /// Seek to a specific line number (0-indexed, global line number).
    /// If the line is before start_line, wraps to start_line.
    pub fn seek_to(&mut self, line_number: usize) -> io::Result<()> {
        let target = if line_number < self.metadata.start_line {
            self.metadata.start_line
        } else {
            line_number
        };

        // Reopen and seek
        let file = File::open(&self.path)?;
        let mut reader = BufReader::new(file);

        // Skip lines until we reach target
        for _ in 0..target {
            let mut buf = String::new();
            if reader.read_line(&mut buf)? == 0 {
                break;
            }
        }

        self.reader = reader;
        self.current_line = target;
        self.current_valid = target.saturating_sub(self.metadata.start_line);

        Ok(())
    }

    pub fn next_record(&mut self) -> io::Result<Option<PhotoRecord>> {
        if self.current_valid >= self.metadata.valid_count {
            return Ok(None);
        }

        let mut line = String::new();
        let bytes_read = self.reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Ok(None);
        }

        let line = line.trim_end();
        let record = parse_csv_line(line, self.current_line);
        self.current_line += 1;
        self.current_valid += 1;

        Ok(record)
    }

    #[allow(dead_code)]
    pub fn metadata(&self) -> &IndexMetadata {
        &self.metadata
    }

    pub fn current_line(&self) -> usize {
        self.current_line
    }
}

/// Appends records to the CSV index file.
pub struct IndexWriter {
    file: File,
    metadata: IndexMetadata,
    dir: PathBuf,
}

impl IndexWriter {
    pub fn open(dir: &Path, metadata: IndexMetadata) -> io::Result<Self> {
        let filename = build_index_filename(&metadata);
        let path = dir.join(&filename);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(IndexWriter {
            file,
            metadata,
            dir: dir.to_path_buf(),
        })
    }

    pub fn append(&mut self, path: &str, original_name: &str, hash: u64) -> io::Result<usize> {
        let line_number = self.metadata.total_lines();
        let hash_str = hash.to_string();
        let line = format!("{},{},{}\n", path, original_name, hash_str);
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        self.metadata.valid_count += 1;
        Ok(line_number)
    }

    #[allow(dead_code)]
    pub fn metadata(&self) -> &IndexMetadata {
        &self.metadata
    }

    /// Atomically rename the index file to reflect updated metadata.
    pub fn sync_metadata(&mut self) -> io::Result<()> {
        let old_name = build_index_filename(&IndexMetadata {
            start_line: self.metadata.start_line,
            valid_count: self.metadata.valid_count - 1, // before the last append
        });
        let new_name = build_index_filename(&self.metadata);
        let old_path = self.dir.join(&old_name);
        let new_path = self.dir.join(&new_name);

        // Only rename if the filename would actually change
        if old_name != new_name && old_path.exists() {
            fs::rename(&old_path, &new_path)?;
            // Reopen the file at the new path
            self.file = OpenOptions::new().append(true).open(&new_path)?;
        }

        Ok(())
    }
}

/// Parse a single CSV line into a PhotoRecord.
fn parse_csv_line(line: &str, line_number: usize) -> Option<PhotoRecord> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() != 3 {
        return None;
    }
    let hash = parts[2].parse().ok()?;
    Some(PhotoRecord {
        path: parts[0].to_string(),
        original_name: parts[1].to_string(),
        hash,
        line_number,
    })
}

/// Scan the entire index file and build a HashSet of hashes for deduplication.
pub fn build_dedup_set(path: &Path, metadata: &IndexMetadata) -> io::Result<HashSet<u64>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut set = HashSet::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        if line_number >= metadata.start_line {
            if let Some(record) = parse_csv_line(&line, line_number) {
                set.insert(record.hash);
            }
        }
    }

    Ok(set)
}

/// Compact the index file by removing ghost entries.
/// Returns the new metadata.
pub fn compact_index(dir: &Path, metadata: &IndexMetadata) -> io::Result<IndexMetadata> {
    let old_name = build_index_filename(metadata);
    let old_path = dir.join(&old_name);

    let new_name = format!("index-{}-{}.csv.tmp", 0, metadata.valid_count);
    let new_path = dir.join(&new_name);

    {
        let old_file = File::open(&old_path)?;
        let old_reader = BufReader::new(old_file);
        let mut new_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&new_path)?;

        for (line_number, line) in old_reader.lines().enumerate() {
            let line = line?;
            if line_number >= metadata.start_line {
                writeln!(new_file, "{}", line)?;
            }
        }
        new_file.flush()?;
    }

    // Atomically rename
    let final_name = build_index_filename(&IndexMetadata {
        start_line: 0,
        valid_count: metadata.valid_count,
    });
    let final_path = dir.join(&final_name);
    fs::rename(&new_path, &final_path)?;

    // Remove old file if it's still there and different from final
    if old_path != final_path && old_path.exists() {
        fs::remove_file(&old_path)?;
    }

    Ok(IndexMetadata {
        start_line: 0,
        valid_count: metadata.valid_count,
    })
}

/// Delete the oldest `count` photos and update metadata.
/// Returns the new metadata and the number of files actually deleted.
pub fn delete_oldest(dir: &Path, metadata: &IndexMetadata, count: usize) -> io::Result<(IndexMetadata, usize)> {
    let old_name = build_index_filename(metadata);
    let old_path = dir.join(&old_name);

    let to_delete = count.min(metadata.valid_count);
    let new_start = metadata.start_line + to_delete;
    let new_valid = metadata.valid_count - to_delete;

    // Read the first `to_delete` valid lines and delete their files
    let file = File::open(&old_path)?;
    let reader = BufReader::new(file);
    let mut deleted = 0;

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        if line_number >= metadata.start_line && line_number < new_start {
            if let Some(record) = parse_csv_line(&line, line_number) {
                let path = PathBuf::from(&record.path);
                if path.exists() {
                    if let Err(e) = fs::remove_file(&path) {
                        log::warn!("Failed to delete {}: {}", path.display(), e);
                    } else {
                        deleted += 1;
                    }
                }
            }
        }
    }

    let new_metadata = IndexMetadata {
        start_line: new_start,
        valid_count: new_valid,
    };

    // Rename the file to reflect new metadata
    let new_name = build_index_filename(&new_metadata);
    let new_path = dir.join(&new_name);
    if old_path != new_path {
        fs::rename(&old_path, &new_path)?;
    }

    Ok((new_metadata, deleted))
}

/// Initialize or open an existing index in the given directory.
/// If no index exists, creates `index-0-0.csv`.
pub fn init_index(dir: &Path) -> io::Result<(PathBuf, IndexMetadata)> {
    if let Some((path, meta)) = find_index_file(dir) {
        return Ok((path, meta));
    }

    let meta = IndexMetadata {
        start_line: 0,
        valid_count: 0,
    };
    let filename = build_index_filename(&meta);
    let path = dir.join(&filename);
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    Ok((path, meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_index_filename() {
        assert_eq!(
            parse_index_filename("index-0-150.csv"),
            Some(IndexMetadata {
                start_line: 0,
                valid_count: 150
            })
        );
        assert_eq!(
            parse_index_filename("index-10-50.csv"),
            Some(IndexMetadata {
                start_line: 10,
                valid_count: 50
            })
        );
        assert_eq!(parse_index_filename("other-0-150.csv"), None);
        assert_eq!(parse_index_filename("index-0-150.txt"), None);
    }

    #[test]
    fn test_build_index_filename() {
        assert_eq!(
            build_index_filename(&IndexMetadata {
                start_line: 0,
                valid_count: 150
            }),
            "index-0-150.csv"
        );
    }

    #[test]
    fn test_ghost_ratio() {
        let meta = IndexMetadata {
            start_line: 50,
            valid_count: 50,
        };
        assert_eq!(meta.ghost_ratio(), 0.5);
    }

    #[test]
    fn test_index_reader() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("index-0-3.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "/photos/00001_test.jpg,test.jpg,12345").unwrap();
        writeln!(file, "/photos/00002_foo.jpg,foo.jpg,67890").unwrap();
        writeln!(file, "/photos/00003_bar.jpg,bar.jpg,11111").unwrap();

        let meta = IndexMetadata {
            start_line: 0,
            valid_count: 3,
        };
        let mut reader = IndexReader::open(&path, meta).unwrap();

        let rec1 = reader.next_record().unwrap().unwrap();
        assert_eq!(rec1.path, "/photos/00001_test.jpg");
        assert_eq!(rec1.line_number, 0);

        let rec2 = reader.next_record().unwrap().unwrap();
        assert_eq!(rec2.original_name, "foo.jpg");

        let rec3 = reader.next_record().unwrap().unwrap();
        assert_eq!(rec3.hash, 11111);

        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn test_index_reader_with_ghosts() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("index-2-2.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "ghost1,old.jpg,1").unwrap();
        writeln!(file, "ghost2,older.jpg,2").unwrap();
        writeln!(file, "/photos/00003_valid.jpg,valid.jpg,3").unwrap();
        writeln!(file, "/photos/00004_valid2.jpg,valid2.jpg,4").unwrap();

        let meta = IndexMetadata {
            start_line: 2,
            valid_count: 2,
        };
        let mut reader = IndexReader::open(&path, meta).unwrap();

        let rec1 = reader.next_record().unwrap().unwrap();
        assert_eq!(rec1.path, "/photos/00003_valid.jpg");
        assert_eq!(rec1.line_number, 2);

        let rec2 = reader.next_record().unwrap().unwrap();
        assert_eq!(rec2.path, "/photos/00004_valid2.jpg");

        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn test_index_writer() {
        let tmpdir = tempfile::tempdir().unwrap();
        let meta = IndexMetadata {
            start_line: 0,
            valid_count: 0,
        };
        let mut writer = IndexWriter::open(tmpdir.path(), meta).unwrap();
        writer.append("/photos/00001_a.jpg", "a.jpg", 100).unwrap();
        writer.append("/photos/00002_b.jpg", "b.jpg", 200).unwrap();
        drop(writer);

        // File remains with original name since we didn't call sync_metadata
        let contents = fs::read_to_string(tmpdir.path().join("index-0-0.csv")).unwrap();
        assert!(contents.contains("/photos/00001_a.jpg,a.jpg,100"));
        assert!(contents.contains("/photos/00002_b.jpg,b.jpg,200"));
    }

    #[test]
    fn test_compact_index() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("index-2-3.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "old1,a.jpg,1").unwrap();
        writeln!(file, "old2,b.jpg,2").unwrap();
        writeln!(file, "/photos/00003_c.jpg,c.jpg,3").unwrap();
        writeln!(file, "/photos/00004_d.jpg,d.jpg,4").unwrap();
        writeln!(file, "/photos/00005_e.jpg,e.jpg,5").unwrap();

        let meta = IndexMetadata {
            start_line: 2,
            valid_count: 3,
        };
        let new_meta = compact_index(tmpdir.path(), &meta).unwrap();
        assert_eq!(new_meta.start_line, 0);
        assert_eq!(new_meta.valid_count, 3);

        let new_path = tmpdir.path().join("index-0-3.csv");
        let contents = fs::read_to_string(&new_path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "/photos/00003_c.jpg,c.jpg,3");
    }

    #[test]
    fn test_delete_oldest() {
        let tmpdir = tempfile::tempdir().unwrap();
        let photos_dir = tmpdir.path().join("photos");
        fs::create_dir(&photos_dir).unwrap();

        // Create photo files
        let photo1 = photos_dir.join("00001_old.jpg");
        File::create(&photo1).unwrap();
        let photo2 = photos_dir.join("00002_old2.jpg");
        File::create(&photo2).unwrap();
        let photo3 = photos_dir.join("00003_new.jpg");
        File::create(&photo3).unwrap();

        let path = tmpdir.path().join("index-0-3.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{},old.jpg,1", photo1.display()).unwrap();
        writeln!(file, "{},old2.jpg,2", photo2.display()).unwrap();
        writeln!(file, "{},new.jpg,3", photo3.display()).unwrap();

        let meta = IndexMetadata {
            start_line: 0,
            valid_count: 3,
        };
        let (new_meta, deleted) = delete_oldest(tmpdir.path(), &meta, 2).unwrap();
        assert_eq!(new_meta.start_line, 2);
        assert_eq!(new_meta.valid_count, 1);
        assert_eq!(deleted, 2);
        assert!(!photo1.exists());
        assert!(!photo2.exists());
        assert!(photo3.exists());
    }

    #[test]
    fn test_dedup_set() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("index-0-3.csv");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "/a.jpg,a.jpg,100").unwrap();
        writeln!(file, "/b.jpg,b.jpg,200").unwrap();
        writeln!(file, "/c.jpg,c.jpg,300").unwrap();

        let meta = IndexMetadata {
            start_line: 0,
            valid_count: 3,
        };
        let set = build_dedup_set(&path, &meta).unwrap();
        assert!(set.contains(&100));
        assert!(set.contains(&200));
        assert!(set.contains(&300));
        assert!(!set.contains(&999));
    }
}
