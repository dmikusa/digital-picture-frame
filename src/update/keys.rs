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

//! Compile-time trusted update-signing keys.
//!
//! The public keys live in `packaging/update-keys/<key-id>.pub`, one OpenSSH
//! `ssh-ed25519` public key per file. `build.rs` parses the files and generates
//! [`TRUSTED_KEYS`] with the raw 32-byte key and the filename as the key ID.
//!
//! The private key never ships to the device, CI, or this repository. Key
//! rotation requires adding the new `.pub` file, then building and signing a
//! release with a key that is *already* trusted. A device only trusts keys in
//! the build it is running, so a release signed only by an unknown key cannot
//! bootstrap itself onto an older device. To remove a key, delete its `.pub`
//! file and ship a release signed by a remaining trusted key.
//!
//! The directory starts empty. With no trusted keys every install is refused
//! (fail closed). The owner must add a public key file before building a
//! release bundle.

/// A pinned update-signing public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedKey {
    /// Short stable identifier, taken from the public key filename (for
    /// example a year and month).
    pub id: &'static str,
    /// Raw 32-byte Ed25519 public key.
    pub bytes: [u8; 32],
}

// Includes the generated `TRUSTED_KEYS` table.
include!(concat!(env!("OUT_DIR"), "/trusted_keys.rs"));
