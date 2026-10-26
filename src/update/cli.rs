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

//! Command line for `photo-frame-update`.
//!
//! Argument parsing is manual and small on purpose: this binary runs as root
//! and is shipped inside the app bundle, so it should stay easy to audit.

use crate::update::health::{HealthCheck, Services};
use crate::update::keys::TRUSTED_KEYS;
use crate::update::{
    gc, install, list_releases, status, InstallOptions, Paths, DEFAULT_LOG_PATH, DEFAULT_ROOT,
    DEFAULT_STATE_DIR,
};
use std::fmt::Display;
use std::path::PathBuf;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

/// Run the CLI and return a process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let Some(command) = args.first() else {
        print_help();
        return EXIT_USAGE;
    };
    if command == "-h" || command == "--help" {
        print_help();
        return EXIT_OK;
    }
    let rest = &args[1..];
    match command.as_str() {
        "install" => command_install(rest),
        "list" => command_list(rest),
        "status" => command_status(rest),
        "gc" => command_gc(rest),
        "keys" => command_keys(rest),
        other => {
            eprintln!("error: unknown command '{}'", other);
            print_help();
            EXIT_USAGE
        }
    }
}

fn command_install(args: &[String]) -> i32 {
    if wants_help(args) {
        print_help();
        return EXIT_OK;
    }
    let parsed = match parse(args, true, false) {
        Ok(parsed) => parsed,
        Err(err) => return usage_error(&err),
    };
    if parsed.positional.len() != 1 {
        return usage_error("install requires exactly one bundle path or https:// URL");
    }

    let paths = parsed.paths();
    let services = Services::default();
    let health = if parsed.no_restart {
        HealthCheck::skip()
    } else {
        HealthCheck::system(services.clone())
    };
    let options = InstallOptions {
        paths: &paths,
        source: &parsed.positional[0],
        expected_sha256: parsed.sha256.as_deref(),
        trusted_keys: TRUSTED_KEYS,
        services,
        health,
        no_restart: parsed.no_restart,
    };

    match install(&options) {
        Ok(report) => {
            match &report.previous {
                Some(previous) => println!(
                    "installed {} (sequence {}) signed by {}; previous release {}",
                    report.version, report.sequence, report.key_id, previous
                ),
                None => println!(
                    "installed {} (sequence {}) signed by {}",
                    report.version, report.sequence, report.key_id
                ),
            }
            EXIT_OK
        }
        Err(err) => fail(&err),
    }
}

fn command_list(args: &[String]) -> i32 {
    if wants_help(args) {
        print_help();
        return EXIT_OK;
    }
    let parsed = match parse(args, false, false) {
        Ok(parsed) => parsed,
        Err(err) => return usage_error(&err),
    };
    if !parsed.positional.is_empty() {
        return usage_error("list takes no positional arguments");
    }

    match list_releases(&parsed.paths()) {
        Ok(releases) if releases.is_empty() => {
            println!("no releases installed");
            EXIT_OK
        }
        Ok(releases) => {
            println!("{:<16} {:>10}  STATE", "VERSION", "SIZE");
            for release in releases {
                let mut state = Vec::new();
                if release.current {
                    state.push("current");
                }
                if release.previous {
                    state.push("previous");
                }
                println!(
                    "{:<16} {:>10}  {}",
                    release.version,
                    format_size(release.size),
                    state.join(",")
                );
            }
            EXIT_OK
        }
        Err(err) => fail(&err),
    }
}

fn command_status(args: &[String]) -> i32 {
    if wants_help(args) {
        print_help();
        return EXIT_OK;
    }
    let parsed = match parse(args, false, false) {
        Ok(parsed) => parsed,
        Err(err) => return usage_error(&err),
    };
    if !parsed.positional.is_empty() {
        return usage_error("status takes no positional arguments");
    }

    let paths = parsed.paths();
    match status(&paths) {
        Ok(report) => {
            println!(
                "current:    {}",
                report.state.current.as_deref().unwrap_or("-")
            );
            println!(
                "previous:   {}",
                report.state.previous.as_deref().unwrap_or("-")
            );
            println!(
                "last_good:  {}",
                report.state.last_good.as_deref().unwrap_or("-")
            );
            match &report.state.pending {
                Some(pending) => println!(
                    "pending:    {} (sequence {}, from {})",
                    pending.to,
                    pending.sequence,
                    pending.from.as_deref().unwrap_or("-")
                ),
                None => println!("pending:    none"),
            }
            println!("sequence:   {}", report.state.sequence);
            println!("releases:   {}", report.release_count);
            match &report.current_link {
                Some(target) if report.current_link_valid => println!(
                    "current link: {} -> releases/{}",
                    paths.current_link().display(),
                    target
                ),
                Some(target) => println!(
                    "current link: {} -> releases/{} (MISSING)",
                    paths.current_link().display(),
                    target
                ),
                None => println!("current link: none"),
            }
            EXIT_OK
        }
        Err(err) => fail(&err),
    }
}

