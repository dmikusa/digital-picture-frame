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

use anyhow::{Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

/// Configuration for the Picture Frame application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameConfig {
    /// Base directory where photos are located
    pub photos_directory: String,
}

impl Default for FrameConfig {
    fn default() -> Self {
        Self {
            photos_directory: "images".to_string(),
        }
    }
}

impl FrameConfig {
    /// Load configuration from file, with fallback locations and defaults
    pub fn load() -> Result<Self> {
        // Try current directory first
        let current_dir_config = PathBuf::from("frame-config.json");
        if current_dir_config.exists() {
            debug!(
                "Found config file in current directory: {:?}",
                current_dir_config
            );
            return Self::load_from_file(&current_dir_config);
        }

        // Try user home directory
        if let Ok(home_dir) = env::var("HOME") {
            let home_config = PathBuf::from(home_dir)
                .join(".picture-frame-ui")
                .join("frame-config.json");

            if home_config.exists() {
                debug!("Found config file in home directory: {:?}", home_config);
                return Self::load_from_file(&home_config);
            }
        }

        // No config file found, use defaults
        warn!("No configuration file found, using defaults");
        info!(
            "To create a config file, place 'frame-config.json' in the current directory or ~/.picture-frame-ui/"
        );
        Ok(Self::default())
    }

    /// Load configuration from a specific file
    fn load_from_file(config_path: &PathBuf) -> Result<Self> {
        let config_content = fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {:?}", config_path))?;

        let config: FrameConfig = serde_json::from_str(&config_content)
            .with_context(|| format!("Failed to parse config file: {:?}", config_path))?;

        info!("Loaded configuration from: {:?}", config_path);
        debug!("Config: {:?}", config);

        Ok(config)
    }

    /// Get the absolute path to the photos directory
    pub fn get_photos_path(&self) -> PathBuf {
        let path = PathBuf::from(&self.photos_directory);

        // Convert relative path to absolute if needed
        if path.is_relative() {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        } else {
            path
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = FrameConfig::default();
        assert_eq!(config.photos_directory, "images");
    }

    #[test]
    fn test_config_serialization() {
        let config = FrameConfig {
            photos_directory: "/home/user/photos".to_string(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: FrameConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.photos_directory, deserialized.photos_directory);
    }

    #[test]
    fn test_load_from_file() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("frame-config.json");

        let test_config = FrameConfig {
            photos_directory: "/test/photos".to_string(),
        };

        let config_json = serde_json::to_string_pretty(&test_config).unwrap();
        fs::write(&config_path, config_json).unwrap();

        let loaded_config = FrameConfig::load_from_file(&config_path).unwrap();
        assert_eq!(test_config.photos_directory, loaded_config.photos_directory);
    }

    #[test]
    fn test_get_photos_path_relative() {
        let config = FrameConfig {
            photos_directory: "photos".to_string(),
        };

        let path = config.get_photos_path();
        assert!(path.is_absolute());
        assert!(path.ends_with("photos"));
    }

    #[test]
    fn test_get_photos_path_absolute() {
        let config = FrameConfig {
            photos_directory: "/home/user/photos".to_string(),
        };

        let path = config.get_photos_path();
        assert_eq!(path, PathBuf::from("/home/user/photos"));
    }
}
