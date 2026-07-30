use std::collections::BTreeSet;

use crate::canonical::{digest_frame, Digest32};
use crate::crypto::{
    signature_message_digest, validate_kid, validate_root_key, verify_signature_membership,
    verify_signature_quorum,
};
use crate::error::{ErrorCode, Result};
use crate::json::{parse_json_bytes, write_json_string, JsonValue};

pub type BoundedAsciiPath = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Bootstrap,
    Snapshot,
    Rotation,
    Revocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub payload: Document,
    pub signatures: Vec<SignatureEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub doc_kind: DocKind,
    pub parent_document_digest: Option<Digest32>,
    pub prior_root: Option<Root>,
    pub root: Root,
    pub sequence: u64,
    pub source_id: String,
    pub state: State,
    pub wire_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub keys: Vec<RootKey>,
    pub quorum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootKey {
    pub kid: String,
    pub spki_der_b64u: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureEntry {
    pub kid: String,
    pub sig_b64u: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub releases: Vec<Release>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseKey {
    pub mcp_id: String,
    pub version: String,
    pub manifest_kind: String,
    pub platform: String,
    pub architecture: String,
    pub variant: String,
    pub release_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseState {
    Active,
    Revoked {
        reason: String,
        revoked_at_sequence: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub architecture: String,
    pub manifest_kind: String,
    pub mcp_id: String,
    pub platform: String,
    pub release_id: String,
    pub variant: String,
    pub version: String,
    pub state: ReleaseState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentDigests {
    pub document: Digest32,
    pub root: Digest32,
    pub state: Digest32,
    pub signature_set: Digest32,
    pub signature_message: Digest32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEnvelope {
    pub raw_bytes: Vec<u8>,
    pub envelope: Envelope,
    pub canonical_payload: Vec<u8>,
    pub canonical_root: Vec<u8>,
    pub canonical_state: Vec<u8>,
    pub canonical_signatures: Vec<u8>,
    pub canonical_envelope: Vec<u8>,
    pub digests: DocumentDigests,
}

pub fn parse_and_verify_envelope(raw: &[u8]) -> Result<VerifiedEnvelope> {
    let envelope = Envelope::from_json(parse_json_bytes(raw)?)?;
    envelope.validate()?;

    let canonical_payload = envelope.payload.to_canonical_json_bytes_unchecked();
    let canonical_root = envelope.payload.root.to_canonical_json_bytes_unchecked();
    let canonical_state = envelope.payload.state.to_canonical_json_bytes_unchecked();
    let canonical_signatures = canonical_signature_array(&envelope.signatures);
    let canonical_envelope = envelope.to_canonical_json_bytes_unchecked();

    let document = digest_frame("document", &canonical_payload);
    let root = digest_frame("root", &canonical_root);
    let state = digest_frame("state", &canonical_state);
    let signature_set = digest_frame("signature-set", &canonical_signatures);
    let signature_message = signature_message_digest(document);

    verify_signature_quorum(
        &envelope.payload.root,
        &envelope.signatures,
        signature_message,
    )?;

    Ok(VerifiedEnvelope {
        raw_bytes: raw.to_vec(),
        envelope,
        canonical_payload,
        canonical_root,
        canonical_state,
        canonical_signatures,
        canonical_envelope,
        digests: DocumentDigests {
            document,
            root,
            state,
            signature_set,
            signature_message,
        },
    })
}

impl Envelope {
    fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let payload = Document::from_json(take_required(&mut object, "payload")?)?;
        let signatures = take_array(take_required(&mut object, "signatures")?)?
            .into_iter()
            .map(SignatureEntry::from_json)
            .collect::<Result<Vec<_>>>()?;
        reject_unknown(object)?;
        Ok(Self {
            payload,
            signatures,
        })
    }

    fn validate(&self) -> Result<()> {
        ensure_signature_array_sorted(&self.signatures)?;
        if self.signatures.len() > 64 {
            return Err(ErrorCode::TooManySignatures);
        }
        for signature in &self.signatures {
            validate_kid(&signature.kid)?;
            validate_ascii(&signature.sig_b64u)?;
            if signature.sig_b64u.len() != 86 {
                return Err(ErrorCode::InvalidSignature);
            }
        }
        self.payload.validate()?;
        verify_signature_membership(
            self.payload.doc_kind,
            &self.payload.root,
            self.payload.prior_root.as_ref(),
            &self.signatures,
        )?;
        Ok(())
    }

    pub fn to_canonical_json_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(self.to_canonical_json_bytes_unchecked())
    }

    fn to_canonical_json_bytes_unchecked(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(b'{');
        write_json_string(&mut out, "payload");
        out.push(b':');
        self.payload.write_canonical_json(&mut out);
        out.push(b',');
        write_json_string(&mut out, "signatures");
        out.push(b':');
        write_signature_array(&mut out, &self.signatures);
        out.push(b'}');
        out
    }
}

impl Document {
    fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let doc_kind = parse_doc_kind(peek_required_string(&object, "doc_kind")?)?;

        let doc_kind_value = take_string(take_required(&mut object, "doc_kind")?)?;
        let parent = take_required(&mut object, "parent_document_digest")?;
        let prior_root = match doc_kind {
            DocKind::Rotation => Some(Root::from_json(take_required(&mut object, "prior_root")?)?),
            _ => None,
        };
        let root = Root::from_json(take_required(&mut object, "root")?)?;
        let sequence = take_number(take_required(&mut object, "sequence")?)?;
        let source_id = take_string(take_required(&mut object, "source_id")?)?;
        let state = State::from_json(take_required(&mut object, "state")?)?;
        let wire_version = take_string(take_required(&mut object, "wire_version")?)?;
        reject_unknown(object)?;

        if doc_kind_value != doc_kind.as_str() {
            return Err(ErrorCode::InvalidDocKind);
        }
        if wire_version != "v2" {
            return Err(ErrorCode::InvalidWireVersion);
        }
        validate_source_id(&source_id)?;

        let parent_document_digest = match doc_kind {
            DocKind::Bootstrap => {
                take_null(parent)?;
                if sequence != 0 {
                    return Err(ErrorCode::InvalidSequence);
                }
                None
            }
            DocKind::Snapshot | DocKind::Rotation | DocKind::Revocation => {
                if sequence == 0 {
                    return Err(ErrorCode::InvalidSequence);
                }
                Some(parse_digest_hex(take_string(parent)?)?)
            }
        };

        Ok(Self {
            doc_kind,
            parent_document_digest,
            prior_root,
            root,
            sequence,
            source_id,
            state,
            wire_version,
        })
    }

    fn validate(&self) -> Result<()> {
        if self.wire_version != "v2" {
            return Err(ErrorCode::InvalidWireVersion);
        }
        validate_source_id(&self.source_id)?;
        match self.doc_kind {
            DocKind::Bootstrap => {
                if self.parent_document_digest.is_some() {
                    return Err(ErrorCode::WrongType);
                }
                if self.sequence != 0 {
                    return Err(ErrorCode::InvalidSequence);
                }
            }
            DocKind::Snapshot | DocKind::Rotation | DocKind::Revocation => {
                if self.parent_document_digest.is_none() {
                    return Err(ErrorCode::MissingField);
                }
                if self.sequence == 0 {
                    return Err(ErrorCode::InvalidSequence);
                }
            }
        }
        match (&self.doc_kind, &self.prior_root) {
            (DocKind::Rotation, None) => return Err(ErrorCode::MissingField),
            (DocKind::Rotation, Some(prior_root)) => prior_root.validate()?,
            (_, Some(_)) => return Err(ErrorCode::UnknownField),
            (_, None) => {}
        }
        self.root.validate()?;
        self.state.validate()?;
        Ok(())
    }

    pub fn to_canonical_json_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(self.to_canonical_json_bytes_unchecked())
    }

    fn to_canonical_json_bytes_unchecked(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical_json(&mut out);
        out
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "doc_kind");
        out.push(b':');
        write_json_string(out, self.doc_kind.as_str());
        out.push(b',');
        write_json_string(out, "parent_document_digest");
        out.push(b':');
        match self.parent_document_digest {
            Some(digest) => write_json_string(out, &digest.to_hex()),
            None => out.extend_from_slice(b"null"),
        }
        if let Some(prior_root) = &self.prior_root {
            out.push(b',');
            write_json_string(out, "prior_root");
            out.push(b':');
            prior_root.write_canonical_json(out);
        }
        out.push(b',');
        write_json_string(out, "root");
        out.push(b':');
        self.root.write_canonical_json(out);
        out.push(b',');
        write_json_string(out, "sequence");
        out.push(b':');
        out.extend_from_slice(self.sequence.to_string().as_bytes());
        out.push(b',');
        write_json_string(out, "source_id");
        out.push(b':');
        write_json_string(out, &self.source_id);
        out.push(b',');
        write_json_string(out, "state");
        out.push(b':');
        self.state.write_canonical_json(out);
        out.push(b',');
        write_json_string(out, "wire_version");
        out.push(b':');
        write_json_string(out, &self.wire_version);
        out.push(b'}');
    }
}

impl DocKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap",
            Self::Snapshot => "snapshot",
            Self::Rotation => "rotation",
            Self::Revocation => "revocation",
        }
    }
}

