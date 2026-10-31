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

//! OpenSSH `SSHSIG` verification for `.sha256.sig` files.
//!
//! The signing workflow uses `ssh-keygen -Y sign`, which writes the standard
//! armored SSH signature format. The device verifies it with the pinned raw
//! Ed25519 public key; it does not invoke `ssh-keygen` or read an
//! `allowed_signers` file.
//!
//! The signed data is the SSHSIG envelope (raw `"SSHSIG"` magic plus the
//! namespace, reserved field, hash algorithm, and hash of the message), as
//! defined by OpenSSH's PROTOCOL.sshsig.

use crate::update::keys::TrustedKey;
use crate::update::signature::{is_ssh_signature, VerifiedSignature};
use crate::update::{Result, UpdateError};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub const NAMESPACE: &[u8] = b"photo-frame-update";
const MAGIC: &[u8] = b"SSHSIG";
const VERSION: u32 = 1;
const HASH_ALGORITHM: &[u8] = b"sha256";
const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const END: &str = "-----END SSH SIGNATURE-----";

/// Verify an armored OpenSSH signature over `message`.
///
/// The public key embedded in the signature must exactly match one of the
/// trusted keys. The matching key's ID is returned for audit logging.
pub fn verify_ssh_signature(
    trusted: &[TrustedKey],
    message: &[u8],
    armored: &[u8],
) -> Result<VerifiedSignature> {
    if trusted.is_empty() {
        return Err(UpdateError::Verification(
            "no trusted update keys are baked into this build; refusing to verify".to_string(),
        ));
    }
    if !is_ssh_signature(armored) {
        return Err(UpdateError::Verification(
            "signature is not an OpenSSH SSH signature".to_string(),
        ));
    }

    let blob = decode_armor(armored)?;
    if blob.len() < MAGIC.len() || &blob[..MAGIC.len()] != MAGIC {
        return Err(UpdateError::Verification(
            "SSH signature has the wrong magic".to_string(),
        ));
    }

    let mut offset = MAGIC.len();
    let version = read_u32(&blob, &mut offset)?;
    if version != VERSION {
        return Err(UpdateError::Verification(format!(
            "unsupported SSH signature version {}",
            version
        )));
    }

    let public_key_blob = read_string(&blob, &mut offset)?;
    let namespace = read_string(&blob, &mut offset)?;
    let reserved = read_string(&blob, &mut offset)?;
    let hash_algorithm = read_string(&blob, &mut offset)?;
    let signature_blob = read_string(&blob, &mut offset)?;
    if offset != blob.len() {
        return Err(UpdateError::Verification(
            "SSH signature has trailing data".to_string(),
        ));
    }

    if namespace != NAMESPACE {
        return Err(UpdateError::Verification(format!(
            "SSH signature namespace is {:?}, expected {:?}",
            String::from_utf8_lossy(namespace),
            String::from_utf8_lossy(NAMESPACE)
        )));
    }
    if !reserved.is_empty() {
        return Err(UpdateError::Verification(
            "SSH signature reserved field is not empty".to_string(),
        ));
    }
    if hash_algorithm != HASH_ALGORITHM {
        return Err(UpdateError::Verification(format!(
            "unsupported SSH signature hash algorithm {:?}",
            String::from_utf8_lossy(hash_algorithm)
        )));
    }

    let (key_type, key_bytes) = parse_public_key(public_key_blob)?;
    let trusted_key = trusted
        .iter()
        .find(|key| key.bytes == key_bytes)
        .ok_or_else(|| {
            UpdateError::Verification(
                "SSH signature public key is not trusted by this build".to_string(),
            )
        })?;
    if key_type != b"ssh-ed25519" {
        return Err(UpdateError::Verification(
            "SSH signature does not use an Ed25519 key".to_string(),
        ));
    }

    let (signature_type, signature_bytes) = parse_signature(signature_blob)?;
    if signature_type != b"ssh-ed25519" {
        return Err(UpdateError::Verification(
            "SSH signature does not use the ssh-ed25519 algorithm".to_string(),
        ));
    }

    let digest = Sha256::digest(message);
    let mut signed_data = Vec::with_capacity(64 + message.len());
    signed_data.extend_from_slice(MAGIC);
    push_string(&mut signed_data, namespace);
    push_string(&mut signed_data, reserved);
    push_string(&mut signed_data, hash_algorithm);
    push_string(&mut signed_data, &digest);

    let verifying_key = VerifyingKey::from_bytes(&trusted_key.bytes).map_err(|_| {
        UpdateError::Verification(format!(
            "trusted key '{}' is not a valid Ed25519 public key",
            trusted_key.id
        ))
    })?;
    let signature = Signature::from_slice(signature_bytes).map_err(|_| {
        UpdateError::Verification("SSH signature is not a valid Ed25519 signature".to_string())
    })?;
    verifying_key
        .verify_strict(&signed_data, &signature)
        .map_err(|_| {
            UpdateError::Verification(format!(
                "SSH signature verification failed for key '{}'",
                trusted_key.id
            ))
        })?;

    Ok(VerifiedSignature {
        key_id: trusted_key.id.to_string(),
    })
}

