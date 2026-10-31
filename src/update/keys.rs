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
//! These raw Ed25519 public keys are baked into `photo-frame-update` and are the
//! only keys that can authorize an install. The private key never ships to the
//! device, CI, or this repository.
//!
//! Key rotation requires adding the new public key to this list, then building
//! and signing a release with a key that is *already* trusted. A device only
//! trusts keys listed here in the build it is running, so a release signed only
//! by an unknown key can never bootstrap itself onto an older device. To remove
//! a key, delete it from this list and ship a release signed by a remaining
//! trusted key.
//!
//! The list starts intentionally empty. With no trusted keys every install is
//! refused (fail closed). The owner must add their public key(s) here before
//! building a release bundle.

/// A pinned update-signing public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedKey {
    /// Short stable identifier, matched against the `key_id` in the signature
    /// envelope (for example a year and month).
    pub id: &'static str,
    /// Raw 32-byte Ed25519 public key.
    pub bytes: [u8; 32],
}

/// Keys trusted by this build. Empty until the owner pins one.
pub const TRUSTED_KEYS: &[TrustedKey] = &[
    // The owner adds their public key(s) here before building a release, e.g.:
    // TrustedKey { id: "2026-10", bytes: [0u8; 32] },
];