impl Root {
    pub(crate) fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let keys = take_array(take_required(&mut object, "keys")?)?
            .into_iter()
            .map(RootKey::from_json)
            .collect::<Result<Vec<_>>>()?;
        let quorum = take_number(take_required(&mut object, "quorum")?)?;
        reject_unknown(object)?;
        Ok(Self { keys, quorum })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.keys.len() > 32 {
            return Err(ErrorCode::TooManyRootKeys);
        }
        ensure_root_keys_sorted(&self.keys)?;
        for key in &self.keys {
            key.validate()?;
        }
        if self.quorum == 0 || self.quorum as usize > self.keys.len() || self.quorum > 32 {
            return Err(ErrorCode::InvalidQuorum);
        }
        Ok(())
    }

    pub fn to_canonical_json_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(self.to_canonical_json_bytes_unchecked())
    }

    fn to_canonical_json_bytes_unchecked(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical_json(&mut out);
        out
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "keys");
        out.push(b':');
        write_root_key_array(out, &self.keys);
        out.push(b',');
        write_json_string(out, "quorum");
        out.push(b':');
        out.extend_from_slice(self.quorum.to_string().as_bytes());
        out.push(b'}');
    }

    pub(crate) fn spki_set(&self) -> BTreeSet<&str> {
        self.keys
            .iter()
            .map(|key| key.spki_der_b64u.as_str())
            .collect()
    }

    pub(crate) fn parse_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let root = Self::from_json(parse_json_bytes(bytes)?)?;
        root.validate()?;
        Ok(root)
    }
}

