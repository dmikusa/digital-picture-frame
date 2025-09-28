use anyhow::Result;
use log::debug;
use std::fs;
use url::Url;

pub trait PhotoLoader {
    fn load_next_photo(&mut self) -> Result<Url>;
}

pub struct FilePhotoLoader {
    base_directory: String,
    photo_iterator: Option<fs::ReadDir>,
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
            debug!("Reading photos from directory: {}", self.base_directory);
            self.photo_iterator = Some(fs::read_dir(&self.base_directory)?);

            if self.photo_iterator.is_none() {
                return Err(anyhow::anyhow!(
                    "No photos found in directory {}",
                    self.base_directory
                ));
            }
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
