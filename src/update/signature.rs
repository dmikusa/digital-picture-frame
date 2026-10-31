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

//! Ed25519 verification of `.sha256.sig` files.
//!
//! The release workflow writes an OpenSSH `SSHSIG` file with
//! `ssh-keygen -Y sign`. Verification is done in-process against the pinned raw
//! Ed25519 key; there is no `ssh-keygen`, `allowed_signers`, certificate,
//! expiry date, or online lookup at install time.
//!
//! For compatibility, the original JSON envelope
//! `{"key_id":"2026-10","signature":"<base64url>"}` is still accepted. New
//! releases use the SSHSIG format.

use crate::update::keys::TrustedKey;
use crate::update::{Result, UpdateError};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// Largest signature envelope accepted. The real one is a few hundred bytes.
pub const MAX_SIGNATURE_ENVELOPE_BYTES: u64 = 64 * 1024;

/// Parsed `.sha256.sig` payload.
#[derive(Debug, Deserialize)]
struct SignatureEnvelope {
    key_id: String,
    signature: String,
}

/// A signature that verified against a pinned key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSignature {
    /// The trusted key ID named by the envelope.
    pub key_id: String,
}

/// Verify `message` against the envelope in `envelope_bytes`.
///
/// `trusted` is the compile-time key list in production and a test key list in
/// tests. An empty list always fails: a build with no pinned key can never
/// install an update.
pub fn verify_signature(
    trusted: &[TrustedKey],
    message: &[u8],
    envelope_bytes: &[u8],
) -> Result<VerifiedSignature> {
    if trusted.is_empty() {
        return Err(UpdateError::Verification(
            "no trusted update keys are baked into this build; refusing to verify".to_string(),
        ));
    }
    if envelope_bytes.len() as u64 > MAX_SIGNATURE_ENVELOPE_BYTES {
        return Err(UpdateError::Verification(format!(
            "signature envelope is larger than {} bytes",
            MAX_SIGNATURE_ENVELOPE_BYTES
        )));
    }

    if is_ssh_signature(envelope_bytes) {
        return super::ssh_signature::verify_ssh_signature(trusted, message, envelope_bytes);
    }

    let envelope: SignatureEnvelope = serde_json::from_slice(envelope_bytes).map_err(|e| {
        UpdateError::Verification(format!("signature envelope is not valid JSON: {}", e))
    })?;

    if envelope.key_id.is_empty() {
        return Err(UpdateError::Verification(
            "signature envelope has an empty key_id".to_string(),
        ));
    }

    let trusted_key = trusted
        .iter()
        .find(|key| key.id == envelope.key_id)
        .ok_or_else(|| {
            UpdateError::Verification(format!(
                "signature key id '{}' is not trusted by this build",
                envelope.key_id
            ))
        })?;

    let signature_bytes = decode_base64url(&envelope.signature)?;
    let signature = Signature::from_slice(&signature_bytes).map_err(|_| {
        UpdateError::Verification(format!(
            "signature for key '{}' is not a valid Ed25519 signature",
            envelope.key_id
        ))
    })?;

    let verifying_key = VerifyingKey::from_bytes(&trusted_key.bytes).map_err(|_| {
        UpdateError::Verification(format!(
            "trusted key '{}' is not a valid Ed25519 public key",
            envelope.key_id
        ))
    })?;

    verifying_key
        .verify_strict(message, &signature)
        .map_err(|_| {
            UpdateError::Verification(format!(
                "signature verification failed for key '{}'",
                envelope.key_id
            ))
        })?;

    Ok(VerifiedSignature {
        key_id: envelope.key_id,
    })
}

/// True when the file starts with the OpenSSH signature armor.
pub(crate) fn is_ssh_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"-----BEGIN SSH SIGNATURE-----")
}

fn decode_base64url(encoded: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(encoded.trim())
        .or_else(|_| URL_SAFE.decode(encoded.trim()))
        .map_err(|_| UpdateError::Verification("signature is not valid base64url".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_key(id: &'static str, seed: u8) -> (SigningKey, TrustedKey) {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let trusted = TrustedKey {
            id,
            bytes: signing.verifying_key().to_bytes(),
        };
        (signing, trusted)
    }

    fn envelope(key_id: &str, signature: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "key_id": key_id,
            "signature": URL_SAFE_NO_PAD.encode(signature),
        }))
        .unwrap()
    }

    const MESSAGE: &[u8] = b"deadbeef  photo-frame-0.2.1.tar.gz\n";

    #[test]
    fn test_verify_signature_succeeds() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signing.sign(MESSAGE);
        let verified =
            verify_signature(&[trusted], MESSAGE, &envelope("test-2026", &sig.to_bytes()))
                .expect("signature should verify");
        assert_eq!(verified.key_id, "test-2026");
    }

    #[test]
    fn test_verify_signature_rejects_tampered_message() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signing.sign(MESSAGE);
        let err = verify_signature(
            &[trusted],
            b"deadbeef  photo-frame-0.2.2.tar.gz\n",
            &envelope("test-2026", &sig.to_bytes()),
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_verify_signature_rejects_tampered_signature() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signing.sign(MESSAGE);
        let mut bytes = sig.to_bytes();
        bytes[0] ^= 0xff;
        let err =
            verify_signature(&[trusted], MESSAGE, &envelope("test-2026", &bytes)).unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_verify_signature_rejects_unknown_key_id() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signing.sign(MESSAGE);
        let err = verify_signature(&[trusted], MESSAGE, &envelope("other-key", &sig.to_bytes()))
            .unwrap_err();
        match err {
            UpdateError::Verification(message) => assert!(message.contains("not trusted")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_verify_signature_fails_closed_without_keys() {
        let (signing, _trusted) = test_key("test-2026", 7);
        let sig = signing.sign(MESSAGE);
        let err =
            verify_signature(&[], MESSAGE, &envelope("test-2026", &sig.to_bytes())).unwrap_err();
        match err {
            UpdateError::Verification(message) => {
                assert!(message.contains("no trusted update keys"))
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_verify_signature_rejects_malformed_envelope() {
        let (_signing, trusted) = test_key("test-2026", 7);
        let err = verify_signature(&[trusted], MESSAGE, b"not json").unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");

        let err = verify_signature(
            &[trusted],
            MESSAGE,
            br#"{"key_id":"test-2026","signature":"!!!"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_verify_signature_accepts_padded_base64url() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signing.sign(MESSAGE);
        let encoded = URL_SAFE.encode(sig.to_bytes());
        let envelope = serde_json::to_vec(&serde_json::json!({
            "key_id": "test-2026",
            "signature": encoded,
        }))
        .unwrap();
        assert!(verify_signature(&[trusted], MESSAGE, &envelope).is_ok());
    }
}