impl RootKey {
    fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let kid = take_string(take_required(&mut object, "kid")?)?;
        let spki_der_b64u = take_string(take_required(&mut object, "spki_der_b64u")?)?;
        reject_unknown(object)?;
        Ok(Self { kid, spki_der_b64u })
    }

    fn validate(&self) -> Result<()> {
        validate_ascii(&self.kid)?;
        validate_ascii(&self.spki_der_b64u)?;
        validate_root_key(self)
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "kid");
        out.push(b':');
        write_json_string(out, &self.kid);
        out.push(b',');
        write_json_string(out, "spki_der_b64u");
        out.push(b':');
        write_json_string(out, &self.spki_der_b64u);
        out.push(b'}');
    }
}

impl SignatureEntry {
    fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let kid = take_string(take_required(&mut object, "kid")?)?;
        let sig_b64u = take_string(take_required(&mut object, "sig_b64u")?)?;
        reject_unknown(object)?;
        Ok(Self { kid, sig_b64u })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "kid");
        out.push(b':');
        write_json_string(out, &self.kid);
        out.push(b',');
        write_json_string(out, "sig_b64u");
        out.push(b':');
        write_json_string(out, &self.sig_b64u);
        out.push(b'}');
    }
}

impl State {
    pub(crate) fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let releases = take_array(take_required(&mut object, "releases")?)?
            .into_iter()
            .map(Release::from_json)
            .collect::<Result<Vec<_>>>()?;
        reject_unknown(object)?;
        Ok(Self { releases })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure_releases_sorted(&self.releases)?;
        Ok(())
    }

    pub fn to_canonical_json_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(self.to_canonical_json_bytes_unchecked())
    }

    fn to_canonical_json_bytes_unchecked(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical_json(&mut out);
        out
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "releases");
        out.push(b':');
        write_release_array(out, &self.releases);
        out.push(b'}');
    }

    pub(crate) fn parse_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let state = Self::from_json(parse_json_bytes(bytes)?)?;
        state.validate()?;
        Ok(state)
    }
}

