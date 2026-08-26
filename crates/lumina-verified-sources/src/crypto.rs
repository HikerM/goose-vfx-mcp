use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};

use crate::canonical::{digest_frame, hex_encode, Digest32};
use crate::error::{ErrorCode, Result};
use crate::wire::{DocKind, Root, RootKey, SignatureEntry};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const ROOT_KID_PREFIX: &str = "ed25519-spki-sha256:";

pub(crate) fn signature_message_digest(document_digest: Digest32) -> Digest32 {
    digest_frame("signature-message", document_digest.as_bytes())
}

pub(crate) fn validate_root_key(root_key: &RootKey) -> Result<()> {
    validate_kid(&root_key.kid)?;
    let decoded = decode_b64u_exact(&root_key.spki_der_b64u, 44, 59)?;
    if decoded[..ED25519_SPKI_PREFIX.len()] != ED25519_SPKI_PREFIX {
        return Err(ErrorCode::InvalidSpki);
    }
    let expected = format!(
        "{ROOT_KID_PREFIX}{}",
        hex_encode(digest_sha256(&decoded).as_slice())
    );
    if root_key.kid != expected {
        return Err(ErrorCode::InvalidKid);
    }
    let key_bytes: [u8; 32] = decoded[ED25519_SPKI_PREFIX.len()..]
        .try_into()
        .map_err(|_| ErrorCode::InvalidSpki)?;
    let _ = VerifyingKey::from_bytes(&key_bytes).map_err(|_| ErrorCode::InvalidSpki)?;
    Ok(())
}

pub(crate) fn verify_signature_membership(
    doc_kind: DocKind,
    current_root: &Root,
    prior_root: Option<&Root>,
    signatures: &[SignatureEntry],
) -> Result<()> {
    let mut allowed = BTreeMap::new();
    for key in &current_root.keys {
        allowed.insert(key.kid.as_str(), ());
    }
    if doc_kind == DocKind::Rotation {
        if let Some(root) = prior_root {
            for key in &root.keys {
                allowed.insert(key.kid.as_str(), ());
            }
        }
    }
    for signature in signatures {
        if !allowed.contains_key(signature.kid.as_str()) {
            return Err(ErrorCode::UnknownSignatureKey);
        }
    }
    Ok(())
}

pub(crate) fn verify_signature_quorum(
    root: &Root,
    signatures: &[SignatureEntry],
    message: Digest32,
) -> Result<()> {
    let verifying_keys = root
        .keys
        .iter()
        .map(|key| verifying_key(key).map(|value| (key.kid.as_str(), value)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut valid = 0usize;
    for signature in signatures {
        if let Some(verifying_key) = verifying_keys.get(signature.kid.as_str()) {
            let bytes = decode_b64u_exact(&signature.sig_b64u, 64, 86)?;
            let raw: [u8; 64] = bytes.try_into().map_err(|_| ErrorCode::InvalidSignature)?;
            let signature = Signature::from_bytes(&raw);
            verifying_key
                .verify_strict(message.as_bytes(), &signature)
                .map_err(|_| ErrorCode::InvalidSignature)?;
            valid += 1;
        }
    }
    if valid < root.quorum as usize {
        return Err(ErrorCode::SignatureQuorumNotMet);
    }
    Ok(())
}

pub(crate) fn validate_kid(value: &str) -> Result<()> {
    if value.len() != ROOT_KID_PREFIX.len() + 64 {
        return Err(ErrorCode::InvalidKid);
    }
    if !value.starts_with(ROOT_KID_PREFIX) {
        return Err(ErrorCode::InvalidKid);
    }
    if value.as_bytes().iter().any(|byte| !byte.is_ascii()) {
        return Err(ErrorCode::InvalidKid);
    }
    if value.as_bytes()[ROOT_KID_PREFIX.len()..]
        .iter()
        .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(ErrorCode::InvalidKid);
    }
    Ok(())
}

pub(crate) fn decode_b64u_exact(
    value: &str,
    expected_len: usize,
    expected_text_len: usize,
) -> Result<Vec<u8>> {
    if value.len() != expected_text_len || value.contains('=') || !value.is_ascii() {
        return Err(ErrorCode::InvalidBase64);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .map_err(|_| ErrorCode::InvalidBase64)?;
    if decoded.len() != expected_len {
        return Err(ErrorCode::InvalidBase64);
    }
    if URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(ErrorCode::InvalidBase64);
    }
    Ok(decoded)
}

pub(crate) fn verifying_key(root_key: &RootKey) -> Result<VerifyingKey> {
    let decoded = decode_b64u_exact(&root_key.spki_der_b64u, 44, 59)?;
    if decoded[..ED25519_SPKI_PREFIX.len()] != ED25519_SPKI_PREFIX {
        return Err(ErrorCode::InvalidSpki);
    }
    let key_bytes: [u8; 32] = decoded[ED25519_SPKI_PREFIX.len()..]
        .try_into()
        .map_err(|_| ErrorCode::InvalidSpki)?;
    VerifyingKey::from_bytes(&key_bytes).map_err(|_| ErrorCode::InvalidSpki)
}

fn digest_sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}
