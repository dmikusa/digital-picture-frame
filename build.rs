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

//! Generate the compile-time trusted update-key table from OpenSSH public keys.
//!
//! Files live in `packaging/update-keys/<key-id>.pub`. Each file contains one
//! line in the usual OpenSSH format:
//!
//! ```text
//! ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... 2026-10
//! ```
//!
//! The key ID is the filename without `.pub`. The generated table contains only
//! the raw 32-byte Ed25519 public key, so the runtime verifier stays small and
//! does not need to parse text.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo"),
    );
    let keys_dir = manifest_dir.join("packaging/update-keys");
    println!("cargo:rerun-if-changed={}", keys_dir.display());

    let mut key_files = Vec::new();
    if keys_dir.is_dir() {
        for entry in fs::read_dir(&keys_dir).expect("read packaging/update-keys") {
            let path = entry.expect("read packaging/update-keys entry").path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("pub") {
                key_files.push(path);
            }
        }
    }
    key_files.sort();

    let mut generated = String::new();
    generated
        .push_str("/// Keys trusted by this build, generated from packaging/update-keys/*.pub.\n");
    generated.push_str("pub const TRUSTED_KEYS: &[TrustedKey] = &[\n");
    for path in key_files {
        println!("cargo:rerun-if-changed={}", path.display());
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("update key filename is valid UTF-8");
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            panic!("{}: key ID must use only [A-Za-z0-9._-]", path.display());
        }

        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {}", path.display(), err));
        let bytes = parse_openssh_public_key(&text)
            .unwrap_or_else(|err| panic!("{}: {}", path.display(), err));

        generated.push_str(&format!("    TrustedKey {{ id: {:?}, bytes: [", id));
        for (index, byte) in bytes.iter().enumerate() {
            if index > 0 {
                generated.push_str(", ");
            }
            generated.push_str(&format!("0x{:02x}", byte));
        }
        generated.push_str("] },\n");
    }
    generated.push_str("];\n");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is always set by Cargo"));
    fs::write(out_dir.join("trusted_keys.rs"), generated)
        .expect("write generated trusted update keys");
}

/// Parse one OpenSSH `ssh-ed25519` public-key line and return the raw key.
fn parse_openssh_public_key(text: &str) -> Result<[u8; 32], String> {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| "public key file is empty".to_string())?;
    let mut fields = line.split_whitespace();
    let kind = fields
        .next()
        .ok_or_else(|| "missing public key type".to_string())?;
    if kind != "ssh-ed25519" {
        return Err(format!(
            "expected an ssh-ed25519 public key, found '{}'",
            kind
        ));
    }
    let encoded = fields
        .next()
        .ok_or_else(|| "missing public key data".to_string())?;
    let blob = STANDARD
        .decode(encoded)
        .map_err(|err| format!("public key is not valid base64: {}", err))?;

    let mut offset = 0;
    let wire_kind = read_ssh_string(&blob, &mut offset)?;
    if wire_kind != b"ssh-ed25519" {
        return Err("public key blob does not contain an ssh-ed25519 key".to_string());
    }
    let wire_key = read_ssh_string(&blob, &mut offset)?;
    if wire_key.len() != 32 {
        return Err(format!(
            "Ed25519 public key must be 32 bytes, found {}",
            wire_key.len()
        ));
    }
    if offset != blob.len() {
        return Err("public key blob has trailing data".to_string());
    }

    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(wire_key);
    Ok(bytes)
}

fn read_ssh_string<'a>(blob: &'a [u8], offset: &mut usize) -> Result<&'a [u8], String> {
    let length_bytes = blob
        .get(*offset..*offset + 4)
        .ok_or_else(|| "public key blob is truncated".to_string())?;
    let length = u32::from_be_bytes(
        length_bytes
            .try_into()
            .expect("slice of four bytes converts to u32"),
    ) as usize;
    *offset += 4;

    let value = blob
        .get(*offset..*offset + length)
        .ok_or_else(|| "public key blob is truncated".to_string())?;
    *offset += length;
    Ok(value)
}