impl Release {
    fn from_json(value: JsonValue) -> Result<Self> {
        let mut object = take_object(value)?;
        let has_reason = contains_key(&object, "reason");
        let has_revoked_at = contains_key(&object, "revoked_at_sequence");
        if has_reason != has_revoked_at {
            return Err(ErrorCode::MissingField);
        }

        let architecture = take_string(take_required(&mut object, "architecture")?)?;
        let manifest_kind = take_string(take_required(&mut object, "manifest_kind")?)?;
        let mcp_id = take_string(take_required(&mut object, "mcp_id")?)?;
        let platform = take_string(take_required(&mut object, "platform")?)?;
        let state = if has_reason {
            let reason = take_string(take_required(&mut object, "reason")?)?;
            let release_id = take_string(take_required(&mut object, "release_id")?)?;
            let revoked_at_sequence =
                take_number(take_required(&mut object, "revoked_at_sequence")?)?;
            let variant = take_string(take_required(&mut object, "variant")?)?;
            let version = take_string(take_required(&mut object, "version")?)?;
            reject_unknown(object)?;
            let release = Self {
                architecture,
                manifest_kind,
                mcp_id,
                platform,
                release_id,
                variant,
                version,
                state: ReleaseState::Revoked {
                    reason,
                    revoked_at_sequence,
                },
            };
            release.validate()?;
            return Ok(release);
        } else {
            ReleaseState::Active
        };
        let release_id = take_string(take_required(&mut object, "release_id")?)?;
        let variant = take_string(take_required(&mut object, "variant")?)?;
        let version = take_string(take_required(&mut object, "version")?)?;
        reject_unknown(object)?;
        let release = Self {
            architecture,
            manifest_kind,
            mcp_id,
            platform,
            release_id,
            variant,
            version,
            state,
        };
        release.validate()?;
        Ok(release)
    }

    fn validate(&self) -> Result<()> {
        validate_ascii(&self.architecture)?;
        validate_ascii(&self.manifest_kind)?;
        validate_ascii(&self.platform)?;
        validate_ascii(&self.variant)?;
        validate_release_id(&self.release_id)?;
        validate_mcp_id(&self.mcp_id)?;
        validate_version(&self.version)?;
        if let ReleaseState::Revoked { reason, .. } = &self.state {
            validate_ascii(reason)?;
        }
        Ok(())
    }