fn command_gc(args: &[String]) -> i32 {
    if wants_help(args) {
        print_help();
        return EXIT_OK;
    }
    let parsed = match parse(args, false, false) {
        Ok(parsed) => parsed,
        Err(err) => return usage_error(&err),
    };
    if !parsed.positional.is_empty() {
        return usage_error("gc takes no positional arguments");
    }

    match gc(&parsed.paths()) {
        Ok(report) => {
            if report.removed.is_empty() {
                println!("nothing to remove ({} releases kept)", report.kept.len());
            } else {
                println!("removed: {}", report.removed.join(", "));
                println!("kept:    {} releases", report.kept.len());
            }
            EXIT_OK
        }
        Err(err) => fail(&err),
    }
}

fn command_keys(args: &[String]) -> i32 {
    if wants_help(args) {
        print_help();
        return EXIT_OK;
    }
    if !args.is_empty() {
        return usage_error("keys takes no arguments");
    }
    if TRUSTED_KEYS.is_empty() {
        eprintln!(
            "warning: no trusted update keys are baked into this build; all installs will be refused"
        );
        return EXIT_OK;
    }
    for key in TRUSTED_KEYS {
        println!("{}", key.id);
    }
    EXIT_OK
}

struct Options {
    root: PathBuf,
    state_dir: PathBuf,
    log_path: PathBuf,
    no_restart: bool,
    sha256: Option<String>,
    #[allow(dead_code)]
    grace_seconds: u64,
    positional: Vec<String>,
}

impl Options {
    fn paths(&self) -> Paths {
        Paths {
            root: self.root.clone(),
            state_dir: self.state_dir.clone(),
            log_path: self.log_path.clone(),
        }
    }
}

fn parse(
    args: &[String],
    allow_sha256: bool,
    allow_grace: bool,
) -> std::result::Result<Options, String> {
    let mut options = Options {
        root: PathBuf::from(DEFAULT_ROOT),
        state_dir: PathBuf::from(DEFAULT_STATE_DIR),
        log_path: PathBuf::from(DEFAULT_LOG_PATH),
        no_restart: false,
        sha256: None,
        grace_seconds: 0,
        positional: Vec::new(),
    };

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--root" => {
                options.root = PathBuf::from(take_value(args, &mut index, "--root")?);
                continue;
            }
            "--state-dir" => {
                options.state_dir = PathBuf::from(take_value(args, &mut index, "--state-dir")?);
                continue;
            }
            "--log" => {
                options.log_path = PathBuf::from(take_value(args, &mut index, "--log")?);
                continue;
            }
            "--no-restart" => {
                options.no_restart = true;
            }
            "--sha256" if allow_sha256 => {
                options.sha256 = Some(take_value(args, &mut index, "--sha256")?);
                continue;
            }
            "--grace-seconds" if allow_grace => {
                let value = take_value(args, &mut index, "--grace-seconds")?;
                options.grace_seconds = value.parse::<u64>().map_err(|_| {
                    format!(
                        "--grace-seconds requires a non-negative integer, got '{}'",
                        value
                    )
                })?;
                continue;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{}'", other));
            }
            other => options.positional.push(other.to_string()),
        }
        index += 1;
    }
    Ok(options)
}

fn take_value(
    args: &[String],
    index: &mut usize,
    flag: &str,
) -> std::result::Result<String, String> {
    *index += 1;
    match args.get(*index) {
        Some(value) => {
            *index += 1;
            Ok(value.clone())
        }
        None => Err(format!("{} requires a value", flag)),
    }
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "-h" || arg == "--help")
}

fn fail(err: &dyn Display) -> i32 {
    eprintln!("error: {}", err);
    EXIT_FAILURE
}

fn usage_error(message: &str) -> i32 {
    eprintln!("error: {}", message);
    eprintln!("usage: photo-frame-update <install|rollback|list|status|gc|guard|keys> [options]");
    EXIT_USAGE
}

