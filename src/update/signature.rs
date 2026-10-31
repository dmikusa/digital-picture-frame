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

//! Verification of `.sha256.sig` files.
//!
//! The release workflow writes a standard OpenSSH `SSHSIG` file with
//! `ssh-keygen -Y sign`. Verification is done in-process against the pinned raw
//! Ed25519 key; there is no `ssh-keygen`, `allowed_signers`, certificate,
//! expiry date, or online lookup at install time.

use crate::update::keys::TrustedKey;
use crate::update::{Result, UpdateError};

/// Largest signature file accepted. A real SSHSIG is a few hundred bytes.
pub const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;

/// A signature that verified against a pinned key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSignature {
    /// The trusted key ID that matched the signature's public key.
    pub key_id: String,
}

/// Verify `message` against the OpenSSH signature in `signature_bytes`.
///
/// `trusted` is the compile-time key list in production and a test key list in
/// tests. An empty list always fails: a build with no pinned key can never
/// install an update.
pub fn verify_signature(
    trusted: &[TrustedKey],
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<VerifiedSignature> {
    if trusted.is_empty() {
        return Err(UpdateError::Verification(
            "no trusted update keys are baked into this build; refusing to verify".to_string(),
        ));
    }
    if signature_bytes.len() as u64 > MAX_SIGNATURE_BYTES {
        return Err(UpdateError::Verification(format!(
            "signature file is larger than {} bytes",
            MAX_SIGNATURE_BYTES
        )));
    }

    super::ssh_signature::verify_ssh_signature(trusted, message, signature_bytes)
}

/// True when the file starts with the OpenSSH signature armor.
pub(crate) fn is_ssh_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"-----BEGIN SSH SIGNATURE-----")
}
