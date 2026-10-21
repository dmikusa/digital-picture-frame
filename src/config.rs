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
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum AspectRatioMode {
    #[serde(rename = "fit")]
    #[default]
    Fit,
    #[serde(rename = "fill")]
    Fill,
}

fn default_check_interval() -> u64 {
    86_400 // 24 hours
}

fn default_control_socket_path() -> PathBuf {
    PathBuf::from("/run/photo-frame/control.sock")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSourceConfig {
    #[serde(rename = "type")]
    pub source_type: String,
    pub name: String,
    #[serde(flatten)]
    pub params: HashMap<String, String>,
    #[serde(default = "default_check_interval")]
    pub check_interval_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub photos_dir: PathBuf,
    /// Socket the display app creates to receive image paths (`IMG`), which the
    /// manager connects to. The display app reads the same path from
    /// `DISPLAY_SOCKET_PATH`. Accepts the legacy `socket_path` key for
    /// compatibility with existing config files.
    #[serde(alias = "socket_path")]
    pub display_socket_path: PathBuf,
    /// Socket the display app creates for control commands (`CLR`/`SHOW`/`RESUME`),
    /// used by config mode. The display app reads the same path from
    /// `CONTROL_SOCKET_PATH`.
    #[serde(default = "default_control_socket_path")]
    pub control_socket_path: PathBuf,
    pub native_resolution: String,
    #[serde(default)]
    pub aspect_ratio_mode: AspectRatioMode,
    #[serde(default = "default_batch_delete_size")]
    pub batch_delete_size: usize,
    #[serde(default = "default_log_max_size")]
    pub log_max_size: usize,
    #[serde(default = "default_log_max_files")]
    pub log_max_files: usize,
    #[serde(default)]
    pub remote_sources: Vec<RemoteSourceConfig>,
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
        let mut config: Config =
            toml::from_str(&contents).map_err(|e| format!("Failed to parse config file: {}", e))?;
        config.validate()?;
        config.photos_dir = config
            .photos_dir
            .canonicalize()
            .map_err(|e| format!("Failed to resolve photos_dir: {}", e))?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.photos_dir.exists() {
            return Err(format!(
                "photos_dir does not exist: {}",
                self.photos_dir.display()
            ));
        }
        if !self.photos_dir.is_dir() {
            return Err(format!(
                "photos_dir is not a directory: {}",
                self.photos_dir.display()
            ));
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

        for source in &self.remote_sources {
            if source.check_interval_seconds == 0 {
                return Err(format!(
                    "check_interval_seconds must be greater than 0 for remote source \"{}\"",
                    source.name
                ));
            }
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

/// Serialize `config` to TOML and atomically replace `path`.
///
/// The temporary file is created in the same directory so the final rename is
/// atomic. It is created with mode 0600 (config may contain credentials) and
/// removed if any step fails, leaving the original file untouched.
pub fn save_config_atomic(path: &Path, config: &Config) -> io::Result<()> {
    let toml_str = toml::to_string_pretty(config).map_err(io::Error::other)?;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("config path has no file name: {}", path.display()),
        )
    })?;
    let tmp_path = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));

    let result =
        write_private_file(&tmp_path, &toml_str).and_then(|()| std::fs::rename(&tmp_path, path));

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }

    result
}

