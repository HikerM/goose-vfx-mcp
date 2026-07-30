use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use goose_verified_sources::{
    frame, parse_and_verify_envelope, ChainLookup, Digest32, DocKind, Document, Envelope,
    ExistingSequence, LookupState, Release, ReleaseKey, ReleaseState, ResolvedDocument, Root,
    RootKey, SequenceDisposition, SignatureEntry, State, VerificationContext, VerifiedEnvelope,
};
use sha2::{Digest, Sha256};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Default)]
pub struct MemoryContext {
    entries: BTreeMap<(String, u64), Vec<StoredVariant>>,
}

#[derive(Clone)]
struct StoredVariant {
    document_digest: Digest32,
    signature_set_digest: Digest32,
    resolved: ResolvedDocument,
}

impl MemoryContext {
    pub fn store(&mut self, verified: &VerifiedEnvelope) {
        self.entries
            .entry((
                verified.envelope.payload.source_id.clone(),
                verified.envelope.payload.sequence,
            ))
            .or_default()
            .push(StoredVariant {
                document_digest: verified.digests.document,
                signature_set_digest: verified.digests.signature_set,
                resolved: ResolvedDocument {
                    document_digest: verified.digests.document,
                    root_digest: verified.digests.root,
                    state_digest: verified.digests.state,
                    canonical_root: verified.canonical_root.clone(),
                    canonical_state: verified.canonical_state.clone(),
                },
            });
    }
}

impl ChainLookup for MemoryContext {
    fn lookup(&self, source_id: &str, sequence: u64) -> LookupState {
        let Some(entries) = self.entries.get(&(source_id.to_string(), sequence)) else {
            return LookupState::Absent;
        };
        let first = entries[0].document_digest;
        if entries.iter().any(|entry| entry.document_digest != first) {
            return LookupState::Fork;
        }
        LookupState::Verified(entries[0].resolved.clone())
    }
}

impl VerificationContext for MemoryContext {
    fn classify_existing(
        &self,
        source_id: &str,
        sequence: u64,
        document_digest: &Digest32,
        signature_set_digest: &Digest32,
    ) -> ExistingSequence {
        let Some(entries) = self.entries.get(&(source_id.to_string(), sequence)) else {
            return ExistingSequence::Absent;
        };
        if entries
            .iter()
            .any(|entry| entry.document_digest != *document_digest)
        {
            return ExistingSequence::Fork;
        }
        if entries
            .iter()
            .any(|entry| entry.signature_set_digest == *signature_set_digest)
        {
            ExistingSequence::Duplicate
        } else {
            ExistingSequence::EquivalentVariant
        }
    }
}

pub fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn root_key(seed: u8) -> (RootKey, SigningKey) {
    let signing_key = signing_key(seed);
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(&signing_key.verifying_key().to_bytes());
    (
        RootKey {
            kid: format!("ed25519-spki-sha256:{}", sha256_hex(&der)),
            spki_der_b64u: URL_SAFE_NO_PAD.encode(der),
        },
        signing_key,
    )
}

pub fn active_release(
    release_id: &str,
    mcp_id: &str,
    version: &str,
    manifest_kind: &str,
    platform: &str,
    architecture: &str,
    variant: &str,
) -> Release {
    Release {
        architecture: architecture.to_string(),
        manifest_kind: manifest_kind.to_string(),
        mcp_id: mcp_id.to_string(),
        platform: platform.to_string(),
        release_id: release_id.to_string(),
        variant: variant.to_string(),
        version: version.to_string(),
        state: ReleaseState::Active,
    }
}

pub fn revoked_release(
    release_id: &str,
    mcp_id: &str,
    version: &str,
    manifest_kind: &str,
    platform: &str,
    architecture: &str,
    variant: &str,
    reason: &str,
    revoked_at_sequence: u64,
) -> Release {
    Release {
        architecture: architecture.to_string(),
        manifest_kind: manifest_kind.to_string(),
        mcp_id: mcp_id.to_string(),
        platform: platform.to_string(),
        release_id: release_id.to_string(),
        variant: variant.to_string(),
        version: version.to_string(),
        state: ReleaseState::Revoked {
            reason: reason.to_string(),
            revoked_at_sequence,
        },
    }
}

pub fn root(keys: &[RootKey], quorum: u64) -> Root {
    let mut keys = keys.to_vec();
    keys.sort_by(|left, right| left.kid.cmp(&right.kid));
    Root { keys, quorum }
}

pub fn state(releases: &[Release]) -> State {
    let mut releases = releases.to_vec();
    releases.sort_by(|left, right| left.key().cmp(&right.key()));
    State { releases }
}

pub fn bootstrap(source_id: &str, root: Root, state: State) -> Document {
    Document {
        doc_kind: DocKind::Bootstrap,
        parent_document_digest: None,
        prior_root: None,
        root,
        sequence: 0,
        source_id: source_id.to_string(),
        state,
        wire_version: "v2".to_string(),
    }
}

pub fn snapshot(
    source_id: &str,
    sequence: u64,
    parent_digest: Digest32,
    root: Root,
    state: State,
) -> Document {
    Document {
        doc_kind: DocKind::Snapshot,
        parent_document_digest: Some(parent_digest),
        prior_root: None,
        root,
        sequence,
        source_id: source_id.to_string(),
        state,
        wire_version: "v2".to_string(),
    }
}

pub fn rotation(
    source_id: &str,
    sequence: u64,
    parent_digest: Digest32,
    prior_root: Root,
    root: Root,
    state: State,
) -> Document {
    Document {
        doc_kind: DocKind::Rotation,
        parent_document_digest: Some(parent_digest),
        prior_root: Some(prior_root),
        root,
        sequence,
        source_id: source_id.to_string(),
        state,
        wire_version: "v2".to_string(),
    }
}

pub fn revocation(
    source_id: &str,
    sequence: u64,
    parent_digest: Digest32,
    root: Root,
    state: State,
) -> Document {
    Document {
        doc_kind: DocKind::Revocation,
        parent_document_digest: Some(parent_digest),
        prior_root: None,
        root,
        sequence,
        source_id: source_id.to_string(),
        state,
        wire_version: "v2".to_string(),
    }
}

pub fn signed_envelope(payload: Document, signers: &[(&RootKey, &SigningKey)]) -> Envelope {
    let message = signature_message_digest(&payload);
    let mut signatures = signers
        .iter()
        .map(|(root_key, signing_key)| SignatureEntry {
            kid: root_key.kid.clone(),
            sig_b64u: URL_SAFE_NO_PAD.encode(signing_key.sign(&message).to_bytes()),
        })
        .collect::<Vec<_>>();
    signatures.sort_by(|left, right| left.kid.cmp(&right.kid));
    Envelope {
        payload,
        signatures,
    }
}

pub fn parse_verified(envelope: Envelope) -> VerifiedEnvelope {
    parse_and_verify_envelope(&envelope.to_canonical_json_bytes().unwrap()).unwrap()
}

pub fn signature_message_digest(payload: &Document) -> [u8; 32] {
    let payload_bytes = payload.to_canonical_json_bytes().unwrap();
    let document_digest = Sha256::digest(frame("document", &payload_bytes));
    Sha256::digest(frame("signature-message", &document_digest)).into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        out.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    out
}

pub fn as_map(releases: &[Release]) -> BTreeMap<ReleaseKey, Release> {
    releases
        .iter()
        .cloned()
        .map(|release| (release.key(), release))
        .collect()
}

pub fn assert_accepted(disposition: SequenceDisposition) {
    assert_eq!(disposition, SequenceDisposition::Accepted);
}