    pub fn key(&self) -> ReleaseKey {
        ReleaseKey {
            mcp_id: self.mcp_id.clone(),
            version: self.version.clone(),
            manifest_kind: self.manifest_kind.clone(),
            platform: self.platform.clone(),
            architecture: self.architecture.clone(),
            variant: self.variant.clone(),
            release_id: self.release_id.clone(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, ReleaseState::Active)
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "architecture");
        out.push(b':');
        write_json_string(out, &self.architecture);
        out.push(b',');
        write_json_string(out, "manifest_kind");
        out.push(b':');
        write_json_string(out, &self.manifest_kind);
        out.push(b',');
        write_json_string(out, "mcp_id");
        out.push(b':');
        write_json_string(out, &self.mcp_id);
        out.push(b',');
        write_json_string(out, "platform");
        out.push(b':');
        write_json_string(out, &self.platform);
        if let ReleaseState::Revoked {
            reason,
            revoked_at_sequence,
        } = &self.state
        {
            out.push(b',');
            write_json_string(out, "reason");
            out.push(b':');
            write_json_string(out, reason);
            out.push(b',');
            write_json_string(out, "release_id");
            out.push(b':');
            write_json_string(out, &self.release_id);
            out.push(b',');
            write_json_string(out, "revoked_at_sequence");
            out.push(b':');
            out.extend_from_slice(revoked_at_sequence.to_string().as_bytes());
        } else {
            out.push(b',');
            write_json_string(out, "release_id");
            out.push(b':');
            write_json_string(out, &self.release_id);
        }
        out.push(b',');
        write_json_string(out, "variant");
        out.push(b':');
        write_json_string(out, &self.variant);
        out.push(b',');
        write_json_string(out, "version");
        out.push(b':');
        write_json_string(out, &self.version);
        out.push(b'}');
    }
}

fn parse_doc_kind(value: String) -> Result<DocKind> {
    match value.as_str() {
        "bootstrap" => Ok(DocKind::Bootstrap),
        "snapshot" => Ok(DocKind::Snapshot),
        "rotation" => Ok(DocKind::Rotation),
        "revocation" => Ok(DocKind::Revocation),
        _ => Err(ErrorCode::InvalidDocKind),
    }
}

fn parse_digest_hex(value: String) -> Result<Digest32> {
    Digest32::from_lower_hex(&value).ok_or(ErrorCode::InvalidParentDigest)
}

fn validate_ascii(value: &str) -> Result<()> {
    if value
        .as_bytes()
        .iter()
        .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
    {
        return Err(ErrorCode::InvalidAscii);
    }
    Ok(())
}

fn validate_source_id(value: &str) -> Result<()> {
    validate_restricted_path(value).map_err(|_| ErrorCode::InvalidSourceId)
}

fn validate_release_id(value: &str) -> Result<()> {
    validate_restricted_path(value).map_err(|_| ErrorCode::InvalidReleaseId)
}

fn validate_restricted_path(value: &str) -> Result<()> {
    validate_ascii(value)?;
    if value.is_empty() || value.len() > 256 {
        return Err(ErrorCode::InvalidSourceId);
    }
    for segment in value.split('/') {
        if segment.is_empty() || segment.len() > 63 {
            return Err(ErrorCode::InvalidSourceId);
        }
        let bytes = segment.as_bytes();
        if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
            return Err(ErrorCode::InvalidSourceId);
        }
        if bytes.len() > 2 {
            for byte in &bytes[1..bytes.len() - 1] {
                if !matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-') {
                    return Err(ErrorCode::InvalidSourceId);
                }
            }
        }
    }
    Ok(())
}

fn validate_mcp_id(value: &str) -> Result<()> {
    validate_ascii(value)?;
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return Err(ErrorCode::InvalidMcpId);
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(ErrorCode::InvalidMcpId);
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return Err(ErrorCode::InvalidMcpId);
    }
    if bytes.len() > 2 {
        for byte in &bytes[1..bytes.len() - 1] {
            if !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'/' | b'-') {
                return Err(ErrorCode::InvalidMcpId);
            }
        }
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<()> {
    validate_ascii(value)?;
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return Err(ErrorCode::InvalidVersion);
    }
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return Err(ErrorCode::InvalidVersion);
    }
    if bytes.len() > 2 {
        for byte in &bytes[1..bytes.len() - 1] {
            if !matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'+' | b'-')
            {
                return Err(ErrorCode::InvalidVersion);
            }
        }
    }
    Ok(())
}