/// Write `contents` to `path`, creating it with mode 0600.
pub(crate) fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;

    // Explicitly set the mode as well: the open mode is filtered through the
    // process umask, and we always want private files to end up as 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }

    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (w, h) = self.resolution();
        write!(
            f,
            "Config {{ photos_dir: {}, display_socket_path: {}, control_socket_path: {}, resolution: {}x{}, aspect_ratio_mode: {:?}, batch_delete_size: {}, log_max_size: {}, log_max_files: {}, remote_sources: {} }}",
            self.photos_dir.display(),
            self.display_socket_path.display(),
            self.control_socket_path.display(),
            w,
            h,
            self.aspect_ratio_mode,
            self.batch_delete_size,
            self.log_max_size,
            self.log_max_files,
            self.remote_sources.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn sample_config(photos_dir: PathBuf) -> Config {
        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.test".to_string());

        Config {
            photos_dir,
            display_socket_path: PathBuf::from("/run/photo-frame/photo-frame.sock"),
            control_socket_path: PathBuf::from("/run/photo-frame/control.sock"),
            native_resolution: "1024x768".to_string(),
            aspect_ratio_mode: AspectRatioMode::Fill,
            batch_delete_size: 10,
            log_max_size: 2048,
            log_max_files: 3,
            remote_sources: vec![RemoteSourceConfig {
                source_type: "immich".to_string(),
                name: "test-source".to_string(),
                params,
                check_interval_seconds: 3600,
            }],
        }
    }

    #[test]
    fn test_parse_valid_config() {
        let toml_str = r#"
photos_dir = "/tmp/photos"
display_socket_path = "/run/photo-frame/photo-frame.sock"
control_socket_path = "/run/photo-frame/control.sock"
native_resolution = "1920x1080"
aspect_ratio_mode = "fit"
batch_delete_size = 10
log_max_size = 131072
log_max_files = 3
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.photos_dir, PathBuf::from("/tmp/photos"));
        assert_eq!(
            config.display_socket_path,
            PathBuf::from("/run/photo-frame/photo-frame.sock")
        );
        assert_eq!(
            config.control_socket_path,
            PathBuf::from("/run/photo-frame/control.sock")
        );
        assert_eq!(config.native_resolution, "1920x1080");
        assert_eq!(config.aspect_ratio_mode, AspectRatioMode::Fit);
        assert_eq!(config.batch_delete_size, 10);
        assert_eq!(config.log_max_size, 131_072);
        assert_eq!(config.log_max_files, 3);
    }

    #[test]
    fn test_parse_defaults() {
        // Uses the legacy `socket_path` key; `control_socket_path` is optional.
        let toml_str = r#"
photos_dir = "/tmp/photos"
socket_path = "/run/photo-frame/photo-frame.sock"
native_resolution = "800x600"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.display_socket_path,
            PathBuf::from("/run/photo-frame/photo-frame.sock")
        );
        assert_eq!(
            config.control_socket_path,
            PathBuf::from("/run/photo-frame/control.sock")
        );
        assert_eq!(config.aspect_ratio_mode, AspectRatioMode::Fit);
        assert_eq!(config.batch_delete_size, 20);
        assert_eq!(config.log_max_size, 262_144);
        assert_eq!(config.log_max_files, 2);
    }

    #[test]
    fn test_validate_resolution() {
        let toml_str = r#"
photos_dir = "/tmp"
display_socket_path = "/tmp/sock"
native_resolution = "abcxdef"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_remote_check_interval() {
        let photos_dir = tempfile::tempdir().unwrap();
        let toml_str = format!(
            r#"
photos_dir = "{}"
display_socket_path = "/tmp/sock"
native_resolution = "1024x768"

[[remote_sources]]
type = "dropbox"
name = "Test Dropbox"
access_token = "token"
check_interval_seconds = 0
"#,
            photos_dir.path().display()
        );
        let config: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.remote_sources.len(), 1);
        assert_eq!(config.remote_sources[0].check_interval_seconds, 0);
        let err = config.validate().unwrap_err();
        assert!(err.contains("check_interval_seconds"), "got: {err}");
    }

    #[test]
    fn test_validate_remote_check_interval_default() {
        let photos_dir = tempfile::tempdir().unwrap();
        let toml_str = format!(
            r#"
photos_dir = "{}"
display_socket_path = "/tmp/sock"
native_resolution = "1024x768"

[[remote_sources]]
type = "dropbox"
name = "Test Dropbox"
access_token = "token"
"#,
            photos_dir.path().display()
        );
        let config: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.remote_sources[0].check_interval_seconds, 86_400);
        assert!(config.validate().is_ok());
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
        // The legacy `socket_path` key still maps to display_socket_path.
        assert_eq!(config.display_socket_path, PathBuf::from("/tmp/sock"));
        assert_eq!(
            config.control_socket_path,
            PathBuf::from("/run/photo-frame/control.sock")
        );
    }

    #[test]
    fn save_config_atomic_round_trips_and_sets_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let photos_dir = dir.path().join("photos");
        std::fs::create_dir(&photos_dir).unwrap();
        let config = sample_config(photos_dir);

        save_config_atomic(&path, &config).unwrap();

        let loaded = Config::from_file(&path).unwrap();
        assert_eq!(loaded.photos_dir, config.photos_dir.canonicalize().unwrap());
        assert_eq!(loaded.native_resolution, "1024x768");
        assert_eq!(loaded.batch_delete_size, 10);
        assert_eq!(loaded.remote_sources.len(), 1);
        assert_eq!(loaded.remote_sources[0].source_type, "immich");
        assert_eq!(loaded.remote_sources[0].name, "test-source");
        assert_eq!(
            loaded.remote_sources[0].params.get("url").unwrap(),
            "https://example.test"
        );

        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // The temporary file must not be left behind after a successful save.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(names
            .iter()
            .all(|name| !name.to_string_lossy().ends_with(".tmp")));
    }

    #[cfg(unix)]
    #[test]
    fn save_config_atomic_preserves_original_on_failure() {
        // Root bypasses directory permissions, so this failure mode cannot be
        // triggered when tests run as root (e.g. in some containers).
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "original config").unwrap();
        let config = sample_config(dir.path().to_path_buf());

        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        let result = save_config_atomic(&path, &config);

        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original config");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
