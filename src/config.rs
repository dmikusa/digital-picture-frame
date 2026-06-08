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

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[derive(Default)]
pub enum AspectRatioMode {
    #[serde(rename = "fit")]
    #[default]
    Fit,
    #[serde(rename = "fill")]
    Fill,
}


#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub photos_dir: PathBuf,
    pub socket_path: PathBuf,
    pub native_resolution: String,
    #[serde(default)]
    pub aspect_ratio_mode: AspectRatioMode,
    #[serde(default = "default_batch_delete_size")]
    pub batch_delete_size: usize,
    #[serde(default = "default_log_max_size")]
    pub log_max_size: usize,
    #[serde(default = "default_log_max_files")]
    pub log_max_files: usize,
}

fn default_batch_delete_size() -> usize {
    20
}

fn default_log_max_size() -> usize {
    262_144 // 256 KiB
}

fn default_log_max_files() -> usize {
    2
}

impl Config {
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file: {}", e))?;
        let mut config: Config = toml::from_str(&contents)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;
        config.validate()?;
        config.photos_dir = config.photos_dir
            .canonicalize()
            .map_err(|e| format!("Failed to resolve photos_dir: {}", e))?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.photos_dir.exists() {
            return Err(format!("photos_dir does not exist: {}", self.photos_dir.display()));
        }
        if !self.photos_dir.is_dir() {
            return Err(format!("photos_dir is not a directory: {}", self.photos_dir.display()));
        }

        // Validate native_resolution format: WxH
        let parts: Vec<&str> = self.native_resolution.split('x').collect();
        if parts.len() != 2 {
            return Err(format!(
                "native_resolution must be in format WxH, got: {}",
                self.native_resolution
            ));
        }
        let width: u32 = parts[0]
            .parse()
            .map_err(|_| format!("Invalid width in native_resolution: {}", parts[0]))?;
        let height: u32 = parts[1]
            .parse()
            .map_err(|_| format!("Invalid height in native_resolution: {}", parts[1]))?;
        if width == 0 || height == 0 {
            return Err("native_resolution width and height must be greater than 0".to_string());
        }

        if self.batch_delete_size == 0 {
            return Err("batch_delete_size must be greater than 0".to_string());
        }

        Ok(())
    }

    pub fn resolution(&self) -> (u32, u32) {
        let parts: Vec<&str> = self.native_resolution.split('x').collect();
        (
            parts[0].parse().unwrap_or(1920),
            parts[1].parse().unwrap_or(1080),
        )
    }

}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (w, h) = self.resolution();
        write!(
            f,
            "Config {{ photos_dir: {}, socket_path: {}, resolution: {}x{}, aspect_ratio_mode: {:?}, batch_delete_size: {}, log_max_size: {}, log_max_files: {} }}",
            self.photos_dir.display(),
            self.socket_path.display(),
            w,
            h,
            self.aspect_ratio_mode,
            self.batch_delete_size,
            self.log_max_size,
            self.log_max_files
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_valid_config() {
        let toml_str = r#"
photos_dir = "/tmp/photos"
socket_path = "/tmp/photo-frame.sock"
native_resolution = "1920x1080"
aspect_ratio_mode = "fit"
batch_delete_size = 10
log_max_size = 131072
log_max_files = 3
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.photos_dir, PathBuf::from("/tmp/photos"));
        assert_eq!(config.socket_path, PathBuf::from("/tmp/photo-frame.sock"));
        assert_eq!(config.native_resolution, "1920x1080");
        assert_eq!(config.aspect_ratio_mode, AspectRatioMode::Fit);
        assert_eq!(config.batch_delete_size, 10);
        assert_eq!(config.log_max_size, 131_072);
        assert_eq!(config.log_max_files, 3);
    }

    #[test]
    fn test_parse_defaults() {
        let toml_str = r#"
photos_dir = "/tmp/photos"
socket_path = "/tmp/photo-frame.sock"
native_resolution = "800x600"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.aspect_ratio_mode, AspectRatioMode::Fit);
        assert_eq!(config.batch_delete_size, 20);
        assert_eq!(config.log_max_size, 262_144);
        assert_eq!(config.log_max_files, 2);
    }

    #[test]
    fn test_validate_resolution() {
        let toml_str = r#"
photos_dir = "/tmp"
socket_path = "/tmp/sock"
native_resolution = "abcxdef"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_from_file() {
        let mut tmpfile = tempfile::NamedTempFile::new().unwrap();
        let toml_str = r#"
photos_dir = "/tmp"
socket_path = "/tmp/sock"
native_resolution = "1024x768"
"#;
        tmpfile.write_all(toml_str.as_bytes()).unwrap();
        let config = Config::from_file(tmpfile.path());
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(config.resolution(), (1024, 768));
    }
}
