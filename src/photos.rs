/*
 * Digital Picture Frame - A fullscreen photo slideshow application
 * Copyright (C) 2025 Daniel Mikusa
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published
 * by the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use anyhow::Result;
use log::{debug, info};
use std::fs;
use std::iter::Peekable;
use url::Url;

pub trait PhotoLoader {
    fn load_next_photo(&mut self) -> Result<Url>;
}

pub struct FilePhotoLoader {
    base_directory: String,
    photo_iterator: Option<Peekable<fs::ReadDir>>,
}

impl FilePhotoLoader {
    pub fn new(base_directory: String) -> Self {
        Self {
            base_directory,
            photo_iterator: None,
        }
    }
}

impl PhotoLoader for FilePhotoLoader {
    fn load_next_photo(&mut self) -> Result<Url> {
        if self.photo_iterator.is_none() {
            info!("Reading photos from directory: {}", self.base_directory);
            let read_dir = fs::read_dir(&self.base_directory)?;

            // Check if directory is empty by trying to peek
            let mut peekable_iter = read_dir.peekable();
            if peekable_iter.peek().is_none() {
                return Err(anyhow::anyhow!(
                    "No photos found in directory {}",
                    self.base_directory
                ));
            }

            // Now use the peekable iterator as our main iterator
            self.photo_iterator = Some(peekable_iter);
        }

        match self.photo_iterator.as_mut().unwrap().next() {
            Some(Ok(entry)) => Ok(Url::from_file_path(entry.path().canonicalize()?).map_err(
                |_| anyhow::anyhow!("unable to create URL from {}", entry.path().display()),
            )?),
            Some(Err(e)) => Err(anyhow::Error::from(e)),
            None => {
                debug!("Reached end of photo list, restarting");
                self.photo_iterator = None;
                self.load_next_photo() // Restart the iterator
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_directory_with_files(files: &[&str]) -> TempDir {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");

        for file in files {
            let file_path = temp_dir.path().join(file);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).expect("Failed to create parent directory");
            }
            fs::write(&file_path, "test content").expect("Failed to create test file");
        }

        temp_dir
    }

    #[test]
    fn test_load_next_photo_with_single_file() {
        let temp_dir = create_test_directory_with_files(&["photo1.jpg"]);
        let mut loader = FilePhotoLoader::new(temp_dir.path().to_string_lossy().to_string());

        let result = loader.load_next_photo();
        assert!(result.is_ok());

        let url = result.unwrap();
        assert!(url.to_string().contains("photo1.jpg"));
    }

    #[test]
    fn test_load_next_photo_with_multiple_files() {
        let temp_dir =
            create_test_directory_with_files(&["photo1.jpg", "photo2.png", "photo3.gif"]);
        let mut loader = FilePhotoLoader::new(temp_dir.path().to_string_lossy().to_string());

        // Load first photo
        let result1 = loader.load_next_photo();
        assert!(result1.is_ok());

        // Load second photo
        let result2 = loader.load_next_photo();
        assert!(result2.is_ok());

        // Load third photo
        let result3 = loader.load_next_photo();
        assert!(result3.is_ok());

        // Verify they're different URLs
        let url1 = result1.unwrap();
        let url2 = result2.unwrap();
        let url3 = result3.unwrap();

        assert_ne!(url1, url2);
        assert_ne!(url2, url3);
        assert_ne!(url1, url3);
    }

    #[test]
    fn test_load_next_photo_cycles_through_directory() {
        let temp_dir = create_test_directory_with_files(&["photo1.jpg", "photo2.png"]);
        let mut loader = FilePhotoLoader::new(temp_dir.path().to_string_lossy().to_string());

        // Load all files in directory (should be 2)
        let mut urls = Vec::new();
        for _ in 0..2 {
            let result = loader.load_next_photo();
            assert!(result.is_ok());
            urls.push(result.unwrap());
        }

        // Next call should restart from beginning
        let result_restart = loader.load_next_photo();
        assert!(result_restart.is_ok());

        // Should match one of the first two URLs (cycling behavior)
        let restart_url = result_restart.unwrap();
        assert!(urls.contains(&restart_url));
    }

    #[test]
    fn test_load_next_photo_nonexistent_directory() {
        let mut loader = FilePhotoLoader::new("/nonexistent/directory".to_string());

        let result = loader.load_next_photo();
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("No such file or directory"));
    }

    #[test]
    fn test_load_next_photo_empty_directory() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let mut loader = FilePhotoLoader::new(temp_dir.path().to_string_lossy().to_string());

        let result = loader.load_next_photo();
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("No photos found in directory"));
    }
}