fn decode_armor(armored: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(armored)
        .map_err(|_| UpdateError::Verification("SSH signature is not valid UTF-8".to_string()))?;
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(BEGIN) {
        return Err(UpdateError::Verification(
            "SSH signature is missing the BEGIN marker".to_string(),
        ));
    }

    let mut encoded = String::new();
    let mut saw_end = false;
    for line in lines {
        let line = line.trim();
        if line == END {
            saw_end = true;
            break;
        }
        if !line.is_empty() {
            encoded.push_str(line);
        }
    }
    if !saw_end {
        return Err(UpdateError::Verification(
            "SSH signature is missing the END marker".to_string(),
        ));
    }

    STANDARD
        .decode(encoded)
        .map_err(|_| UpdateError::Verification("SSH signature is not valid base64".to_string()))
}

fn parse_public_key(blob: &[u8]) -> Result<(&[u8], [u8; 32])> {
    let mut offset = 0;
    let key_type = read_string(blob, &mut offset)?;
    if key_type != b"ssh-ed25519" {
        return Err(UpdateError::Verification(
            "SSH signature public key is not ssh-ed25519".to_string(),
        ));
    }
    let key = read_string(blob, &mut offset)?;
    if key.len() != 32 {
        return Err(UpdateError::Verification(format!(
            "SSH Ed25519 public key must be 32 bytes, found {}",
            key.len()
        )));
    }
    if offset != blob.len() {
        return Err(UpdateError::Verification(
            "SSH public key has trailing data".to_string(),
        ));
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(key);
    Ok((key_type, bytes))
}

fn parse_signature(blob: &[u8]) -> Result<(&[u8], &[u8])> {
    let mut offset = 0;
    let signature_type = read_string(blob, &mut offset)?;
    let signature = read_string(blob, &mut offset)?;
    if offset != blob.len() {
        return Err(UpdateError::Verification(
            "SSH signature has trailing data".to_string(),
        ));
    }
    if signature.len() != 64 {
        return Err(UpdateError::Verification(format!(
            "SSH Ed25519 signature must be 64 bytes, found {}",
            signature.len()
        )));
    }
    Ok((signature_type, signature))
}

fn read_u32(blob: &[u8], offset: &mut usize) -> Result<u32> {
    let bytes = blob
        .get(*offset..*offset + 4)
        .ok_or_else(|| UpdateError::Verification("SSH signature is truncated".to_string()))?;
    *offset += 4;
    Ok(u32::from_be_bytes(
        bytes
            .try_into()
            .expect("slice of four bytes converts to u32"),
    ))
}

fn read_string<'a>(blob: &'a [u8], offset: &mut usize) -> Result<&'a [u8]> {
    let length = read_u32(blob, offset)? as usize;
    let value = blob
        .get(*offset..*offset + length)
        .ok_or_else(|| UpdateError::Verification("SSH signature is truncated".to_string()))?;
    *offset += length;
    Ok(value)
}