fn print_help() {
    println!("photo-frame-update — install signed photo frame app releases");
    println!();
    println!("Usage: photo-frame-update <command> [options]");
    println!();
    println!("Commands:");
    println!("  install <path|https-url>   Verify and install a release bundle");
    println!("  rollback                   Activate the recorded previous release");
    println!("  list                       Show installed releases");
    println!("  status                     Show update state");
    println!("  gc                         Remove old releases");
    println!("  guard                      Confirm or revert an unconfirmed update");
    println!("  keys                       List trusted signing key IDs");
    println!();
    println!("Options:");
    println!("  --sha256 <hex>        Expected bundle hash (required for HTTPS URLs)");
    println!(
        "  --root <dir>          Install root (default {})",
        DEFAULT_ROOT
    );
    println!(
        "  --state-dir <dir>     State directory (default {})",
        DEFAULT_STATE_DIR
    );
    println!("  --grace-seconds <n>   guard: wait before the health check");
    println!("  --no-restart          Skip systemctl restarts and health checks (dev)");
    println!(
        "  --log <file>          Audit log (default {})",
        DEFAULT_LOG_PATH
    );
    println!("  -h, --help            Show this help");
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn test_parse_defaults() {
        let parsed = parse(&args(&["bundle.tar.gz"]), true, false).unwrap();
        assert_eq!(parsed.root, PathBuf::from(DEFAULT_ROOT));
        assert_eq!(parsed.state_dir, PathBuf::from(DEFAULT_STATE_DIR));
        assert_eq!(parsed.log_path, PathBuf::from(DEFAULT_LOG_PATH));
        assert!(!parsed.no_restart);
        assert_eq!(parsed.positional, vec!["bundle.tar.gz".to_string()]);
    }

    #[test]
    fn test_parse_overrides() {
        let parsed = parse(
            &args(&[
                "bundle.tar.gz",
                "--root",
                "/tmp/root",
                "--state-dir",
                "/tmp/state",
                "--log",
                "/tmp/log",
                "--sha256",
                "abc",
                "--no-restart",
            ]),
            true,
            false,
        )
        .unwrap();
        assert_eq!(parsed.root, PathBuf::from("/tmp/root"));
        assert_eq!(parsed.state_dir, PathBuf::from("/tmp/state"));
        assert_eq!(parsed.log_path, PathBuf::from("/tmp/log"));
        assert_eq!(parsed.sha256.as_deref(), Some("abc"));
        assert!(parsed.no_restart);
    }

    #[test]
    fn test_parse_rejects_unknown_and_missing_values() {
        assert!(parse(&args(&["--bogus"]), true, false).is_err());
        assert!(parse(&args(&["--root"]), true, false).is_err());
        assert!(parse(&args(&["--grace-seconds"]), true, false).is_err());
        assert!(parse(&args(&["--grace-seconds", "x"]), true, true).is_err());
        assert!(parse(&args(&["--sha256", "abc"]), false, false).is_err());
    }

    #[test]
    fn test_parse_grace_seconds() {
        let parsed = parse(&args(&["--grace-seconds", "45"]), false, true).unwrap();
        assert_eq!(parsed.grace_seconds, 45);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn test_unknown_command_is_usage_error() {
        assert_eq!(run(args(&["nope"])), EXIT_USAGE);
    }

    #[test]
    fn test_help_is_ok() {
        assert_eq!(run(args(&["--help"])), EXIT_OK);
        assert_eq!(run(args(&["install", "--help"])), EXIT_OK);
    }

    #[test]
    fn test_keys_command_is_ok() {
        assert_eq!(run(args(&["keys"])), EXIT_OK);
    }

    #[test]
    fn test_install_requires_a_source() {
        assert_eq!(run(args(&["install"])), EXIT_USAGE);
    }

    #[test]
    fn test_install_rejects_http_url() {
        // No network access is attempted: the scheme is rejected first.
        let code = run(args(&[
            "install",
            "http://example.com/bundle.tar.gz",
            "--sha256",
            &"0".repeat(64),
            "--no-restart",
            "--root",
            "/tmp/photo-frame-update-test-root",
            "--state-dir",
            "/tmp/photo-frame-update-test-state",
            "--log",
            "/tmp/photo-frame-update-test.log",
        ]));
        assert_eq!(code, EXIT_FAILURE);
    }
}