fn ensure_root_keys_sorted(keys: &[RootKey]) -> Result<()> {
    ensure_sorted_unique(keys.iter().map(|key| key.kid.as_str()))
}

fn ensure_signature_array_sorted(signatures: &[SignatureEntry]) -> Result<()> {
    ensure_sorted_unique(signatures.iter().map(|signature| signature.kid.as_str()))
}

fn ensure_releases_sorted(releases: &[Release]) -> Result<()> {
    ensure_sorted_unique(releases.iter().map(|release| release.key()))
}

fn ensure_sorted_unique<T>(values: impl IntoIterator<Item = T>) -> Result<()>
where
    T: Ord,
{
    let mut previous: Option<T> = None;
    for value in values {
        if let Some(ref prev) = previous {
            if value < *prev {
                return Err(ErrorCode::UnsortedArray);
            }
            if value == *prev {
                return Err(ErrorCode::DuplicateArrayEntry);
            }
        }
        previous = Some(value);
    }
    Ok(())
}

fn canonical_signature_array(signatures: &[SignatureEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    write_signature_array(&mut out, signatures);
    out
}

fn write_root_key_array(out: &mut Vec<u8>, keys: &[RootKey]) {
    out.push(b'[');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        key.write_canonical_json(out);
    }
    out.push(b']');
}

fn write_signature_array(out: &mut Vec<u8>, signatures: &[SignatureEntry]) {
    out.push(b'[');
    for (index, signature) in signatures.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        signature.write_canonical_json(out);
    }
    out.push(b']');
}

fn write_release_array(out: &mut Vec<u8>, releases: &[Release]) {
    out.push(b'[');
    for (index, release) in releases.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        release.write_canonical_json(out);
    }
    out.push(b']');
}

fn take_object(value: JsonValue) -> Result<Vec<(String, JsonValue)>> {
    match value {
        JsonValue::Object(value) => Ok(value),
        _ => Err(ErrorCode::WrongType),
    }
}

fn take_array(value: JsonValue) -> Result<Vec<JsonValue>> {
    match value {
        JsonValue::Array(value) => Ok(value),
        _ => Err(ErrorCode::WrongType),
    }
}

fn take_string(value: JsonValue) -> Result<String> {
    match value {
        JsonValue::String(value) => {
            validate_ascii(&value)?;
            Ok(value)
        }
        _ => Err(ErrorCode::WrongType),
    }
}

fn take_number(value: JsonValue) -> Result<u64> {
    match value {
        JsonValue::Number(value) => Ok(value),
        _ => Err(ErrorCode::WrongType),
    }
}

fn take_null(value: JsonValue) -> Result<()> {
    match value {
        JsonValue::Null => Ok(()),
        _ => Err(ErrorCode::WrongType),
    }
}

fn take_required(object: &mut Vec<(String, JsonValue)>, key: &str) -> Result<JsonValue> {
    let index = object
        .iter()
        .position(|(candidate, _)| candidate == key)
        .ok_or(ErrorCode::MissingField)?;
    Ok(object.swap_remove(index).1)
}

fn peek_required_string(object: &[(String, JsonValue)], key: &str) -> Result<String> {
    object
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| match value {
            JsonValue::String(value) => {
                validate_ascii(value)?;
                Ok(value.clone())
            }
            _ => Err(ErrorCode::WrongType),
        })
        .ok_or(ErrorCode::MissingField)?
}

fn contains_key(object: &[(String, JsonValue)], key: &str) -> bool {
    object.iter().any(|(candidate, _)| candidate == key)
}

fn reject_unknown(object: Vec<(String, JsonValue)>) -> Result<()> {
    if object.is_empty() {
        Ok(())
    } else {
        Err(ErrorCode::UnknownField)
    }
}