fn push_string(buffer: &mut Vec<u8>, value: &[u8]) {
    buffer.extend_from_slice(&(value.len() as u32).to_be_bytes());
    buffer.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const MESSAGE: &[u8] = b"deadbeef  photo-frame-0.2.1.tar.gz\n";

    fn test_key(id: &'static str, seed: u8) -> (SigningKey, TrustedKey) {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let trusted = TrustedKey {
            id,
            bytes: signing.verifying_key().to_bytes(),
        };
        (signing, trusted)
    }

    fn public_key_blob(key: &[u8; 32]) -> Vec<u8> {
        let mut blob = Vec::new();
        push_string(&mut blob, b"ssh-ed25519");
        push_string(&mut blob, key);
        blob
    }

    fn signature_blob(signature: &[u8]) -> Vec<u8> {
        let mut blob = Vec::new();
        push_string(&mut blob, b"ssh-ed25519");
        push_string(&mut blob, signature);
        blob
    }

    fn armor(blob: &[u8]) -> Vec<u8> {
        let encoded = STANDARD.encode(blob);
        let mut out = String::from(BEGIN);
        out.push('\n');
        for chunk in encoded.as_bytes().chunks(70) {
            out.push_str(std::str::from_utf8(chunk).unwrap());
            out.push('\n');
        }
        out.push_str(END);
        out.push('\n');
        out.into_bytes()
    }

    fn signed_sshsig(
        signing: &SigningKey,
        message: &[u8],
        namespace: &[u8],
        hash_algorithm: &[u8],
    ) -> Vec<u8> {
        let digest = Sha256::digest(message);
        let mut signed_data = Vec::new();
        signed_data.extend_from_slice(MAGIC);
        push_string(&mut signed_data, namespace);
        push_string(&mut signed_data, b"");
        push_string(&mut signed_data, hash_algorithm);
        push_string(&mut signed_data, &digest);
        let signature = signing.sign(&signed_data);

        let mut payload = Vec::new();
        payload.extend_from_slice(MAGIC);
        payload.extend_from_slice(&VERSION.to_be_bytes());
        push_string(
            &mut payload,
            &public_key_blob(&signing.verifying_key().to_bytes()),
        );
        push_string(&mut payload, namespace);
        push_string(&mut payload, b"");
        push_string(&mut payload, hash_algorithm);
        push_string(&mut payload, &signature_blob(&signature.to_bytes()));
        armor(&payload)
    }

    #[test]
    fn test_ssh_signature_verifies_and_returns_key_id() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signed_sshsig(&signing, MESSAGE, NAMESPACE, HASH_ALGORITHM);
        let verified = verify_ssh_signature(&[trusted], MESSAGE, &sig).unwrap();
        assert_eq!(verified.key_id, "test-2026");
    }

    #[test]
    fn test_ssh_signature_rejects_tampered_message() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signed_sshsig(&signing, MESSAGE, NAMESPACE, HASH_ALGORITHM);
        let err = verify_ssh_signature(&[trusted], b"deadbeef  photo-frame-0.2.2.tar.gz\n", &sig)
            .unwrap_err();
        assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
    }

    #[test]
    fn test_ssh_signature_rejects_untrusted_key() {
        let (signing, _trusted) = test_key("test-2026", 7);
        let (_, other) = test_key("other-2026", 8);
        let sig = signed_sshsig(&signing, MESSAGE, NAMESPACE, HASH_ALGORITHM);
        let err = verify_ssh_signature(&[other], MESSAGE, &sig).unwrap_err();
        match err {
            UpdateError::Verification(message) => assert!(message.contains("not trusted")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_ssh_signature_rejects_wrong_namespace() {
        let (signing, trusted) = test_key("test-2026", 7);
        let sig = signed_sshsig(&signing, MESSAGE, b"other", HASH_ALGORITHM);
        let err = verify_ssh_signature(&[trusted], MESSAGE, &sig).unwrap_err();
        match err {
            UpdateError::Verification(message) => assert!(message.contains("namespace")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_ssh_keygen_fixture_verifies() {
        // Generated by:
        //   ssh-keygen -Y sign -f key -n photo-frame-update \
        //     -O hashalg=sha256 checksum
        // The public key is testdata/sshsig.pub; only the raw key is needed
        // here because the verifier reads the embedded key from the signature.
        const KEY: [u8; 32] = [
            0x71, 0x99, 0x03, 0x1b, 0x3d, 0x90, 0xde, 0x1a, 0xfb, 0xa7, 0x0d, 0x3b, 0x63, 0x61,
            0x3d, 0x09, 0x15, 0x1b, 0x50, 0x9c, 0xd7, 0xdc, 0x0f, 0x9f, 0xe9, 0x43, 0x8d, 0xfd,
            0x75, 0xe0, 0x2e, 0x59,
        ];
        let trusted = TrustedKey {
            id: "fixture",
            bytes: KEY,
        };
        let signature = include_bytes!("testdata/sshsig.sig");
        let verified = verify_ssh_signature(&[trusted], MESSAGE, signature).unwrap();
        assert_eq!(verified.key_id, "fixture");
    }

    #[test]
    fn test_ssh_signature_rejects_malformed_armor() {
        let (_signing, trusted) = test_key("test-2026", 7);
        for input in [
            b"not a signature".as_slice(),
            b"-----BEGIN SSH SIGNATURE-----\nAAAA\n".as_slice(),
            b"-----BEGIN SSH SIGNATURE-----\n!!!!\n-----END SSH SIGNATURE-----\n".as_slice(),
        ] {
            let err = verify_ssh_signature(&[trusted], MESSAGE, input).unwrap_err();
            assert!(matches!(err, UpdateError::Verification(_)), "{err:?}");
        }
    }
}
